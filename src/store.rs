// Storage module - HOT PATH
// Contains all storage-related code: Entry, ShardedStore, buffer pool, eviction
// All functions in hot paths are marked #[inline(always)]

use ahash::AHasher;
use bytes::Bytes;
use crossbeam::queue::SegQueue;
use dashmap::DashMap;
use once_cell::sync::Lazy;
use std::hash::Hasher;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::{CONFIG, EvictionPolicy};

// ==================== Global Statistics ====================

pub static TOTAL_CONNECTIONS: AtomicU64 = AtomicU64::new(0);
pub static ACTIVE_CONNECTIONS: AtomicUsize = AtomicUsize::new(0);
pub static TOTAL_COMMANDS: AtomicU64 = AtomicU64::new(0);
pub static MEMORY_USED: AtomicU64 = AtomicU64::new(0);
pub static EVICTED_KEYS: AtomicU64 = AtomicU64::new(0);
pub static REJECTED_CONNECTIONS: AtomicU64 = AtomicU64::new(0);

// Server start time for LRU (seconds since epoch, stored once)
pub static SERVER_START_TIME: Lazy<u64> = Lazy::new(get_timestamp);

// ==================== Buffer Pool ====================

static BUFFER_POOL: Lazy<SegQueue<Vec<u8>>> = Lazy::new(|| {
    let pool = SegQueue::new();
    for _ in 0..CONFIG.server.buffer_pool_size {
        pool.push(Vec::with_capacity(CONFIG.server.buffer_size));
    }
    pool
});

#[inline(always)]
pub fn get_buffer() -> Vec<u8> {
    BUFFER_POOL
        .pop()
        .unwrap_or_else(|| Vec::with_capacity(CONFIG.server.buffer_size))
}

#[inline(always)]
pub fn return_buffer(mut buf: Vec<u8>) {
    buf.clear();
    // Return buffers that are reasonably sized, but don't keep extremely large ones
    // This prevents pool draining while avoiding unbounded memory growth
    if buf.capacity() <= CONFIG.server.buffer_size * 2 {
        BUFFER_POOL.push(buf);
    } else if buf.capacity() <= CONFIG.server.buffer_size * 4 && BUFFER_POOL.len() < CONFIG.server.buffer_pool_size / 4 {
        // Keep some larger buffers if pool is getting low, but cap at 25% of pool size
        BUFFER_POOL.push(buf);
    }
    // Otherwise drop the buffer (too large or pool is full enough)
}

// ==================== Entry ====================

pub struct Entry {
    pub value: Bytes,
    pub expiry: Option<u64>,
    pub last_accessed: AtomicU32,
}

impl Clone for Entry {
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
            expiry: self.expiry,
            last_accessed: AtomicU32::new(self.last_accessed.load(Ordering::Relaxed)),
        }
    }
}

// ==================== Sharded Store ====================

pub struct ShardedStore {
    pub shards: Vec<Arc<DashMap<Bytes, Entry>>>,
    pub num_shards: usize,
}

impl Clone for ShardedStore {
    fn clone(&self) -> Self {
        Self {
            shards: self.shards.clone(),
            num_shards: self.num_shards,
        }
    }
}

impl ShardedStore {
    pub fn new(num_shards: usize) -> Self {
        let mut shards = Vec::with_capacity(num_shards);
        for _ in 0..num_shards {
            shards.push(Arc::new(DashMap::with_capacity(1000)));
        }
        Self { shards, num_shards }
    }

    #[inline(always)]
    pub fn hash(&self, key: &[u8]) -> usize {
        let mut hasher = AHasher::default();
        hasher.write(key);
        hasher.finish() as usize % self.num_shards
    }

    #[inline(always)]
    pub fn set(&self, key: Bytes, value: Bytes, ttl: Option<u64>, now: u64) -> Option<usize> {
        let expiry = ttl.map(|s| now + s);
        let key_len = key.len();
        let shard = &self.shards[self.hash(&key)];

        let timestamp = if fastrand::u32(..).is_multiple_of(10) {
            get_uptime_seconds()
        } else {
            0
        };

        let old_entry = shard.insert(
            key,
            Entry {
                value,
                expiry,
                last_accessed: AtomicU32::new(timestamp),
            },
        );

        old_entry.map(|e| entry_size(key_len, e.value.len()))
    }

    #[inline(always)]
    pub fn get_existing_size(&self, key: &[u8], now: u64) -> Option<usize> {
        let shard = &self.shards[self.hash(key)];
        if let Some(entry) = shard.get(key) {
            // Check if expired
            if let Some(expiry) = entry.expiry && now >= expiry {
                return None;
            }
            return Some(entry_size(key.len(), entry.value.len()));
        }
        None
    }

    #[inline(always)]
    pub fn get(&self, key: &[u8], now: u64) -> Option<Bytes> {
        let shard = &self.shards[self.hash(key)];

        if let Some(entry) = shard.get(key) {
            if let Some(expiry) = entry.expiry
                && now >= expiry
            {
                // Extract info before dropping to avoid race condition
                let key_bytes = Bytes::copy_from_slice(key);
                let key_len = key_bytes.len();
                let value_len = entry.value.len();
                drop(entry);

                // Atomic remove - only one thread can succeed
                if shard.remove(key_bytes.as_ref()).is_some() && CONFIG.memory.max_memory > 0 {
                    let size = entry_size(key_len, value_len);
                    MEMORY_USED.fetch_sub(size as u64, Ordering::Relaxed);
                }
                return None;
            }

            maybe_update_access_time(&entry);
            return Some(entry.value.clone());
        }
        None
    }

    #[inline(always)]
    pub fn delete(&self, keys: &[Bytes], now: u64) -> usize {
        let mut count = 0;
        for key in keys {
            let shard = &self.shards[self.hash(key)];
            if let Some((_, entry)) = shard.remove(key) {
                // Always decrement memory if key was removed, regardless of expiry
                // (expired keys still consume memory until removed)
                if CONFIG.memory.max_memory > 0 {
                    let size = entry_size(key.len(), entry.value.len());
                    MEMORY_USED.fetch_sub(size as u64, Ordering::Relaxed);
                }
                // Only count as deleted if not expired (Redis semantics)
                if entry.expiry.is_none_or(|exp| now < exp) {
                    count += 1;
                }
            }
        }
        count
    }

    #[inline(always)]
    pub fn exists(&self, keys: &[Bytes], now: u64) -> usize {
        let mut count = 0;
        for key in keys {
            let shard = &self.shards[self.hash(key)];
            if let Some(entry) = shard.get(key.as_ref())
                && entry.expiry.is_none_or(|exp| now < exp)
            {
                count += 1;
            }
        }
        count
    }

    pub fn keys(&self, now: u64) -> Vec<Bytes> {
        let mut result = Vec::new();
        for shard in &self.shards {
            for entry in shard.iter() {
                let (key, val) = entry.pair();
                if val.expiry.is_none_or(|exp| exp > now) {
                    result.push(key.clone());
                }
            }
        }
        result
    }

    /// Scan keys with cursor-based iteration
    /// Returns (new_cursor, keys_found)
    /// Cursor encoding: shard_idx * 1_000_000_000 + position_in_shard
    /// Cursor 0 means iteration complete
    pub fn scan(
        &self,
        cursor: u64,
        count_hint: usize,
        pattern: Option<&[u8]>,
        now: u64,
    ) -> (u64, Vec<Bytes>) {
        let mut result = Vec::new();
        let mut keys_checked = 0;
        let count = count_hint.max(10).min(1000); // Clamp count between 10-1000

        // Decode cursor: shard_idx and position within shard
        let start_shard = (cursor / 1_000_000_000) as usize;
        let start_position = (cursor % 1_000_000_000) as usize;

        if start_shard >= self.num_shards {
            return (0, result); // Cursor out of range, iteration complete
        }

        // Iterate through shards starting from cursor position
        let mut current_shard = start_shard;
        let mut current_position = start_position;
        
        while current_shard < self.num_shards {
            let shard = &self.shards[current_shard];
            let entries: Vec<_> = shard.iter().collect(); // Collect to avoid holding lock during processing

            for (pos, entry) in entries.iter().enumerate().skip(current_position) {
                // Check if we have enough keys or checked enough entries
                if result.len() >= count || keys_checked >= count * 3 {
                    // Encode new cursor position
                    let new_cursor = if pos + 1 < entries.len() {
                        // Still in this shard
                        (current_shard as u64) * 1_000_000_000 + (pos + 1) as u64
                    } else if current_shard + 1 < self.num_shards {
                        // Move to next shard
                        ((current_shard + 1) as u64) * 1_000_000_000
                    } else {
                        // Reached end
                        0
                    };
                    return (new_cursor, result);
                }

                keys_checked += 1;
                let (key, val) = entry.pair();

                // Skip expired keys
                if val.expiry.is_some_and(|exp| now >= exp) {
                    continue;
                }

                // Apply pattern matching if provided
                if let Some(pat) = pattern {
                    if !matches_pattern(key, pat) {
                        continue;
                    }
                }

                result.push(key.clone());
            }
            
            // Move to next shard
            current_shard += 1;
            current_position = 0;
        }

        // Iteration complete
        (0, result)
    }

    pub fn len(&self) -> usize {
        self.shards.iter().map(|s| s.len()).sum()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.shards.iter().all(|s| s.is_empty())
    }

    pub fn clear(&self) {
        for shard in &self.shards {
            shard.clear();
        }
    }
}

// ==================== Helper Functions ====================

#[inline(always)]
pub fn get_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[inline(always)]
pub fn get_uptime_seconds() -> u32 {
    (get_timestamp() - *SERVER_START_TIME) as u32
}

#[inline(always)]
pub fn entry_size(key_len: usize, value_len: usize) -> usize {
    key_len + value_len + 64
}

#[inline(always)]
fn maybe_update_access_time(entry: &dashmap::mapref::one::Ref<'_, Bytes, Entry>) {
    if fastrand::u32(..).is_multiple_of(10) {
        entry
            .last_accessed
            .store(get_uptime_seconds(), Ordering::Relaxed);
    }
}

// ==================== Eviction ====================

#[inline(always)]
pub fn evict_if_needed(store: &ShardedStore, needed_size: usize) -> bool {
    let max_memory = CONFIG.memory.max_memory;

    if max_memory == 0 {
        return true;
    }

    let current = MEMORY_USED.load(Ordering::Relaxed);
    if current + needed_size as u64 <= max_memory {
        return true;
    }

    let policy: EvictionPolicy = CONFIG.memory.eviction_policy.parse().unwrap_or_default();

    if policy == EvictionPolicy::NoEviction {
        return false;
    }

    let mut freed = 0;
    let mut attempts = 0;
    let max_attempts = 100;

    while freed < needed_size && attempts < max_attempts {
        attempts += 1;

        let evicted = match policy {
            EvictionPolicy::AllKeysLru => evict_lru(store),
            EvictionPolicy::AllKeysRandom => evict_random(store),
            EvictionPolicy::NoEviction => break,
        };

        if evicted == 0 {
            break;
        }

        freed += evicted;
    }

    let final_used = MEMORY_USED.load(Ordering::Relaxed);
    final_used + needed_size as u64 <= max_memory
}

#[inline]
fn evict_lru(store: &ShardedStore) -> usize {
    let sample_size = CONFIG.memory.eviction_sample_size;

    let mut oldest_key: Option<Bytes> = None;
    let mut oldest_time: u32 = u32::MAX;
    let mut oldest_shard_idx = 0;

    for _ in 0..sample_size {
        let shard_idx = fastrand::usize(..store.num_shards);
        let shard = &store.shards[shard_idx];

        // Sample a random entry from the shard
        let len = shard.len();
        if len == 0 {
            continue;
        }
        let skip = fastrand::usize(..len);
        if let Some(entry) = shard.iter().nth(skip) {
            let last_accessed = entry.value().last_accessed.load(Ordering::Relaxed);

            if oldest_key.is_none() || last_accessed < oldest_time {
                oldest_key = Some(entry.key().clone());
                oldest_time = last_accessed;
                oldest_shard_idx = shard_idx;
            }
        }
    }

    if let Some(key) = oldest_key {
        let key_len = key.len();
        let shard = &store.shards[oldest_shard_idx];
        if let Some((_, entry)) = shard.remove(&key) {
            let size = entry_size(key_len, entry.value.len());
            MEMORY_USED.fetch_sub(size as u64, Ordering::Relaxed);
            EVICTED_KEYS.fetch_add(1, Ordering::Relaxed);
            return size;
        }
    }

    0
}

#[inline]
fn evict_random(store: &ShardedStore) -> usize {
    let shard_idx = fastrand::usize(..store.num_shards);
    let shard = &store.shards[shard_idx];

    // Sample a random entry from the shard
    let len = shard.len();
    if len == 0 {
        return 0;
    }
    let skip = fastrand::usize(..len);
    if let Some(entry) = shard.iter().nth(skip) {
        let key = entry.key().clone();
        let key_len = key.len();
        drop(entry);

        if let Some((_, entry)) = shard.remove(&key) {
            let size = entry_size(key_len, entry.value.len());
            MEMORY_USED.fetch_sub(size as u64, Ordering::Relaxed);
            EVICTED_KEYS.fetch_add(1, Ordering::Relaxed);
            return size;
        }
    }

    0
}

// Expire random keys (passive expiration)
pub fn expire_random_keys(store: &ShardedStore, count: usize) -> usize {
    let now = get_timestamp();
    let mut expired_count = 0;

    for _ in 0..count {
        let shard_idx = fastrand::usize(..store.num_shards);
        let shard = &store.shards[shard_idx];

        // Sample a random entry from the shard
        let len = shard.len();
        if len == 0 {
            continue;
        }
        let skip = fastrand::usize(..len);
        
        if let Some(entry) = shard.iter().nth(skip)
            && let Some(expiry) = entry.value().expiry
            && now >= expiry
        {
            // Extract info before dropping to avoid race condition
            let key = entry.key().clone();
            let key_len = key.len();
            let value_len = entry.value().value.len();
            drop(entry);

            // Atomic remove - only one thread can succeed
            if shard.remove(key.as_ref()).is_some() {
                expired_count += 1;
                if CONFIG.memory.max_memory > 0 {
                    let size = entry_size(key_len, value_len);
                    MEMORY_USED.fetch_sub(size as u64, Ordering::Relaxed);
                }
            }
        }
    }

    expired_count
}

// ==================== Pattern Matching ====================

/// Simple glob pattern matching for SCAN MATCH
/// Supports:
/// - * matches any sequence of characters
/// - ? matches any single character
/// - Literal characters match exactly
fn matches_pattern(key: &[u8], pattern: &[u8]) -> bool {
    let key_len = key.len();
    let pat_len = pattern.len();
    let mut key_idx = 0;
    let mut pat_idx = 0;
    let mut star_pos: Option<usize> = None;
    let mut key_backup = 0;

    while key_idx < key_len {
        if pat_idx < pat_len && pattern[pat_idx] == b'*' {
            star_pos = Some(pat_idx);
            key_backup = key_idx;
            pat_idx += 1;
        } else if pat_idx < pat_len
            && (pattern[pat_idx] == b'?' || pattern[pat_idx] == key[key_idx])
        {
            key_idx += 1;
            pat_idx += 1;
        } else if let Some(star) = star_pos {
            pat_idx = star + 1;
            key_backup += 1;
            key_idx = key_backup;
        } else {
            return false;
        }
    }

    // Handle trailing stars
    while pat_idx < pat_len && pattern[pat_idx] == b'*' {
        pat_idx += 1;
    }

    pat_idx == pat_len
}
