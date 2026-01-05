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
    if buf.capacity() <= CONFIG.server.buffer_size * 2 {
        BUFFER_POOL.push(buf);
    }
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
    pub fn get(&self, key: &[u8], now: u64) -> Option<Bytes> {
        let shard = &self.shards[self.hash(key)];

        if let Some(entry) = shard.get(key) {
            if let Some(expiry) = entry.expiry
                && now >= expiry
            {
                let key_len = key.len();
                let value_len = entry.value.len();
                drop(entry);

                if shard.remove(key).is_some() && CONFIG.memory.max_memory > 0 {
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
            if let Some((_, entry)) = shard.remove(key)
                && entry.expiry.is_none_or(|exp| now < exp)
            {
                count += 1;
                if CONFIG.memory.max_memory > 0 {
                    let size = entry_size(key.len(), entry.value.len());
                    MEMORY_USED.fetch_sub(size as u64, Ordering::Relaxed);
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
                && entry.expiry.is_none_or(|exp| exp > now)
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

        if let Some(entry) = shard.iter().next() {
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

    if let Some(entry) = shard.iter().next() {
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

        if let Some(entry) = shard.iter().next()
            && let Some(expiry) = entry.value().expiry
            && now >= expiry
        {
            let key = entry.key().clone();
            let key_len = key.len();
            let value_len = entry.value().value.len();
            drop(entry);

            if shard.remove(&key).is_some() {
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
