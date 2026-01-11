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
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::collections::VecDeque;
use std::sync::RwLock;
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

// Cached eviction policy (parsed once at startup, avoids string parsing on hot path)
pub static EVICTION_POLICY: Lazy<EvictionPolicy> = Lazy::new(|| {
    CONFIG.memory.eviction_policy.parse().unwrap_or_default()
});

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

// ==================== Entry Value Types ====================

#[derive(Clone)]
pub enum EntryValue {
    String(Bytes),
    Hash(Arc<DashMap<Bytes, Bytes>>),
}

// ==================== S3-FIFO Queue Types ====================

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum QueueType {
    Small = 0,
    Main = 1,
}

impl From<u8> for QueueType {
    fn from(value: u8) -> Self {
        match value {
            0 => QueueType::Small,
            1 => QueueType::Main,
            _ => QueueType::Small,
        }
    }
}

// ==================== Entry ====================

pub struct Entry {
    pub value: EntryValue,
    pub expiry: Option<u64>,
    pub last_accessed: AtomicU32,
    pub queue_type: AtomicU8,      // 0=Small, 1=Main (only used for S3-FIFO)
    pub access_count: AtomicU8,     // For Small queue promotion (only used for S3-FIFO)
}

impl Clone for Entry {
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
            expiry: self.expiry,
            last_accessed: AtomicU32::new(self.last_accessed.load(Ordering::Relaxed)),
            queue_type: AtomicU8::new(self.queue_type.load(Ordering::Relaxed)),
            access_count: AtomicU8::new(self.access_count.load(Ordering::Relaxed)),
        }
    }
}

// ==================== S3-FIFO Ghost Queue ====================

struct GhostEntry {
    key_hash: u64,
    #[allow(dead_code)] // Reserved for future use (e.g., expiring old ghost entries)
    evicted_at: u64,
}

pub struct S3FifoQueues {
    small_queue: VecDeque<Bytes>,      // Keys in Small queue
    main_queue: VecDeque<Bytes>,        // Keys in Main queue
    ghost_queue: VecDeque<GhostEntry>,  // Metadata only
    ghost_set: ahash::AHashSet<u64>,    // Fast lookup for ghost entries
    small_capacity: usize,
    main_capacity: usize,
    ghost_capacity: usize,
}

impl S3FifoQueues {
    fn new(small_capacity: usize, main_capacity: usize, ghost_capacity: usize) -> Self {
        Self {
            small_queue: VecDeque::with_capacity(small_capacity),
            main_queue: VecDeque::with_capacity(main_capacity),
            ghost_queue: VecDeque::with_capacity(ghost_capacity),
            ghost_set: ahash::AHashSet::with_capacity(ghost_capacity),
            small_capacity,
            main_capacity,
            ghost_capacity,
        }
    }

    fn is_in_ghost(&self, key_hash: u64) -> bool {
        self.ghost_set.contains(&key_hash)
    }

    fn add_to_ghost(&mut self, key_hash: u64, now: u64) {
        // Remove oldest if at capacity
        if self.ghost_queue.len() >= self.ghost_capacity {
            if let Some(oldest) = self.ghost_queue.pop_front() {
                self.ghost_set.remove(&oldest.key_hash);
            }
        }
        self.ghost_queue.push_back(GhostEntry {
            key_hash,
            evicted_at: now,
        });
        self.ghost_set.insert(key_hash);
    }

    fn remove_from_ghost(&mut self, key_hash: u64) {
        if self.ghost_set.remove(&key_hash) {
            // Remove from queue (linear search, but ghost queue should be small)
            self.ghost_queue.retain(|e| e.key_hash != key_hash);
        }
    }
}

// ==================== Sharded Store ====================

pub struct ShardedStore {
    pub shards: Vec<Arc<DashMap<Bytes, Entry>>>,
    pub s3fifo_queues: Vec<Arc<RwLock<S3FifoQueues>>>,  // One per shard (only used for S3-FIFO)
    pub num_shards: usize,
}

impl Clone for ShardedStore {
    fn clone(&self) -> Self {
        Self {
            shards: self.shards.clone(),
            s3fifo_queues: self.s3fifo_queues.clone(),
            num_shards: self.num_shards,
        }
    }
}

impl ShardedStore {
    pub fn new(num_shards: usize) -> Self {
        let mut shards = Vec::with_capacity(num_shards);
        let mut s3fifo_queues = Vec::with_capacity(num_shards);
        
        // Calculate queue capacities based on max_memory per shard
        let max_memory = CONFIG.memory.max_memory;
        let (small_cap, main_cap, ghost_cap) = if max_memory > 0 {
            let per_shard_memory = max_memory / num_shards as u64;
            // Estimate: assume average entry size is ~256 bytes
            let estimated_entries = (per_shard_memory / 256).max(100) as usize;
            let small_cap = (estimated_entries as f64 * 0.1).ceil() as usize;
            let main_cap = (estimated_entries as f64 * 0.9).ceil() as usize;
            let ghost_cap = (estimated_entries as f64 * 0.1).ceil() as usize;
            (small_cap.max(10), main_cap.max(10), ghost_cap.max(10))
        } else {
            // Default sizes when no memory limit
            (100, 900, 100)
        };
        
        for _ in 0..num_shards {
            shards.push(Arc::new(DashMap::with_capacity(1000)));
            s3fifo_queues.push(Arc::new(RwLock::new(S3FifoQueues::new(
                small_cap,
                main_cap,
                ghost_cap,
            ))));
        }
        Self {
            shards,
            s3fifo_queues,
            num_shards,
        }
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
        let shard_idx = self.hash(&key);
        let shard = &self.shards[shard_idx];

        let timestamp = if fastrand::u32(..).is_multiple_of(10) {
            get_uptime_seconds()
        } else {
            0
        };

        // Check if using S3-FIFO policy (using cached policy to avoid string parsing)
        let policy = *EVICTION_POLICY;
        
        // Remove old entry from queues if it exists and we're using S3-FIFO
        if policy == EvictionPolicy::AllKeysS3Fifo {
            if let Some(old_entry) = shard.get(&key) {
                let old_queue_type = QueueType::from(old_entry.queue_type.load(Ordering::Relaxed));
                let queues = &self.s3fifo_queues[shard_idx];
                let mut queues_guard = queues.write().unwrap();
                
                match old_queue_type {
                    QueueType::Small => {
                        queues_guard.small_queue.retain(|k| k != &key);
                    }
                    QueueType::Main => {
                        queues_guard.main_queue.retain(|k| k != &key);
                    }
                }
                drop(queues_guard);
            }
        }
        
        let (queue_type, access_count) = if policy == EvictionPolicy::AllKeysS3Fifo {
            // Check Ghost queue to decide Small vs Main
            let queues = &self.s3fifo_queues[shard_idx];
            let mut queues_guard = queues.write().unwrap();
            
            let mut hasher = AHasher::default();
            hasher.write(&key);
            let key_hash = hasher.finish();
            
            let qtype = if queues_guard.is_in_ghost(key_hash) {
                // Found in Ghost - insert to Main queue
                queues_guard.remove_from_ghost(key_hash);
                // Add to Main queue (may need to evict if full)
                while queues_guard.main_queue.len() >= queues_guard.main_capacity {
                    if let Some(_) = queues_guard.main_queue.pop_front() {
                        break;
                    }
                }
                queues_guard.main_queue.push_back(key.clone());
                QueueType::Main
            } else {
                // Not in Ghost - insert to Small queue
                while queues_guard.small_queue.len() >= queues_guard.small_capacity {
                    if let Some(_) = queues_guard.small_queue.pop_front() {
                        break;
                    }
                }
                queues_guard.small_queue.push_back(key.clone());
                QueueType::Small
            };
            drop(queues_guard);
            (qtype as u8, 0u8)
        } else {
            // Not using S3-FIFO, use default values
            (0u8, 0u8)
        };

        let old_entry = shard.insert(
            key,
            Entry {
                value: EntryValue::String(value),
                expiry,
                last_accessed: AtomicU32::new(timestamp),
                queue_type: AtomicU8::new(queue_type),
                access_count: AtomicU8::new(access_count),
            },
        );

        old_entry.map(|e| calculate_entry_size(key_len, &e.value))
    }

    #[inline(always)]
    pub fn get_existing_size(&self, key: &[u8], now: u64) -> Option<usize> {
        let shard = &self.shards[self.hash(key)];
        if let Some(entry) = shard.get(key) {
            // Check if expired
            if let Some(expiry) = entry.expiry && now >= expiry {
                return None;
            }
            return Some(calculate_entry_size(key.len(), &entry.value));
        }
        None
    }

    #[inline(always)]
    pub fn get(&self, key: &[u8], now: u64) -> Option<Bytes> {
        let shard_idx = self.hash(key);
        let shard = &self.shards[shard_idx];

        if let Some(entry) = shard.get(key) {
            if let Some(expiry) = entry.expiry
                && now >= expiry
            {
                // Extract info before dropping to avoid race condition
                let key_bytes = Bytes::copy_from_slice(key);
                let key_len = key_bytes.len();
                let entry_size = calculate_entry_size(key_len, &entry.value);
                drop(entry);

                // Atomic remove - only one thread can succeed
                if shard.remove(key_bytes.as_ref()).is_some() && CONFIG.memory.max_memory > 0 {
                    MEMORY_USED.fetch_sub(entry_size as u64, Ordering::Relaxed);
                }
                return None;
            }

            maybe_update_access_time(&entry);
            
            // Handle S3-FIFO access tracking (using cached policy to avoid string parsing)
            let policy = *EVICTION_POLICY;
            if policy == EvictionPolicy::AllKeysS3Fifo {
                let queue_type = QueueType::from(entry.queue_type.load(Ordering::Relaxed));
                let key_bytes = Bytes::copy_from_slice(key);
                
                match queue_type {
                    QueueType::Small => {
                        // Increment access count
                        let access_count = entry.access_count.fetch_add(1, Ordering::Relaxed) + 1;
                        
                        // Promote to Main if accessed more than once
                        if access_count > 1 {
                            let queues = &self.s3fifo_queues[shard_idx];
                            let mut queues_guard = queues.write().unwrap();
                            
                            // Remove from Small queue
                            queues_guard.small_queue.retain(|k| k != &key_bytes);
                            
                            // Add to Main queue
                            while queues_guard.main_queue.len() >= queues_guard.main_capacity {
                                if let Some(_) = queues_guard.main_queue.pop_front() {
                                    break;
                                }
                            }
                            queues_guard.main_queue.push_back(key_bytes.clone());
                            drop(queues_guard);
                            
                            // Update entry queue type
                            entry.queue_type.store(QueueType::Main as u8, Ordering::Relaxed);
                        }
                    }
                    QueueType::Main => {
                        // Reinsert at tail to maintain FIFO order
                        let queues = &self.s3fifo_queues[shard_idx];
                        let mut queues_guard = queues.write().unwrap();
                        
                        // Remove from current position
                        queues_guard.main_queue.retain(|k| k != &key_bytes);
                        
                        // Add to tail
                        queues_guard.main_queue.push_back(key_bytes);
                        drop(queues_guard);
                    }
                }
            }
            
            // Only return value if it's a string
            if let EntryValue::String(ref val) = entry.value {
                return Some(val.clone());
            }
            // If it's a hash, return None (type mismatch)
            return None;
        }
        None
    }

    #[inline(always)]
    pub fn delete(&self, keys: &[Bytes], now: u64) -> usize {
        let policy = *EVICTION_POLICY;
        let mut count = 0;
        for key in keys {
            let shard_idx = self.hash(key);
            let shard = &self.shards[shard_idx];
            
            // Remove from S3-FIFO queues if using S3-FIFO
            if policy == EvictionPolicy::AllKeysS3Fifo {
                if let Some(entry) = shard.get(key) {
                    let queue_type = QueueType::from(entry.queue_type.load(Ordering::Relaxed));
                    let queues = &self.s3fifo_queues[shard_idx];
                    let mut queues_guard = queues.write().unwrap();
                    
                    match queue_type {
                        QueueType::Small => {
                            queues_guard.small_queue.retain(|k| k != key);
                        }
                        QueueType::Main => {
                            queues_guard.main_queue.retain(|k| k != key);
                        }
                    }
                    drop(queues_guard);
                }
            }
            
            if let Some((_, entry)) = shard.remove(key) {
                // Always decrement memory if key was removed, regardless of expiry
                // (expired keys still consume memory until removed)
                if CONFIG.memory.max_memory > 0 {
                    let size = calculate_entry_size(key.len(), &entry.value);
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

    // ==================== Hash Operations ====================

    /// Set one or more field-value pairs in a hash
    /// Returns the number of fields that were newly set (not updated)
    pub fn hset(&self, key: Bytes, fields: &[(Bytes, Bytes)], now: u64) -> Result<usize, &'static str> {
        let key_len = key.len();
        let shard_idx = self.hash(&key);
        let shard = &self.shards[shard_idx];

        let timestamp = if fastrand::u32(..).is_multiple_of(10) {
            get_uptime_seconds()
        } else {
            0
        };

        // Check if using S3-FIFO policy (using cached policy to avoid string parsing)
        let policy = *EVICTION_POLICY;
        
        // Remove old entry from queues if it exists and we're using S3-FIFO
        if policy == EvictionPolicy::AllKeysS3Fifo {
            if let Some(old_entry) = shard.get(&key) {
                let old_queue_type = QueueType::from(old_entry.queue_type.load(Ordering::Relaxed));
                let queues = &self.s3fifo_queues[shard_idx];
                let mut queues_guard = queues.write().unwrap();
                
                match old_queue_type {
                    QueueType::Small => {
                        queues_guard.small_queue.retain(|k| k != &key);
                    }
                    QueueType::Main => {
                        queues_guard.main_queue.retain(|k| k != &key);
                    }
                }
                drop(queues_guard);
            }
        }

        // Check if key exists and validate type
        let (hash_map, old_size, preserved_expiry) = if let Some(entry) = shard.get(&key) {
            // Check if expired
            if let Some(expiry) = entry.expiry && now >= expiry {
                // Expired, create new hash
                (Arc::new(DashMap::new()), None, None)
            } else {
                // Check type and extract info
                match &entry.value {
                    EntryValue::Hash(hash) => {
                        // CRITICAL: Clone the contents, not just the Arc reference
                        // This ensures we don't modify the original hash_map while calculating sizes
                        let size = calculate_entry_size(key_len, &entry.value);
                        let new_hash_map = Arc::new(DashMap::new());
                        // Copy all existing fields to the new hash map
                        for hash_entry in hash.iter() {
                            new_hash_map.insert(hash_entry.key().clone(), hash_entry.value().clone());
                        }
                        (new_hash_map, Some(size), entry.expiry)
                    }
                    EntryValue::String(_) => {
                        return Err("WRONGTYPE Operation against a key holding the wrong kind of value");
                    }
                }
            }
        } else {
            // Key doesn't exist, create new hash
            (Arc::new(DashMap::new()), None, None)
        };

        // Set fields and count new ones
        let mut new_count = 0;
        for (field, value) in fields {
            if hash_map.insert(field.clone(), value.clone()).is_none() {
                new_count += 1;
            }
        }

        // Determine queue placement for S3-FIFO
        let (queue_type, access_count) = if policy == EvictionPolicy::AllKeysS3Fifo {
            let queues = &self.s3fifo_queues[shard_idx];
            let mut queues_guard = queues.write().unwrap();
            
            let mut hasher = AHasher::default();
            hasher.write(&key);
            let key_hash = hasher.finish();
            
            let qtype = if queues_guard.is_in_ghost(key_hash) {
                // Found in Ghost - insert to Main queue
                queues_guard.remove_from_ghost(key_hash);
                while queues_guard.main_queue.len() >= queues_guard.main_capacity {
                    if let Some(_) = queues_guard.main_queue.pop_front() {
                        break;
                    }
                }
                queues_guard.main_queue.push_back(key.clone());
                QueueType::Main
            } else {
                // Not in Ghost - insert to Small queue
                while queues_guard.small_queue.len() >= queues_guard.small_capacity {
                    if let Some(_) = queues_guard.small_queue.pop_front() {
                        break;
                    }
                }
                queues_guard.small_queue.push_back(key.clone());
                QueueType::Small
            };
            drop(queues_guard);
            (qtype as u8, 0u8)
        } else {
            (0u8, 0u8)
        };

        // Insert or update entry
        let new_entry = Entry {
            value: EntryValue::Hash(hash_map.clone()),
            expiry: preserved_expiry, // Preserve expiry from old entry if it exists
            last_accessed: AtomicU32::new(timestamp),
            queue_type: AtomicU8::new(queue_type),
            access_count: AtomicU8::new(access_count),
        };

        let removed_entry = shard.insert(key, new_entry);
        let new_size = calculate_entry_size(key_len, &EntryValue::Hash(hash_map));

        // Update memory tracking
        if CONFIG.memory.max_memory > 0 {
            if let Some(old_sz) = old_size {
                MEMORY_USED.fetch_sub(old_sz as u64, Ordering::Relaxed);
            } else if let Some(old) = removed_entry {
                // Fallback: calculate from removed entry if old_size wasn't set
                MEMORY_USED.fetch_sub(calculate_entry_size(key_len, &old.value) as u64, Ordering::Relaxed);
            }
            MEMORY_USED.fetch_add(new_size as u64, Ordering::Relaxed);
        }

        Ok(new_count)
    }

    /// Get a field value from a hash
    pub fn hget(&self, key: &[u8], field: &[u8], now: u64) -> Result<Option<Bytes>, &'static str> {
        let shard_idx = self.hash(key);
        let shard = &self.shards[shard_idx];

        if let Some(entry) = shard.get(key) {
            if let Some(expiry) = entry.expiry && now >= expiry {
                // Expired - remove it
                let key_bytes = Bytes::copy_from_slice(key);
                let key_len = key_bytes.len();
                let entry_size = calculate_entry_size(key_len, &entry.value);
                drop(entry);

                if shard.remove(key_bytes.as_ref()).is_some() && CONFIG.memory.max_memory > 0 {
                    MEMORY_USED.fetch_sub(entry_size as u64, Ordering::Relaxed);
                }
                return Ok(None);
            }

            maybe_update_access_time(&entry);
            
            // Handle S3-FIFO access tracking (same logic as get(), using cached policy)
            let policy = *EVICTION_POLICY;
            if policy == EvictionPolicy::AllKeysS3Fifo {
                let queue_type = QueueType::from(entry.queue_type.load(Ordering::Relaxed));
                let key_bytes = Bytes::copy_from_slice(key);
                
                match queue_type {
                    QueueType::Small => {
                        let access_count = entry.access_count.fetch_add(1, Ordering::Relaxed) + 1;
                        if access_count > 1 {
                            let queues = &self.s3fifo_queues[shard_idx];
                            let mut queues_guard = queues.write().unwrap();
                            queues_guard.small_queue.retain(|k| k != &key_bytes);
                            while queues_guard.main_queue.len() >= queues_guard.main_capacity {
                                if let Some(_) = queues_guard.main_queue.pop_front() {
                                    break;
                                }
                            }
                            queues_guard.main_queue.push_back(key_bytes.clone());
                            drop(queues_guard);
                            entry.queue_type.store(QueueType::Main as u8, Ordering::Relaxed);
                        }
                    }
                    QueueType::Main => {
                        let queues = &self.s3fifo_queues[shard_idx];
                        let mut queues_guard = queues.write().unwrap();
                        queues_guard.main_queue.retain(|k| k != &key_bytes);
                        queues_guard.main_queue.push_back(key_bytes);
                        drop(queues_guard);
                    }
                }
            }

            match &entry.value {
                EntryValue::Hash(hash_map) => {
                    Ok(hash_map.get(field).map(|v| v.value().clone()))
                }
                EntryValue::String(_) => {
                    Err("WRONGTYPE Operation against a key holding the wrong kind of value")
                }
            }
        } else {
            Ok(None)
        }
    }

    /// Get all field-value pairs from a hash
    pub fn hgetall(&self, key: &[u8], now: u64) -> Result<Vec<Bytes>, &'static str> {
        let shard_idx = self.hash(key);
        let shard = &self.shards[shard_idx];

        if let Some(entry) = shard.get(key) {
            if let Some(expiry) = entry.expiry && now >= expiry {
                // Expired - remove it
                let key_bytes = Bytes::copy_from_slice(key);
                let key_len = key_bytes.len();
                let entry_size = calculate_entry_size(key_len, &entry.value);
                drop(entry);

                if shard.remove(key_bytes.as_ref()).is_some() && CONFIG.memory.max_memory > 0 {
                    MEMORY_USED.fetch_sub(entry_size as u64, Ordering::Relaxed);
                }
                return Ok(Vec::new());
            }

            maybe_update_access_time(&entry);
            
            // Handle S3-FIFO access tracking (same logic as get(), using cached policy)
            let policy = *EVICTION_POLICY;
            if policy == EvictionPolicy::AllKeysS3Fifo {
                let queue_type = QueueType::from(entry.queue_type.load(Ordering::Relaxed));
                let key_bytes = Bytes::copy_from_slice(key);
                
                match queue_type {
                    QueueType::Small => {
                        let access_count = entry.access_count.fetch_add(1, Ordering::Relaxed) + 1;
                        if access_count > 1 {
                            let queues = &self.s3fifo_queues[shard_idx];
                            let mut queues_guard = queues.write().unwrap();
                            queues_guard.small_queue.retain(|k| k != &key_bytes);
                            while queues_guard.main_queue.len() >= queues_guard.main_capacity {
                                if let Some(_) = queues_guard.main_queue.pop_front() {
                                    break;
                                }
                            }
                            queues_guard.main_queue.push_back(key_bytes.clone());
                            drop(queues_guard);
                            entry.queue_type.store(QueueType::Main as u8, Ordering::Relaxed);
                        }
                    }
                    QueueType::Main => {
                        let queues = &self.s3fifo_queues[shard_idx];
                        let mut queues_guard = queues.write().unwrap();
                        queues_guard.main_queue.retain(|k| k != &key_bytes);
                        queues_guard.main_queue.push_back(key_bytes);
                        drop(queues_guard);
                    }
                }
            }

            match &entry.value {
                EntryValue::Hash(hash_map) => {
                    let mut result = Vec::new();
                    for entry in hash_map.iter() {
                        result.push(entry.key().clone());
                        result.push(entry.value().clone());
                    }
                    Ok(result)
                }
                EntryValue::String(_) => {
                    Err("WRONGTYPE Operation against a key holding the wrong kind of value")
                }
            }
        } else {
            Ok(Vec::new())
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
pub fn calculate_entry_size(key_len: usize, value: &EntryValue) -> usize {
    match value {
        EntryValue::String(bytes) => entry_size(key_len, bytes.len()),
        EntryValue::Hash(hash_map) => {
            // Base overhead for hash entry
            let mut size = key_len + 64 + 128; // Base entry + DashMap overhead
            // Add size of all field keys and values
            for entry in hash_map.iter() {
                size += entry.key().len() + entry.value().len();
            }
            size
        }
    }
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

    let policy = *EVICTION_POLICY;

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
            EvictionPolicy::AllKeysS3Fifo => evict_s3fifo(store),
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
            let size = calculate_entry_size(key_len, &entry.value);
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
            let size = calculate_entry_size(key_len, &entry.value);
            MEMORY_USED.fetch_sub(size as u64, Ordering::Relaxed);
            EVICTED_KEYS.fetch_add(1, Ordering::Relaxed);
            return size;
        }
    }

    0
}

#[inline]
fn evict_s3fifo(store: &ShardedStore) -> usize {
    // Try each shard until we find one with items to evict
    for _ in 0..store.num_shards {
        let shard_idx = fastrand::usize(..store.num_shards);
        let shard = &store.shards[shard_idx];
        let queues = &store.s3fifo_queues[shard_idx];
        
        // Try to evict from Small queue first
        let mut evicted_key: Option<Bytes> = None;
        let mut from_small = false;
        
        {
            let mut queues_guard = queues.write().unwrap();
            
            // Try Small queue first (FIFO - remove from front)
            if let Some(key) = queues_guard.small_queue.pop_front() {
                evicted_key = Some(key);
                from_small = true;
            } else if let Some(key) = queues_guard.main_queue.pop_front() {
                // Fall back to Main queue (FIFO - remove from front)
                evicted_key = Some(key);
                from_small = false;
            }
            
            // If we found a key to evict from Small, add to Ghost
            if let Some(ref key) = evicted_key {
                if from_small {
                    let mut hasher = AHasher::default();
                    hasher.write(key);
                    let key_hash = hasher.finish();
                    queues_guard.add_to_ghost(key_hash, get_timestamp());
                }
            }
        }
        
        // Now remove from the actual store
        if let Some(key) = evicted_key {
            let key_len = key.len();
            if let Some((_, entry)) = shard.remove(&key) {
                let size = calculate_entry_size(key_len, &entry.value);
                MEMORY_USED.fetch_sub(size as u64, Ordering::Relaxed);
                EVICTED_KEYS.fetch_add(1, Ordering::Relaxed);
                return size;
            }
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
            let entry_size = calculate_entry_size(key_len, &entry.value().value);
            drop(entry);

            // Atomic remove - only one thread can succeed
            if shard.remove(key.as_ref()).is_some() {
                expired_count += 1;
                if CONFIG.memory.max_memory > 0 {
                    MEMORY_USED.fetch_sub(entry_size as u64, Ordering::Relaxed);
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
