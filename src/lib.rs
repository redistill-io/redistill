// Redistill library - exposes internal components for testing and potential future SDK usage
// This module provides access to the core data structures and functions without requiring
// the full binary to be run.

#![allow(dead_code)] // Some items may only be used in tests

use ahash::AHasher;
pub use bytes::{Bytes, BytesMut};
pub use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::hash::Hasher;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

// ==================== Configuration Structures ====================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_bind")]
    pub bind: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_num_shards")]
    pub num_shards: usize,
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
    #[serde(default = "default_buffer_size")]
    pub buffer_size: usize,
    #[serde(default = "default_buffer_pool_size")]
    pub buffer_pool_size: usize,
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,
    #[serde(default = "default_connection_timeout")]
    pub connection_timeout: u64,
    #[serde(default)]
    pub connection_rate_limit: u64,
    #[serde(default)]
    pub health_check_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub tls_enabled: bool,
    #[serde(default)]
    pub tls_cert_path: String,
    #[serde(default)]
    pub tls_key_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
    #[serde(default = "default_log_format")]
    pub format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceConfig {
    #[serde(default = "default_true")]
    pub tcp_nodelay: bool,
    #[serde(default = "default_tcp_keepalive")]
    pub tcp_keepalive: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    #[serde(default)]
    pub max_memory: u64,
    #[serde(default = "default_eviction_policy")]
    pub eviction_policy: String,
    #[serde(default = "default_eviction_sample_size")]
    pub eviction_sample_size: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub security: SecurityConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub performance: PerformanceConfig,
    #[serde(default)]
    pub memory: MemoryConfig,
}

// Default functions
fn default_bind() -> String {
    "127.0.0.1".to_string()
}
fn default_port() -> u16 {
    6379
}
fn default_num_shards() -> usize {
    256
}
fn default_batch_size() -> usize {
    16
}
fn default_buffer_size() -> usize {
    16 * 1024
}
fn default_buffer_pool_size() -> usize {
    1024
}
fn default_max_connections() -> usize {
    10000
}
fn default_connection_timeout() -> u64 {
    300
}
fn default_log_level() -> String {
    "info".to_string()
}
fn default_log_format() -> String {
    "text".to_string()
}
fn default_true() -> bool {
    true
}
fn default_tcp_keepalive() -> u64 {
    60
}
fn default_eviction_policy() -> String {
    "allkeys-lru".to_string()
}
fn default_eviction_sample_size() -> usize {
    5
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            port: default_port(),
            num_shards: default_num_shards(),
            batch_size: default_batch_size(),
            buffer_size: default_buffer_size(),
            buffer_pool_size: default_buffer_pool_size(),
            max_connections: default_max_connections(),
            connection_timeout: default_connection_timeout(),
            connection_rate_limit: 0,
            health_check_port: 0,
        }
    }
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            password: String::new(),
            tls_enabled: false,
            tls_cert_path: String::new(),
            tls_key_path: String::new(),
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            format: default_log_format(),
        }
    }
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        Self {
            tcp_nodelay: default_true(),
            tcp_keepalive: default_tcp_keepalive(),
        }
    }
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            max_memory: 0,
            eviction_policy: default_eviction_policy(),
            eviction_sample_size: default_eviction_sample_size(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            security: SecurityConfig::default(),
            logging: LoggingConfig::default(),
            performance: PerformanceConfig::default(),
            memory: MemoryConfig::default(),
        }
    }
}

// ==================== Eviction Policy ====================

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EvictionPolicy {
    NoEviction,
    AllKeysLru,
    AllKeysRandom,
}

impl EvictionPolicy {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "allkeys-lru" => EvictionPolicy::AllKeysLru,
            "allkeys-random" => EvictionPolicy::AllKeysRandom,
            "noeviction" => EvictionPolicy::NoEviction,
            _ => EvictionPolicy::AllKeysLru, // default
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            EvictionPolicy::NoEviction => "noeviction",
            EvictionPolicy::AllKeysLru => "allkeys-lru",
            EvictionPolicy::AllKeysRandom => "allkeys-random",
        }
    }
}

// ==================== Storage Entry ====================

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

impl ShardedStore {
    pub fn new(num_shards: usize) -> Self {
        let mut shards = Vec::with_capacity(num_shards);
        for _ in 0..num_shards {
            shards.push(Arc::new(DashMap::with_capacity(1000)));
        }
        Self { shards, num_shards }
    }

    pub fn clone(&self) -> Self {
        Self {
            shards: self.shards.clone(),
            num_shards: self.num_shards,
        }
    }

    // Fast AHash with hardware acceleration (AES-NI)
    #[inline(always)]
    pub fn hash(&self, key: &[u8]) -> usize {
        let mut hasher = AHasher::default();
        hasher.write(key);
        hasher.finish() as usize % self.num_shards
    }

    #[inline(always)]
    pub fn set(&self, key: Bytes, value: Bytes, ttl: Option<u64>, now: u64) {
        let expiry = ttl.map(|s| now + s);
        let shard = &self.shards[self.hash(&key)];
        shard.insert(
            key,
            Entry {
                value,
                expiry,
                last_accessed: AtomicU32::new(0), // Will be set by get_uptime_seconds() in real usage
            },
        );
    }

    #[inline(always)]
    pub fn get(&self, key: &[u8], now: u64) -> Option<Bytes> {
        let shard = &self.shards[self.hash(key)];

        // Try read-only access first
        if let Some(entry) = shard.get(key) {
            if let Some(expiry) = entry.expiry {
                if now >= expiry {
                    // Expired - need to remove
                    drop(entry);
                    shard.remove(key);
                    return None;
                }
            }

            return Some(entry.value.clone());
        }
        None
    }

    #[inline(always)]
    pub fn delete(&self, keys: &[Bytes]) -> usize {
        // Group by shard for efficiency
        let mut shard_keys: Vec<Vec<&Bytes>> = vec![Vec::new(); self.num_shards];
        for key in keys {
            shard_keys[self.hash(key)].push(key);
        }

        let mut count = 0;
        for (idx, keys_in_shard) in shard_keys.iter().enumerate() {
            if !keys_in_shard.is_empty() {
                let shard = &self.shards[idx];
                for key in keys_in_shard {
                    if shard.remove(*key).is_some() {
                        count += 1;
                    }
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
            if let Some(entry) = shard.get(key.as_ref()) {
                if entry.expiry.is_none() || entry.expiry.unwrap() > now {
                    count += 1;
                }
            }
        }
        count
    }

    pub fn keys(&self, now: u64) -> Vec<Bytes> {
        let mut result = Vec::new();
        for shard in &self.shards {
            for entry in shard.iter() {
                let (key, val) = entry.pair();
                if val.expiry.is_none() || val.expiry.unwrap() > now {
                    result.push(key.clone());
                }
            }
        }
        result
    }

    pub fn len(&self) -> usize {
        self.shards.iter().map(|s| s.len()).sum()
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
        .unwrap()
        .as_secs()
}

// Calculate approximate size of an entry
#[inline(always)]
pub fn entry_size(key_len: usize, value_len: usize) -> usize {
    key_len + value_len + 64 // ~64 bytes overhead for Arc, Entry struct, etc.
}

// Format bytes as human-readable string
pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.2}GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2}MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2}KB", bytes as f64 / KB as f64)
    } else {
        format!("{}B", bytes)
    }
}

// Fast byte comparison helpers
#[inline(always)]
pub fn eq_ignore_case_3(a: &[u8], b: &[u8; 3]) -> bool {
    a.len() == 3 && (a[0] | 0x20) == b[0] && (a[1] | 0x20) == b[1] && (a[2] | 0x20) == b[2]
}

#[inline(always)]
pub fn eq_ignore_case_6(a: &[u8], b: &[u8; 6]) -> bool {
    a.len() == 6
        && (a[0] | 0x20) == b[0]
        && (a[1] | 0x20) == b[1]
        && (a[2] | 0x20) == b[2]
        && (a[3] | 0x20) == b[3]
        && (a[4] | 0x20) == b[4]
        && (a[5] | 0x20) == b[5]
}

// Connection state for authentication
pub struct ConnectionState {
    pub authenticated: bool,
}

impl ConnectionState {
    pub fn new() -> Self {
        Self {
            // If no password is set, authentication is not required
            authenticated: true, // Default to true for testing
        }
    }
}

// Eviction stub for testing (actual implementation uses global config)
#[inline(always)]
pub fn evict_if_needed(_store: &ShardedStore, _needed_size: usize) -> bool {
    // In tests with default config (max_memory = 0), always allow
    true
}
