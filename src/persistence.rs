// Persistence module - Snapshot save/load
// Cold path - only runs on startup, shutdown, and periodic saves

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use crate::config::CONFIG;
use crate::store::{
    Entry, EntryValue, MEMORY_USED, ShardedStore, calculate_entry_size, get_timestamp,
};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

// ==================== Snapshot Constants ====================

const SNAPSHOT_MAGIC: &[u8; 4] = b"RDST";
const SNAPSHOT_VERSION: u8 = 1;

// ==================== Global State ====================

pub static SAVE_IN_PROGRESS: AtomicBool = AtomicBool::new(false);
pub static LAST_SAVE_TIME: AtomicU64 = AtomicU64::new(0);

// ==================== Snapshot Entry ====================

#[derive(Serialize, Deserialize)]
enum SnapshotValue {
    String(Vec<u8>),
    Hash(Vec<(Vec<u8>, Vec<u8>)>), // Vec of (field, value) pairs
}

#[derive(Serialize, Deserialize)]
struct SnapshotEntry {
    key: Vec<u8>,
    value: SnapshotValue,
    expiry: Option<u64>,
}

// Backward compatibility: old format without type discriminator
#[derive(Serialize, Deserialize)]
struct LegacySnapshotEntry {
    key: Vec<u8>,
    value: Vec<u8>,
    expiry: Option<u64>,
}

// ==================== Snapshot Helpers ====================

/// Build a `SnapshotEntry` for `key` if the key still exists and is not expired.
fn build_snapshot_entry(store: &ShardedStore, key: &Bytes, now: u64) -> Option<SnapshotEntry> {
    let shard = &store.shards[store.hash(key)];
    let entry = shard.get(key.as_ref())?;

    // Skip expired entries
    if let Some(expiry) = entry.expiry
        && now >= expiry
    {
        return None;
    }

    let snapshot_value = match &entry.value {
        EntryValue::String(bytes) => SnapshotValue::String(bytes.to_vec()),
        EntryValue::Hash(hash_map) => {
            let map_guard = hash_map.read().unwrap();
            let mut fields = Vec::with_capacity(map_guard.len());
            for (k, v) in map_guard.iter() {
                fields.push((k.to_vec(), v.to_vec()));
            }
            SnapshotValue::Hash(fields)
        }
    };

    Some(SnapshotEntry {
        key: key.to_vec(),
        value: snapshot_value,
        expiry: entry.expiry,
    })
}

/// Write the full snapshot to any `Write` sink.
///
/// The header entry-count field is written up-front using the live key count;
/// entries that expire between `keys()` and serialization are silently skipped,
/// so the reader may see fewer entries than the header count (it breaks early on
/// a short read of the length prefix, which is fine because `entry_count` in the
/// header is an upper bound, not an exact count).
fn write_snapshot<W: std::io::Write>(w: &mut W, store: &ShardedStore) -> Result<usize, String> {
    let now = get_timestamp();

    // Snapshot the live keys once; expired keys that sneak in after this point
    // will be skipped per-entry via build_snapshot_entry.
    let keys = store.keys(now);
    let key_count = keys.len() as u64;

    // Header: magic | version | timestamp | entry_count
    w.write_all(SNAPSHOT_MAGIC)
        .map_err(|e| format!("Failed to write header: {}", e))?;
    w.write_all(&[SNAPSHOT_VERSION])
        .map_err(|e| format!("Failed to write version: {}", e))?;
    w.write_all(&now.to_le_bytes())
        .map_err(|e| format!("Failed to write timestamp: {}", e))?;
    w.write_all(&key_count.to_le_bytes())
        .map_err(|e| format!("Failed to write entry count: {}", e))?;

    let mut count: usize = 0;
    for key in &keys {
        let Some(snapshot_entry) = build_snapshot_entry(store, key, now) else {
            continue;
        };

        let encoded = bincode::serialize(&snapshot_entry)
            .map_err(|e| format!("Failed to serialize entry: {}", e))?;

        // Length-prefixed entry (u32 LE)
        w.write_all(&(encoded.len() as u32).to_le_bytes())
            .map_err(|e| format!("Failed to write entry length: {}", e))?;
        w.write_all(&encoded)
            .map_err(|e| format!("Failed to write entry data: {}", e))?;

        count += 1;
    }

    w.flush().map_err(|e| format!("Failed to flush: {}", e))?;
    Ok(count)
}

/// Deserialize one entry blob and insert it into `store`.
fn apply_snapshot_entry(store: &ShardedStore, entry_data: &[u8], now: u64) -> Result<(), String> {
    use std::sync::atomic::{AtomicU8, AtomicU32};

    // Try new format first, fall back to legacy.
    let (key, entry_value, expiry) = match bincode::deserialize::<SnapshotEntry>(entry_data) {
        Ok(snapshot_entry) => {
            let expiry = snapshot_entry.expiry;
            let value = match snapshot_entry.value {
                SnapshotValue::String(bytes) => EntryValue::String(Bytes::from(bytes)),
                SnapshotValue::Hash(fields) => {
                    let mut map = HashMap::new();
                    for (field, val) in fields {
                        map.insert(Bytes::from(field), Bytes::from(val));
                    }
                    EntryValue::Hash(Arc::new(RwLock::new(map)))
                }
            };
            (Bytes::from(snapshot_entry.key), value, expiry)
        }
        Err(_) => {
            // Legacy format (backward compatibility)
            let legacy_entry: LegacySnapshotEntry = bincode::deserialize(entry_data)
                .map_err(|e| format!("Failed to deserialize entry: {}", e))?;
            (
                Bytes::from(legacy_entry.key),
                EntryValue::String(Bytes::from(legacy_entry.value)),
                legacy_entry.expiry,
            )
        }
    };

    // Skip expired entries
    if let Some(exp) = expiry
        && now >= exp
    {
        return Ok(());
    }

    // Calculate remaining TTL
    let ttl = expiry.and_then(|exp| if exp > now { Some(exp - now) } else { None });

    // Track memory
    if CONFIG.memory.max_memory > 0 {
        let size = calculate_entry_size(key.len(), &entry_value);
        MEMORY_USED.fetch_add(size as u64, Ordering::Relaxed);
    }

    let shard_idx = store.hash(&key);
    let shard = &store.shards[shard_idx];
    shard.insert(
        key,
        Entry {
            value: entry_value,
            expiry: ttl.map(|t| now + t),
            last_accessed: AtomicU32::new(0),
            queue_type: AtomicU8::new(0),
            access_count: AtomicU8::new(0),
        },
    );

    Ok(())
}

/// Read a snapshot from any `Read` source and populate `store`.
///
/// Returns the number of entries successfully loaded (expired entries are skipped
/// and do not count).
fn read_snapshot<R: std::io::Read>(r: &mut R, store: &ShardedStore) -> Result<usize, String> {
    // Read and verify header
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic)
        .map_err(|e| format!("Failed to read magic: {}", e))?;
    if &magic != SNAPSHOT_MAGIC {
        return Err("Invalid snapshot file (bad magic)".to_string());
    }

    let mut version = [0u8; 1];
    r.read_exact(&mut version)
        .map_err(|e| format!("Failed to read version: {}", e))?;
    if version[0] != SNAPSHOT_VERSION {
        return Err(format!("Unsupported snapshot version: {}", version[0]));
    }

    let mut timestamp_bytes = [0u8; 8];
    r.read_exact(&mut timestamp_bytes)
        .map_err(|e| format!("Failed to read timestamp: {}", e))?;
    let _snapshot_time = u64::from_le_bytes(timestamp_bytes);

    let mut count_bytes = [0u8; 8];
    r.read_exact(&mut count_bytes)
        .map_err(|e| format!("Failed to read entry count: {}", e))?;
    let entry_count = u64::from_le_bytes(count_bytes);

    let now = get_timestamp();
    let mut loaded: usize = 0;

    for _ in 0..entry_count {
        let mut len_bytes = [0u8; 4];
        if r.read_exact(&mut len_bytes).is_err() {
            break;
        }
        let entry_len = u32::from_le_bytes(len_bytes) as usize;

        let mut entry_data = vec![0u8; entry_len];
        r.read_exact(&mut entry_data)
            .map_err(|e| format!("Failed to read entry data: {}", e))?;

        apply_snapshot_entry(store, &entry_data, now)?;
        loaded += 1;
    }

    Ok(loaded)
}

// ==================== Save Snapshot ====================

pub fn save_snapshot_sync(store: &ShardedStore, path: &str) -> Result<usize, String> {
    use std::io::BufWriter;

    if SAVE_IN_PROGRESS.swap(true, Ordering::SeqCst) {
        return Err("Background save already in progress".to_string());
    }

    let result = (|| {
        let temp_path = format!("{}.tmp", path);

        let file = std::fs::File::create(&temp_path)
            .map_err(|e| format!("Failed to create snapshot file: {}", e))?;
        let mut writer = BufWriter::with_capacity(64 * 1024, file);

        let count = write_snapshot(&mut writer, store)?;

        drop(writer);

        // Atomic rename
        std::fs::rename(&temp_path, path)
            .map_err(|e| format!("Failed to rename snapshot file: {}", e))?;

        LAST_SAVE_TIME.store(get_timestamp(), Ordering::Relaxed);

        Ok(count)
    })();

    SAVE_IN_PROGRESS.store(false, Ordering::SeqCst);
    result
}

/// Serialize the entire store to an in-memory byte buffer using the standard
/// RDST snapshot format.  The buffer can be shipped over a socket and loaded
/// with [`load_store_from_bytes`].
#[allow(dead_code)]
pub fn dump_store_to_bytes(store: &ShardedStore) -> Result<Vec<u8>, String> {
    let mut buf = std::io::Cursor::new(Vec::with_capacity(64 * 1024));
    let _count = write_snapshot(&mut buf, store)?;
    Ok(buf.into_inner())
}

// ==================== Load Snapshot ====================

pub fn load_snapshot(store: &ShardedStore, path: &str) -> Result<usize, String> {
    use std::io::BufReader;

    if !std::path::Path::new(path).exists() {
        return Ok(0);
    }

    let file = std::fs::File::open(path).map_err(|e| format!("Failed to open snapshot: {}", e))?;
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    read_snapshot(&mut reader, store)
}

/// Load a store from a byte slice produced by [`dump_store_to_bytes`].
#[allow(dead_code)]
pub fn load_store_from_bytes(store: &ShardedStore, data: &[u8]) -> Result<usize, String> {
    // Validate the whole payload before touching the live store. A failed
    // full sync must never leave a prefix of the master's keys installed.
    validate_snapshot_bytes(data, false)?;
    let mut cur = std::io::Cursor::new(data);
    read_snapshot(&mut cur, store)
}

/// Install a validated full-sync image. Callers must exclude client commands
/// while this replaces the store and its memory/eviction accounting.
pub fn replace_store_from_bytes(store: &ShardedStore, data: &[u8]) -> Result<usize, String> {
    validate_snapshot_bytes(data, true)?;
    store.clear_for_replication();
    let mut cur = std::io::Cursor::new(data);
    let count = read_snapshot(&mut cur, store)?;
    store.rebuild_eviction_queues();
    Ok(count)
}

fn validate_snapshot_bytes(data: &[u8], exact_count: bool) -> Result<(), String> {
    const HEADER_LEN: usize = 4 + 1 + 8 + 8;
    if data.len() < HEADER_LEN || &data[..4] != SNAPSHOT_MAGIC {
        return Err("Invalid snapshot payload (bad header)".to_string());
    }
    if data[4] != SNAPSHOT_VERSION {
        return Err(format!("Unsupported snapshot version: {}", data[4]));
    }

    let entry_count = u64::from_le_bytes(data[13..21].try_into().unwrap());
    let mut pos = HEADER_LEN;
    for _ in 0..entry_count {
        // The header count is an upper bound: an entry may have expired after
        // the key list was captured. A full-sync snapshot is captured under
        // the barrier, so its count must be exact.
        if pos == data.len() {
            return if exact_count {
                Err("Truncated full-sync snapshot".to_string())
            } else {
                Ok(())
            };
        }
        let end_prefix = pos.checked_add(4).ok_or("Snapshot entry length overflow")?;
        let prefix = data
            .get(pos..end_prefix)
            .ok_or("Truncated snapshot entry length")?;
        let len = u32::from_le_bytes(prefix.try_into().unwrap()) as usize;
        pos = end_prefix;
        let end_entry = pos.checked_add(len).ok_or("Snapshot entry overflow")?;
        let entry = data.get(pos..end_entry).ok_or("Truncated snapshot entry")?;
        if bincode::deserialize::<SnapshotEntry>(entry).is_err() {
            bincode::deserialize::<LegacySnapshotEntry>(entry)
                .map_err(|e| format!("Invalid snapshot entry: {e}"))?;
        }
        pos = end_entry;
    }
    if pos != data.len() {
        return Err("Trailing bytes in snapshot payload".to_string());
    }
    Ok(())
}

// ==================== Background Snapshot Task ====================

pub async fn snapshot_task(
    store: ShardedStore,
    interval_secs: u64,
    path: String,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) {
    if interval_secs == 0 {
        return;
    }

    let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
    interval.tick().await; // Skip first immediate tick

    loop {
        tokio::select! {
            biased;
            _ = shutdown_rx.changed() => break,
            _ = interval.tick() => {}
        }

        let store_clone = store.clone();
        let path_clone = path.clone();

        tokio::task::spawn_blocking(
            move || match save_snapshot_sync(&store_clone, &path_clone) {
                Ok(count) => {
                    eprintln!(
                        "Background snapshot saved: {} keys to {}",
                        count, path_clone
                    );
                }
                Err(e) => {
                    eprintln!("Background snapshot failed: {}", e);
                }
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::ShardedStore;
    use bytes::Bytes;

    #[test]
    fn dump_and_load_roundtrip_strings_and_hashes() {
        let now = crate::store::get_timestamp();
        let src = ShardedStore::new(16);

        // 2 strings (one with TTL) + 1 hash field
        src.set(
            Bytes::from_static(b"k1"),
            Bytes::from_static(b"v1"),
            None,
            now,
        );
        src.set(
            Bytes::from_static(b"k2"),
            Bytes::from_static(b"v2"),
            Some(now + 100_000), // TTL well in the future (ms)
            now,
        );
        src.hset(
            Bytes::from_static(b"h1"),
            &[(Bytes::from_static(b"f1"), Bytes::from_static(b"hv1"))],
            now,
        )
        .unwrap();

        let bytes = dump_store_to_bytes(&src).expect("dump");

        let dst = ShardedStore::new(16);
        let count = load_store_from_bytes(&dst, &bytes).expect("load");

        assert_eq!(count, 3);

        let now2 = crate::store::get_timestamp();
        assert_eq!(dst.get(b"k1", now2), Some(Bytes::from_static(b"v1")));
        assert_eq!(dst.get(b"k2", now2), Some(Bytes::from_static(b"v2")));
        assert_eq!(
            dst.hget(b"h1", b"f1", now2).unwrap(),
            Some(Bytes::from_static(b"hv1"))
        );
    }

    #[test]
    fn malformed_replica_dump_does_not_partially_load() {
        let src = ShardedStore::new(2);
        let now = get_timestamp();
        src.set(
            Bytes::from_static(b"one"),
            Bytes::from_static(b"1"),
            None,
            now,
        );
        src.set(
            Bytes::from_static(b"two"),
            Bytes::from_static(b"2"),
            None,
            now,
        );
        let mut dump = dump_store_to_bytes(&src).unwrap();
        dump.truncate(dump.len() - 3);

        let dst = ShardedStore::new(2);
        dst.set(
            Bytes::from_static(b"keep"),
            Bytes::from_static(b"yes"),
            None,
            now,
        );
        assert!(load_store_from_bytes(&dst, &dump).is_err());
        assert_eq!(dst.len(), 1);
        assert_eq!(dst.get(b"keep", now), Some(Bytes::from_static(b"yes")));
    }

    #[test]
    fn full_sync_rejects_clean_prefix_of_snapshot() {
        let src = ShardedStore::new(2);
        let now = get_timestamp();
        src.set(
            Bytes::from_static(b"one"),
            Bytes::from_static(b"1"),
            None,
            now,
        );
        src.set(
            Bytes::from_static(b"two"),
            Bytes::from_static(b"2"),
            None,
            now,
        );
        let mut dump = dump_store_to_bytes(&src).unwrap();
        let first_len = u32::from_le_bytes(dump[21..25].try_into().unwrap()) as usize;
        dump.truncate(25 + first_len);

        let dst = ShardedStore::new(2);
        dst.set(
            Bytes::from_static(b"keep"),
            Bytes::from_static(b"yes"),
            None,
            now,
        );
        assert!(replace_store_from_bytes(&dst, &dump).is_err());
        assert_eq!(dst.len(), 1);
        assert_eq!(dst.get(b"keep", now), Some(Bytes::from_static(b"yes")));
    }
}
