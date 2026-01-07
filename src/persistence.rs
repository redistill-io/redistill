// Persistence module - Snapshot save/load
// Cold path - only runs on startup, shutdown, and periodic saves

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use crate::config::CONFIG;
use crate::store::{Entry, EntryValue, MEMORY_USED, ShardedStore, calculate_entry_size, get_timestamp};
use std::sync::Arc;
use dashmap::DashMap;

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

// ==================== Save Snapshot ====================

pub fn save_snapshot_sync(store: &ShardedStore, path: &str) -> Result<usize, String> {
    use std::io::{BufWriter, Seek, SeekFrom, Write};

    if SAVE_IN_PROGRESS.swap(true, Ordering::SeqCst) {
        return Err("Background save already in progress".to_string());
    }

    let result = (|| {
        let now = get_timestamp();
        let temp_path = format!("{}.tmp", path);

        let file = std::fs::File::create(&temp_path)
            .map_err(|e| format!("Failed to create snapshot file: {}", e))?;
        let mut writer = BufWriter::with_capacity(64 * 1024, file);

        // Write header
        writer
            .write_all(SNAPSHOT_MAGIC)
            .map_err(|e| format!("Failed to write header: {}", e))?;
        writer
            .write_all(&[SNAPSHOT_VERSION])
            .map_err(|e| format!("Failed to write version: {}", e))?;
        writer
            .write_all(&now.to_le_bytes())
            .map_err(|e| format!("Failed to write timestamp: {}", e))?;

        // Placeholder for entry count
        let entry_count_pos = writer
            .stream_position()
            .map_err(|e| format!("Failed to get position: {}", e))?;
        writer
            .write_all(&0u64.to_le_bytes())
            .map_err(|e| format!("Failed to write entry count: {}", e))?;

        // Stream entries from all shards
        let mut count: u64 = 0;
        for shard in &store.shards {
            for entry in shard.iter() {
                let (key, val) = entry.pair();

                // Skip expired entries
                if let Some(expiry) = val.expiry
                    && now >= expiry
                {
                    continue;
                }

                let snapshot_value = match &val.value {
                    EntryValue::String(bytes) => SnapshotValue::String(bytes.to_vec()),
                    EntryValue::Hash(hash_map) => {
                        let mut fields = Vec::new();
                        for entry in hash_map.iter() {
                            fields.push((entry.key().to_vec(), entry.value().to_vec()));
                        }
                        SnapshotValue::Hash(fields)
                    }
                };

                let snapshot_entry = SnapshotEntry {
                    key: key.to_vec(),
                    value: snapshot_value,
                    expiry: val.expiry,
                };

                let encoded = bincode::serialize(&snapshot_entry)
                    .map_err(|e| format!("Failed to serialize entry: {}", e))?;

                // Write length-prefixed entry
                writer
                    .write_all(&(encoded.len() as u32).to_le_bytes())
                    .map_err(|e| format!("Failed to write entry length: {}", e))?;
                writer
                    .write_all(&encoded)
                    .map_err(|e| format!("Failed to write entry data: {}", e))?;

                count += 1;
            }
        }

        // Go back and write actual count
        writer
            .seek(SeekFrom::Start(entry_count_pos))
            .map_err(|e| format!("Failed to seek: {}", e))?;
        writer
            .write_all(&count.to_le_bytes())
            .map_err(|e| format!("Failed to write entry count: {}", e))?;

        writer
            .flush()
            .map_err(|e| format!("Failed to flush: {}", e))?;
        drop(writer);

        // Atomic rename
        std::fs::rename(&temp_path, path)
            .map_err(|e| format!("Failed to rename snapshot file: {}", e))?;

        LAST_SAVE_TIME.store(now, Ordering::Relaxed);

        Ok(count as usize)
    })();

    SAVE_IN_PROGRESS.store(false, Ordering::SeqCst);
    result
}

// ==================== Load Snapshot ====================

pub fn load_snapshot(store: &ShardedStore, path: &str) -> Result<usize, String> {
    use std::io::{BufReader, Read};
    use std::sync::atomic::AtomicU32;

    if !std::path::Path::new(path).exists() {
        return Ok(0);
    }

    let file = std::fs::File::open(path).map_err(|e| format!("Failed to open snapshot: {}", e))?;
    let mut reader = BufReader::with_capacity(64 * 1024, file);

    // Read and verify header
    let mut magic = [0u8; 4];
    reader
        .read_exact(&mut magic)
        .map_err(|e| format!("Failed to read magic: {}", e))?;
    if &magic != SNAPSHOT_MAGIC {
        return Err("Invalid snapshot file (bad magic)".to_string());
    }

    let mut version = [0u8; 1];
    reader
        .read_exact(&mut version)
        .map_err(|e| format!("Failed to read version: {}", e))?;
    if version[0] != SNAPSHOT_VERSION {
        return Err(format!("Unsupported snapshot version: {}", version[0]));
    }

    let mut timestamp_bytes = [0u8; 8];
    reader
        .read_exact(&mut timestamp_bytes)
        .map_err(|e| format!("Failed to read timestamp: {}", e))?;
    let _snapshot_time = u64::from_le_bytes(timestamp_bytes);

    let mut count_bytes = [0u8; 8];
    reader
        .read_exact(&mut count_bytes)
        .map_err(|e| format!("Failed to read entry count: {}", e))?;
    let entry_count = u64::from_le_bytes(count_bytes);

    let now = get_timestamp();
    let mut loaded: usize = 0;
    let mut skipped_expired: usize = 0;

    // Read entries
    for _ in 0..entry_count {
        let mut len_bytes = [0u8; 4];
        if reader.read_exact(&mut len_bytes).is_err() {
            break;
        }
        let entry_len = u32::from_le_bytes(len_bytes) as usize;

        let mut entry_data = vec![0u8; entry_len];
        reader
            .read_exact(&mut entry_data)
            .map_err(|e| format!("Failed to read entry data: {}", e))?;

        // Try to deserialize as new format first, fall back to legacy format
        let (key, entry_value, expiry) = match bincode::deserialize::<SnapshotEntry>(&entry_data) {
            Ok(snapshot_entry) => {
                // New format with type discriminator
                let expiry = snapshot_entry.expiry;
                let value = match snapshot_entry.value {
                    SnapshotValue::String(bytes) => EntryValue::String(Bytes::from(bytes)),
                    SnapshotValue::Hash(fields) => {
                        let hash_map = Arc::new(DashMap::new());
                        for (field, val) in fields {
                            hash_map.insert(Bytes::from(field), Bytes::from(val));
                        }
                        EntryValue::Hash(hash_map)
                    }
                };
                (Bytes::from(snapshot_entry.key), value, expiry)
            }
            Err(_) => {
                // Legacy format (backward compatibility)
                let legacy_entry: LegacySnapshotEntry = bincode::deserialize(&entry_data)
                    .map_err(|e| format!("Failed to deserialize entry: {}", e))?;
                (
                    Bytes::from(legacy_entry.key),
                    EntryValue::String(Bytes::from(legacy_entry.value)),
                    legacy_entry.expiry,
                )
            }
        };

        // Skip expired entries
        if let Some(exp) = expiry && now >= exp {
            skipped_expired += 1;
            continue;
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
            },
        );

        loaded += 1;
    }

    if skipped_expired > 0 {
        eprintln!("(skipped {} expired keys during load)", skipped_expired);
    }

    Ok(loaded)
}

// ==================== Background Snapshot Task ====================

pub async fn snapshot_task(store: ShardedStore, interval_secs: u64, path: String) {
    if interval_secs == 0 {
        return;
    }

    let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
    interval.tick().await; // Skip first immediate tick

    loop {
        interval.tick().await;

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
