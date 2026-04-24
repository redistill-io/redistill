// AOF (Append-Only File) persistence.
//
// Writes every successful write-command to a log file in RESP format so the
// store can be rebuilt on startup. Orthogonal to the RDB snapshot path —
// users can run RDB-only, AOF-only, both, or neither.
//
// Design notes:
// * A single `Mutex<BufWriter<File>>` serializes writes. Under the write
//   workloads we target, mutex contention is dwarfed by the syscall + fsync
//   cost. If we ever see the mutex on a flame graph we can swap to an mpsc
//   channel + dedicated writer task without changing the public API.
// * `sync_data` (fdatasync) is used instead of `sync_all` — we only care
//   about data durability, not metadata.
// * Replay reuses the main command dispatch path: `CommandReader` yields
//   parsed `Vec<Bytes>` frames, main.rs feeds them to `execute_command`
//   with a scratch writer and an authenticated connection state.

use bytes::Bytes;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use crate::store::{EntryValue, ShardedStore, get_timestamp};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsyncPolicy {
    /// fsync on every write command. Slowest, safest — zero data loss on crash.
    Always,
    /// fsync in a background task every second. Default. ≤1s of data loss on crash.
    EverySec,
    /// Never fsync explicitly; rely on OS page-cache flushes. Data loss bounded
    /// by kernel's dirty_expire_centisecs (typically 30s on Linux).
    No,
}

impl FsyncPolicy {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "always" => Ok(Self::Always),
            "everysec" => Ok(Self::EverySec),
            "no" => Ok(Self::No),
            other => Err(format!(
                "invalid aof_fsync policy: '{}' (expected always|everysec|no)",
                other
            )),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::EverySec => "everysec",
            Self::No => "no",
        }
    }
}

pub struct Aof {
    inner: Mutex<BufWriter<File>>,
    fsync: FsyncPolicy,
    /// Set by `append` under EverySec/No, cleared by `sync`.
    dirty: AtomicBool,
    /// Path to the active AOF file. Rewrite replaces the file at this path.
    path: PathBuf,
    /// Bytes of live state we have written since the last rewrite (or open).
    /// Updated by `append` and `sync`; reset by `rewrite`.
    size_bytes: AtomicU64,
    /// Size we wrote during the most recent rewrite. Used to trigger the next
    /// one when the log has grown `aof_rewrite_percentage` past this baseline.
    last_rewrite_size: AtomicU64,
    /// Guard against two rewrites running at once.
    rewrite_in_progress: AtomicBool,
}

/// Stats returned by `rewrite` for logging.
#[derive(Debug, Clone, Copy)]
pub struct RewriteStats {
    pub keys_written: u64,
    pub bytes_written: u64,
    pub previous_bytes: u64,
}

impl Aof {
    /// Open (create if missing) an AOF file in append mode.
    pub fn open(path: &Path, fsync: FsyncPolicy) -> io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let initial_size = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            inner: Mutex::new(BufWriter::with_capacity(64 * 1024, file)),
            fsync,
            dirty: AtomicBool::new(false),
            path: path.to_path_buf(),
            size_bytes: AtomicU64::new(initial_size),
            last_rewrite_size: AtomicU64::new(initial_size),
            rewrite_in_progress: AtomicBool::new(false),
        })
    }

    /// Append a single command (as a RESP array) to the AOF. Called from the
    /// hot path after a write command has been applied to the store.
    ///
    /// Under `Always`, this flushes and fsyncs inline — the caller's ack
    /// effectively blocks on disk. Under `EverySec` and `No`, this only
    /// buffers; durability is advanced by the background task or the OS.
    pub fn append(&self, command: &[Bytes]) -> io::Result<()> {
        let mut w = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let written = write_resp_array(&mut *w, command)?;
        match self.fsync {
            FsyncPolicy::Always => {
                w.flush()?;
                w.get_ref().sync_data()?;
            }
            FsyncPolicy::EverySec | FsyncPolicy::No => {
                self.dirty.store(true, Ordering::Relaxed);
            }
        }
        drop(w);
        self.size_bytes.fetch_add(written as u64, Ordering::Relaxed);
        Ok(())
    }

    /// Flush buffered data and fsync. Called by the `everysec` timer and on
    /// graceful shutdown.
    pub fn sync(&self) -> io::Result<()> {
        let mut w = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        w.flush()?;
        w.get_ref().sync_data()?;
        self.dirty.store(false, Ordering::Relaxed);
        Ok(())
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Relaxed)
    }

    pub fn fsync_policy(&self) -> FsyncPolicy {
        self.fsync
    }

    pub fn size_bytes(&self) -> u64 {
        self.size_bytes.load(Ordering::Relaxed)
    }

    pub fn last_rewrite_size(&self) -> u64 {
        self.last_rewrite_size.load(Ordering::Relaxed)
    }

    pub fn is_rewriting(&self) -> bool {
        self.rewrite_in_progress.load(Ordering::Relaxed)
    }

    /// Decide whether the log has grown enough that a rewrite is worth doing.
    /// Rewrite iff `size >= min_size AND size >= last_rewrite × (1 + pct/100)`.
    pub fn should_rewrite(&self, min_size_bytes: u64, percentage: u64) -> bool {
        let size = self.size_bytes();
        if size < min_size_bytes {
            return false;
        }
        let base = self.last_rewrite_size().max(min_size_bytes);
        let trigger = base.saturating_add(base.saturating_mul(percentage) / 100);
        size >= trigger
    }

    /// Stop-the-world rewrite: serialize the live store to a new AOF,
    /// atomically rename over the current file, and swap our active file
    /// descriptor. Blocks all hot-path writers for the duration — acceptable
    /// for a first implementation; per-shard concurrent rewrite is a future
    /// optimization.
    ///
    /// Returns `Ok(None)` if another rewrite is already running. Only one
    /// rewrite can be in flight at a time.
    pub fn rewrite(&self, store: &ShardedStore) -> io::Result<Option<RewriteStats>> {
        // Claim exclusive rewrite rights.
        if self
            .rewrite_in_progress
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Ok(None);
        }

        let result = self.rewrite_inner(store);
        self.rewrite_in_progress.store(false, Ordering::SeqCst);
        result.map(Some)
    }

    fn rewrite_inner(&self, store: &ShardedStore) -> io::Result<RewriteStats> {
        let previous_bytes = self.size_bytes();

        // Write to a temp path alongside the real file so the atomic rename
        // lands on the same filesystem.
        let temp_path = {
            let mut p = self.path.clone();
            let fname = p.file_name().map(|s| s.to_os_string()).unwrap_or_default();
            let mut name = std::ffi::OsString::from(".rewrite.");
            name.push(&fname);
            p.set_file_name(name);
            p
        };

        // Produce the new file. We build it fully and fsync before acquiring
        // the live file lock, so the stop-the-world window is just the
        // rename + reopen (microseconds), not the full serialization.
        let now = get_timestamp();
        let (keys_written, bytes_written) = write_store_snapshot(&temp_path, store, now)?;

        // Now take the live file lock for the swap. We flush the existing
        // writer so any buffered bytes that haven't reached the kernel yet
        // land on the old file — they'll be included in the next rewrite.
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.flush()?;

        // NOTE: A write that committed between our snapshot walk finishing and
        // this lock being acquired is NOT in the new file and will be lost on
        // the next restart. This is the "stop-the-world" cost. Parallel
        // rewrite (capture diff during snapshot) is a future PR.
        // Guard against this in practice by triggering rewrite only when
        // load is low enough that the walk completes quickly.

        std::fs::rename(&temp_path, &self.path)?;

        // Re-open the path in append mode. The old descriptor (inside the old
        // BufWriter) is now pointing at an unlinked inode and will be dropped.
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        *guard = BufWriter::with_capacity(64 * 1024, file);

        self.size_bytes.store(bytes_written, Ordering::Relaxed);
        self.last_rewrite_size
            .store(bytes_written, Ordering::Relaxed);
        self.dirty.store(false, Ordering::Relaxed);

        Ok(RewriteStats {
            keys_written,
            bytes_written,
            previous_bytes,
        })
    }
}

/// Write a RESP array framing of `command`. Returns the number of bytes
/// written so callers can track file growth cheaply.
fn write_resp_array<W: Write>(w: &mut W, command: &[Bytes]) -> io::Result<usize> {
    // *N\r\n  $len\r\n<bytes>\r\n  $len\r\n<bytes>\r\n ...
    let mut total = 0;
    let header = format!("*{}\r\n", command.len());
    w.write_all(header.as_bytes())?;
    total += header.len();
    for part in command {
        let len_hdr = format!("${}\r\n", part.len());
        w.write_all(len_hdr.as_bytes())?;
        total += len_hdr.len();
        w.write_all(part)?;
        total += part.len();
        w.write_all(b"\r\n")?;
        total += 2;
    }
    Ok(total)
}

// ==================== Rewrite Snapshot ====================

/// Serialize the current live state of `store` to `temp_path` as an AOF.
/// Writes one `SET`/`HSET` per live key (plus an `EXPIRE` when the key has a
/// future TTL), fsyncs, and returns `(keys_written, bytes_written)`. Expired
/// keys are skipped.
fn write_store_snapshot(
    temp_path: &Path,
    store: &ShardedStore,
    now: u64,
) -> io::Result<(u64, u64)> {
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(temp_path)?;
    let mut w = BufWriter::with_capacity(256 * 1024, file);
    let mut keys: u64 = 0;
    let mut bytes: u64 = 0;

    for shard in &store.shards {
        // Iterating a DashMap holds per-bucket locks briefly; writers to
        // other buckets proceed. This is the source of the stop-the-world
        // relaxation limitation noted in `rewrite_inner`.
        for entry in shard.iter() {
            let (key, val) = entry.pair();

            // Skip expired keys — they're logically gone.
            if let Some(exp) = val.expiry
                && now >= exp
            {
                continue;
            }

            match &val.value {
                EntryValue::String(v) => {
                    let n = emit_set(&mut w, key, v)?;
                    bytes += n as u64;
                }
                EntryValue::Hash(h) => {
                    // Clone field/value pairs under the hash's read lock so
                    // we can drop the lock before blocking on file I/O.
                    let pairs: Vec<(Bytes, Bytes)> = {
                        let guard = h.read().unwrap();
                        guard.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
                    };
                    if pairs.is_empty() {
                        continue;
                    }
                    let n = emit_hset(&mut w, key, &pairs)?;
                    bytes += n as u64;
                }
            }

            // Encode remaining TTL (in seconds) as a separate EXPIRE so we
            // don't need to teach SET about PEXPIREAT. Key's `expiry` is an
            // absolute unix-seconds value; convert to remaining seconds.
            if let Some(exp) = val.expiry {
                let remaining = exp.saturating_sub(now);
                if remaining > 0 {
                    let n = emit_expire(&mut w, key, remaining)?;
                    bytes += n as u64;
                }
            }

            keys += 1;
        }
    }

    w.flush()?;
    w.get_ref().sync_data()?;
    drop(w);
    Ok((keys, bytes))
}

fn emit_set<W: Write>(w: &mut W, key: &Bytes, value: &Bytes) -> io::Result<usize> {
    let cmd: [&[u8]; 3] = [b"SET", key.as_ref(), value.as_ref()];
    write_resp_array_raw(w, &cmd)
}

fn emit_hset<W: Write>(w: &mut W, key: &Bytes, pairs: &[(Bytes, Bytes)]) -> io::Result<usize> {
    // HSET key f1 v1 f2 v2 ...
    let mut parts: Vec<&[u8]> = Vec::with_capacity(2 + pairs.len() * 2);
    parts.push(b"HSET");
    parts.push(key.as_ref());
    for (f, v) in pairs {
        parts.push(f.as_ref());
        parts.push(v.as_ref());
    }
    write_resp_array_raw(w, &parts)
}

fn emit_expire<W: Write>(w: &mut W, key: &Bytes, ttl_secs: u64) -> io::Result<usize> {
    let ttl_str = ttl_secs.to_string();
    let cmd: [&[u8]; 3] = [b"EXPIRE", key.as_ref(), ttl_str.as_bytes()];
    write_resp_array_raw(w, &cmd)
}

/// Slice-flavored variant of `write_resp_array`: avoids forcing callers to
/// wrap every arg in a `Bytes`.
fn write_resp_array_raw<W: Write>(w: &mut W, parts: &[&[u8]]) -> io::Result<usize> {
    let mut total = 0;
    let header = format!("*{}\r\n", parts.len());
    w.write_all(header.as_bytes())?;
    total += header.len();
    for part in parts {
        let len_hdr = format!("${}\r\n", part.len());
        w.write_all(len_hdr.as_bytes())?;
        total += len_hdr.len();
        w.write_all(part)?;
        total += part.len();
        w.write_all(b"\r\n")?;
        total += 2;
    }
    Ok(total)
}

// ==================== Replay ====================

/// Reader that yields one parsed RESP command per call. Used only at startup.
pub struct CommandReader {
    reader: BufReader<File>,
}

impl CommandReader {
    /// Open the AOF for replay. Returns `Ok(None)` if the file does not exist.
    pub fn open(path: &Path) -> io::Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let file = File::open(path)?;
        Ok(Some(Self {
            reader: BufReader::with_capacity(64 * 1024, file),
        }))
    }

    /// Returns `Ok(None)` on clean EOF, `Ok(Some(cmd))` on a parsed command,
    /// `Err` on malformed input (treat as a hard startup failure).
    pub fn next_command(&mut self) -> io::Result<Option<Vec<Bytes>>> {
        let mut line = Vec::new();
        let n = self.reader.read_until(b'\n', &mut line)?;
        if n == 0 {
            return Ok(None);
        }
        if !line.ends_with(b"\r\n") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "aof: expected CRLF after header",
            ));
        }
        if line[0] != b'*' {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("aof: expected RESP array (*), got {:?}", line[0] as char),
            ));
        }
        let count: usize = parse_num(&line[1..line.len() - 2])?;
        let mut parts = Vec::with_capacity(count);
        for _ in 0..count {
            line.clear();
            let hn = self.reader.read_until(b'\n', &mut line)?;
            if hn == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "aof: unexpected EOF mid-array",
                ));
            }
            if !line.ends_with(b"\r\n") || line[0] != b'$' {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "aof: expected bulk-string header",
                ));
            }
            let blen: usize = parse_num(&line[1..line.len() - 2])?;
            let mut buf = vec![0u8; blen];
            self.reader.read_exact(&mut buf)?;
            let mut crlf = [0u8; 2];
            self.reader.read_exact(&mut crlf)?;
            if crlf != *b"\r\n" {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "aof: expected CRLF after bulk",
                ));
            }
            parts.push(Bytes::from(buf));
        }
        Ok(Some(parts))
    }
}

fn parse_num(bytes: &[u8]) -> io::Result<usize> {
    std::str::from_utf8(bytes)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
        .parse()
        .map_err(|e: std::num::ParseIntError| io::Error::new(io::ErrorKind::InvalidData, e))
}

// ==================== Background Sync Task ====================

/// Polls the AOF size every 10 seconds and kicks off an async rewrite when
/// the thresholds in config are crossed. No-op when `percentage == 0`.
pub async fn rewrite_supervisor_task(
    aof: Arc<Aof>,
    store: ShardedStore,
    min_size: u64,
    percentage: u64,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) {
    if percentage == 0 {
        return;
    }
    let mut interval = tokio::time::interval(Duration::from_secs(10));
    interval.tick().await; // skip the first immediate tick
    loop {
        tokio::select! {
            biased;
            _ = shutdown_rx.changed() => break,
            _ = interval.tick() => {
                if aof.should_rewrite(min_size, percentage) && !aof.is_rewriting() {
                    let aof_c = aof.clone();
                    let store_c = store.clone();
                    tokio::task::spawn_blocking(move || {
                        match aof_c.rewrite(&store_c) {
                            Ok(Some(stats)) => {
                                eprintln!(
                                    "AOF auto-rewrite: {} keys, {} -> {} bytes",
                                    stats.keys_written, stats.previous_bytes, stats.bytes_written
                                );
                            }
                            Ok(None) => {} // another rewrite got there first
                            Err(e) => eprintln!("AOF auto-rewrite failed: {}", e),
                        }
                    });
                }
            }
        }
    }
}

/// Fsyncs the AOF once per second while it is dirty. Only spawned when
/// `aof_fsync = "everysec"`. Exits on shutdown.
pub async fn everysec_task(aof: Arc<Aof>, mut shutdown_rx: tokio::sync::watch::Receiver<bool>) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.tick().await; // skip first immediate tick
    loop {
        tokio::select! {
            biased;
            _ = shutdown_rx.changed() => break,
            _ = interval.tick() => {
                if aof.is_dirty() {
                    let a = aof.clone();
                    let res = tokio::task::spawn_blocking(move || a.sync()).await;
                    if let Ok(Err(e)) = res {
                        eprintln!("AOF everysec fsync failed: {}", e);
                    }
                }
            }
        }
    }
    // One last sync so any buffered writes after the final tick still land.
    let final_sync = tokio::task::spawn_blocking(move || aof.sync()).await;
    if let Ok(Err(e)) = final_sync {
        eprintln!("AOF shutdown sync failed: {}", e);
    }
}
