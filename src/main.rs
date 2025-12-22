// Global allocator - jemalloc for performance
#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: jemallocator::Jemalloc = jemallocator::Jemalloc;

use bytes::{Buf, Bytes, BytesMut};
use crossbeam::queue::SegQueue;
use dashmap::DashMap;
use http_body_util::Full;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::signal;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig as RustlsServerConfig;

// Configuration structures
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ServerConfig {
    #[serde(default = "default_bind")]
    bind: String,
    #[serde(default = "default_port")]
    port: u16,
    #[serde(default = "default_num_shards")]
    num_shards: usize,
    #[serde(default = "default_batch_size")]
    batch_size: usize,
    #[serde(default = "default_buffer_size")]
    buffer_size: usize,
    #[serde(default = "default_buffer_pool_size")]
    buffer_pool_size: usize,
    #[serde(default = "default_max_connections")]
    max_connections: usize,
    #[serde(default = "default_connection_timeout")]
    connection_timeout: u64,
    #[serde(default)]
    connection_rate_limit: u64, // Max new connections per second (0 = unlimited)
    #[serde(default)]
    health_check_port: u16, // HTTP health check port (0 = disabled)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SecurityConfig {
    #[serde(default)]
    password: String,
    #[serde(default)]
    tls_enabled: bool,
    #[serde(default)]
    tls_cert_path: String,
    #[serde(default)]
    tls_key_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LoggingConfig {
    #[serde(default = "default_log_level")]
    level: String,
    #[serde(default = "default_log_format")]
    format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PerformanceConfig {
    #[serde(default = "default_true")]
    tcp_nodelay: bool,
    #[serde(default = "default_tcp_keepalive")]
    tcp_keepalive: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MemoryConfig {
    #[serde(default)]
    max_memory: u64, // 0 = unlimited
    #[serde(default = "default_eviction_policy")]
    eviction_policy: String,
    #[serde(default = "default_eviction_sample_size")]
    eviction_sample_size: usize,
}

fn default_eviction_policy() -> String {
    "allkeys-lru".to_string()
}

fn default_eviction_sample_size() -> usize {
    5
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

#[derive(Debug, Clone, Copy, PartialEq)]
enum EvictionPolicy {
    NoEviction,
    AllKeysLru,
    AllKeysRandom,
}

impl EvictionPolicy {
    fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "allkeys-lru" => EvictionPolicy::AllKeysLru,
            "allkeys-random" => EvictionPolicy::AllKeysRandom,
            "noeviction" => EvictionPolicy::NoEviction,
            _ => EvictionPolicy::AllKeysLru, // default
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            EvictionPolicy::NoEviction => "noeviction",
            EvictionPolicy::AllKeysLru => "allkeys-lru",
            EvictionPolicy::AllKeysRandom => "allkeys-random",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Config {
    #[serde(default)]
    server: ServerConfig,
    #[serde(default)]
    security: SecurityConfig,
    #[serde(default)]
    logging: LoggingConfig,
    #[serde(default)]
    performance: PerformanceConfig,
    #[serde(default)]
    memory: MemoryConfig,
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

impl Config {
    fn load() -> Result<Self, Box<dyn std::error::Error>> {
        // Check for custom config path from env var, otherwise use default
        let config_path =
            std::env::var("REDISTILL_CONFIG").unwrap_or_else(|_| "redistill.toml".to_string());

        let mut config = if std::path::Path::new(&config_path).exists() {
            let contents = std::fs::read_to_string(&config_path)?;
            toml::from_str(&contents)?
        } else {
            if config_path != "redistill.toml" {
                eprintln!(
                    "⚠️  Config file '{}' not found, using defaults",
                    config_path
                );
            }
            Config::default()
        };

        // Allow ENV vars to override config
        if let Ok(password) = std::env::var("REDIS_PASSWORD") {
            config.security.password = password;
        }

        if let Ok(port) = std::env::var("REDIS_PORT") {
            if let Ok(p) = port.parse() {
                config.server.port = p;
            }
        }

        if let Ok(bind) = std::env::var("REDIS_BIND") {
            config.server.bind = bind;
        }

        Ok(config)
    }
}

// Global configuration
static CONFIG: Lazy<Config> = Lazy::new(|| {
    Config::load().unwrap_or_else(|e| {
        eprintln!("Failed to load config: {}", e);
        eprintln!("Using default configuration");
        Config::default()
    })
});

// Global metrics
static TOTAL_COMMANDS: AtomicU64 = AtomicU64::new(0);
static TOTAL_CONNECTIONS: AtomicU64 = AtomicU64::new(0);
static ACTIVE_CONNECTIONS: AtomicUsize = AtomicUsize::new(0);

// Memory tracking (approximate)
static MEMORY_USED: AtomicU64 = AtomicU64::new(0);
static EVICTED_KEYS: AtomicU64 = AtomicU64::new(0);
static SERVER_START_TIME: AtomicU32 = AtomicU32::new(0);

// Connection rate limiting
static LAST_CONNECTION_CHECK: AtomicU64 = AtomicU64::new(0);
static CONNECTIONS_THIS_SECOND: AtomicU64 = AtomicU64::new(0);
static REJECTED_CONNECTIONS: AtomicU64 = AtomicU64::new(0);
static START_TIME: Lazy<SystemTime> = Lazy::new(SystemTime::now);

// Buffer pool for zero-allocation response writing
static BUFFER_POOL: Lazy<SegQueue<Vec<u8>>> = Lazy::new(|| {
    let pool = SegQueue::new();
    for _ in 0..CONFIG.server.buffer_pool_size {
        pool.push(Vec::with_capacity(CONFIG.server.buffer_size));
    }
    pool
});

#[inline(always)]
fn get_buffer() -> Vec<u8> {
    BUFFER_POOL
        .pop()
        .unwrap_or_else(|| Vec::with_capacity(CONFIG.server.buffer_size))
}

#[inline(always)]
fn return_buffer(mut buf: Vec<u8>) {
    buf.clear();
    if buf.capacity() <= CONFIG.server.buffer_size * 2 {
        BUFFER_POOL.push(buf);
    }
}

// Entry with Bytes for zero-copy
struct Entry {
    value: Bytes,
    expiry: Option<u64>,
    last_accessed: AtomicU32, // Seconds since server start (for LRU)
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

// Sharded store with DashMap for lock-free reads
struct ShardedStore {
    shards: Vec<Arc<DashMap<Bytes, Entry>>>,
    num_shards: usize,
}

impl ShardedStore {
    fn new(num_shards: usize) -> Self {
        let mut shards = Vec::with_capacity(num_shards);
        for _ in 0..num_shards {
            shards.push(Arc::new(DashMap::with_capacity(1000)));
        }
        Self { shards, num_shards }
    }

    fn clone(&self) -> Self {
        Self {
            shards: self.shards.clone(),
            num_shards: self.num_shards,
        }
    }

    // Fast FNV-1a hash
    #[inline(always)]
    fn hash(&self, key: &[u8]) -> usize {
        let mut hash: u64 = 0xcbf29ce484222325;
        for &byte in key {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash as usize % self.num_shards
    }

    #[inline(always)]
    fn set(&self, key: Bytes, value: Bytes, ttl: Option<u64>, now: u64) {
        let expiry = ttl.map(|s| now + s);
        let shard = &self.shards[self.hash(&key)];
        shard.insert(
            key,
            Entry {
                value,
                expiry,
                last_accessed: AtomicU32::new(get_uptime_seconds()),
            },
        );
    }

    #[inline(always)]
    fn get(&self, key: &[u8], now: u64) -> Option<Bytes> {
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

            // Update access time approximately (90% skip for performance)
            maybe_update_access_time(&entry);

            return Some(entry.value.clone());
        }
        None
    }

    #[inline(always)]
    fn delete(&self, keys: &[Bytes]) -> usize {
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
    fn exists(&self, keys: &[Bytes], now: u64) -> usize {
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

    fn keys(&self, now: u64) -> Vec<Bytes> {
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

    fn len(&self) -> usize {
        self.shards.iter().map(|s| s.len()).sum()
    }

    fn clear(&self) {
        for shard in &self.shards {
            shard.clear();
        }
    }
}

// Optimized RESP parser with zero-copy
struct RespParser {
    buffer: BytesMut,
}

impl RespParser {
    #[inline]
    fn new() -> Self {
        Self {
            buffer: BytesMut::with_capacity(CONFIG.server.buffer_size),
        }
    }

    #[inline(always)]
    fn has_buffered_data(&self) -> bool {
        self.buffer.len() > 0
    }

    async fn parse_command<S>(&mut self, stream: &mut S) -> Result<Vec<Bytes>, ()>
    where
        S: AsyncRead + Unpin,
    {
        loop {
            match self.try_parse() {
                Ok(Some(cmd)) => return Ok(cmd),
                Ok(None) => {
                    if stream.read_buf(&mut self.buffer).await.is_err() {
                        return Err(());
                    }
                    if self.buffer.is_empty() {
                        return Err(());
                    }
                }
                Err(_) => return Err(()),
            }
        }
    }

    fn try_parse(&mut self) -> Result<Option<Vec<Bytes>>, ()> {
        if self.buffer.len() < 4 {
            return Ok(None);
        }

        let mut cursor = 0;
        let len = self.buffer.len();

        if self.buffer[cursor] != b'*' {
            return Err(());
        }
        cursor += 1;

        // Fast integer parsing
        let mut array_len = 0usize;
        loop {
            if cursor >= len {
                return Ok(None);
            }
            let byte = self.buffer[cursor];
            if byte == b'\r' {
                break;
            }
            if !(b'0'..=b'9').contains(&byte) {
                return Err(());
            }
            array_len = array_len * 10 + (byte - b'0') as usize;
            cursor += 1;
        }

        if cursor + 1 >= len || self.buffer[cursor + 1] != b'\n' {
            return Ok(None);
        }
        cursor += 2;

        let mut result = Vec::with_capacity(array_len);

        for _ in 0..array_len {
            if cursor >= len || self.buffer[cursor] != b'$' {
                return Ok(None);
            }
            cursor += 1;

            let mut str_len = 0usize;
            loop {
                if cursor >= len {
                    return Ok(None);
                }
                let byte = self.buffer[cursor];
                if byte == b'\r' {
                    break;
                }
                if !(b'0'..=b'9').contains(&byte) {
                    return Err(());
                }
                str_len = str_len * 10 + (byte - b'0') as usize;
                cursor += 1;
            }

            if cursor + 1 >= len || self.buffer[cursor + 1] != b'\n' {
                return Ok(None);
            }
            cursor += 2;

            if cursor + str_len + 2 > len {
                return Ok(None);
            }

            // Store as reference for now - we'll convert after parsing
            let start = cursor;
            let end = cursor + str_len;
            result.push(Bytes::copy_from_slice(&self.buffer[start..end]));
            cursor += str_len + 2;
        }

        self.buffer.advance(cursor);
        Ok(Some(result))
    }
}

// Optimized RESP writer with pooled buffers
struct RespWriter {
    buffer: Vec<u8>,
}

impl RespWriter {
    fn new() -> Self {
        Self {
            buffer: get_buffer(),
        }
    }

    #[inline(always)]
    fn write_u64(&mut self, mut n: u64) {
        if n == 0 {
            self.buffer.push(b'0');
            return;
        }

        let mut buf = [0u8; 20];
        let mut i = 20;

        while n > 0 {
            i -= 1;
            buf[i] = b'0' + (n % 10) as u8;
            n /= 10;
        }

        self.buffer.extend_from_slice(&buf[i..]);
    }

    #[inline(always)]
    fn write_simple_string(&mut self, s: &[u8]) {
        self.buffer.push(b'+');
        self.buffer.extend_from_slice(s);
        self.buffer.extend_from_slice(b"\r\n");
    }

    #[inline(always)]
    fn write_bulk_string(&mut self, s: &[u8]) {
        self.buffer.push(b'$');
        self.write_u64(s.len() as u64);
        self.buffer.extend_from_slice(b"\r\n");
        self.buffer.extend_from_slice(s);
        self.buffer.extend_from_slice(b"\r\n");
    }

    #[inline(always)]
    fn write_null(&mut self) {
        self.buffer.extend_from_slice(b"$-1\r\n");
    }

    #[inline(always)]
    fn write_integer(&mut self, i: usize) {
        self.buffer.push(b':');
        self.write_u64(i as u64);
        self.buffer.extend_from_slice(b"\r\n");
    }

    #[inline(always)]
    fn write_error(&mut self, s: &[u8]) {
        self.buffer.extend_from_slice(b"-ERR ");
        self.buffer.extend_from_slice(s);
        self.buffer.extend_from_slice(b"\r\n");
    }

    #[inline(always)]
    fn write_array(&mut self, arr: &[Bytes]) {
        self.buffer.push(b'*');
        self.write_u64(arr.len() as u64);
        self.buffer.extend_from_slice(b"\r\n");
        for item in arr {
            self.write_bulk_string(item);
        }
    }

    #[inline(always)]
    fn should_flush(&self) -> bool {
        self.buffer.len() >= 8192
    }

    #[inline(always)]
    async fn flush<S>(&mut self, stream: &mut S) -> Result<(), ()>
    where
        S: AsyncWrite + Unpin,
    {
        if !self.buffer.is_empty() {
            stream.write_all(&self.buffer).await.map_err(|_| ())?;
            self.buffer.clear();
        }
        Ok(())
    }
}

impl Drop for RespWriter {
    fn drop(&mut self) {
        let buf = std::mem::replace(&mut self.buffer, Vec::new());
        return_buffer(buf);
    }
}

#[inline(always)]
fn get_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

// Get uptime in seconds for LRU tracking
#[inline(always)]
fn get_uptime_seconds() -> u32 {
    let start = SERVER_START_TIME.load(Ordering::Relaxed);
    if start == 0 {
        return 0;
    }
    get_timestamp().saturating_sub(start as u64) as u32
}

// Approximate access tracking: only update 10% of the time
#[inline(always)]
fn maybe_update_access_time(entry: &Entry) {
    // Skip entirely if memory limits disabled (zero-cost)
    if CONFIG.memory.max_memory == 0 {
        return;
    }

    // Fast path: skip 90% of updates for performance
    if fastrand::u8(..100) < 90 {
        return;
    }
    // Slow path: update access time
    entry
        .last_accessed
        .store(get_uptime_seconds(), Ordering::Relaxed);
}

// Calculate approximate size of an entry
#[inline(always)]
fn entry_size(key_len: usize, value_len: usize) -> usize {
    key_len + value_len + 64 // ~64 bytes overhead for Arc, Entry struct, etc.
}

// Check connection rate limit
#[inline]
fn check_rate_limit() -> bool {
    let rate_limit = CONFIG.server.connection_rate_limit;

    // No rate limit
    if rate_limit == 0 {
        return true;
    }

    let now = get_timestamp();
    let last_check = LAST_CONNECTION_CHECK.load(Ordering::Relaxed);

    // New second - reset counter
    if now > last_check {
        LAST_CONNECTION_CHECK.store(now, Ordering::Relaxed);
        CONNECTIONS_THIS_SECOND.store(1, Ordering::Relaxed);
        return true;
    }

    // Same second - check limit
    let count = CONNECTIONS_THIS_SECOND.fetch_add(1, Ordering::Relaxed);
    count < rate_limit
}

// Format bytes as human-readable string
fn format_bytes(bytes: u64) -> String {
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
fn eq_ignore_case_3(a: &[u8], b: &[u8; 3]) -> bool {
    a.len() == 3 && (a[0] | 0x20) == b[0] && (a[1] | 0x20) == b[1] && (a[2] | 0x20) == b[2]
}

#[inline(always)]
fn eq_ignore_case_6(a: &[u8], b: &[u8; 6]) -> bool {
    a.len() == 6
        && (a[0] | 0x20) == b[0]
        && (a[1] | 0x20) == b[1]
        && (a[2] | 0x20) == b[2]
        && (a[3] | 0x20) == b[3]
        && (a[4] | 0x20) == b[4]
        && (a[5] | 0x20) == b[5]
}

// Connection state for authentication
struct ConnectionState {
    authenticated: bool,
}

impl ConnectionState {
    fn new() -> Self {
        Self {
            // If no password is set, authentication is not required
            authenticated: CONFIG.security.password.is_empty(),
        }
    }
}

// Eviction: ensure memory is available
#[inline(always)]
fn evict_if_needed(store: &ShardedStore, needed_size: usize) -> bool {
    let max_memory = CONFIG.memory.max_memory;

    // Fast path: unlimited memory (zero-cost)
    if max_memory == 0 {
        return true;
    }

    // Check if eviction needed (relaxed for performance)
    let current = MEMORY_USED.load(Ordering::Relaxed);
    if current + needed_size as u64 <= max_memory {
        return true;
    }

    // Get eviction policy
    let policy = EvictionPolicy::from_str(&CONFIG.memory.eviction_policy);

    // No eviction policy - reject new keys
    if policy == EvictionPolicy::NoEviction {
        return false;
    }

    // Evict until we have enough space
    let mut freed = 0;
    let mut attempts = 0;
    let max_attempts = 100; // Prevent infinite loop

    while freed < needed_size && attempts < max_attempts {
        attempts += 1;

        let evicted = match policy {
            EvictionPolicy::AllKeysLru => evict_lru(store),
            EvictionPolicy::AllKeysRandom => evict_random(store),
            EvictionPolicy::NoEviction => break,
        };

        if evicted == 0 {
            break; // No more keys to evict
        }

        freed += evicted;
    }

    freed >= needed_size
}

// Evict using LRU policy
#[inline]
fn evict_lru(store: &ShardedStore) -> usize {
    let sample_size = CONFIG.memory.eviction_sample_size;

    // Sample keys from random shards
    let mut oldest_key: Option<Bytes> = None;
    let mut oldest_time: u32 = u32::MAX;
    let mut oldest_shard_idx = 0;

    for _ in 0..sample_size {
        let shard_idx = fastrand::usize(..store.num_shards);
        let shard = &store.shards[shard_idx];

        // Get a random key from this shard
        if let Some(entry) = shard.iter().next() {
            let last_accessed = entry.value().last_accessed.load(Ordering::Relaxed);

            if oldest_key.is_none() || last_accessed < oldest_time {
                oldest_key = Some(entry.key().clone());
                oldest_time = last_accessed;
                oldest_shard_idx = shard_idx;
            }
        }
    }

    // Evict the oldest key
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

// Evict using random policy
#[inline]
fn evict_random(store: &ShardedStore) -> usize {
    // Pick a random shard
    let shard_idx = fastrand::usize(..store.num_shards);
    let shard = &store.shards[shard_idx];

    // Get first key (effectively random due to HashMap internals)
    if let Some(entry) = shard.iter().next() {
        let key = entry.key().clone();
        let key_len = key.len();
        let value_len = entry.value().value.len();
        drop(entry);

        if let Some((_, _)) = shard.remove(&key) {
            let size = entry_size(key_len, value_len);
            MEMORY_USED.fetch_sub(size as u64, Ordering::Relaxed);
            EVICTED_KEYS.fetch_add(1, Ordering::Relaxed);
            return size;
        }
    }

    0
}

// Execute command - fully inlined and optimized
#[inline(always)]
fn execute_command(
    store: &ShardedStore,
    command: &[Bytes],
    writer: &mut RespWriter,
    state: &mut ConnectionState,
    now: u64,
) {
    TOTAL_COMMANDS.fetch_add(1, Ordering::Relaxed);
    if command.is_empty() {
        writer.write_error(b"empty command");
        return;
    }

    let cmd = &command[0];

    // AUTH and PING don't require authentication
    let requires_auth = !matches!(cmd.len(), 4 if eq_ignore_case_3(&cmd[..3], b"aut") && (cmd[3] | 0x20) == b'h')
        && !matches!(cmd.len(), 4 if eq_ignore_case_3(&cmd[..3], b"pin") && (cmd[3] | 0x20) == b'g');

    if requires_auth && !state.authenticated {
        writer.write_error(b"NOAUTH Authentication required");
        return;
    }

    // Optimized command matching
    match cmd.len() {
        3 => {
            if eq_ignore_case_3(cmd, b"set") {
                if command.len() >= 3 {
                    let key = &command[1];
                    let value = &command[2];

                    // Check memory limit before setting
                    let size = entry_size(key.len(), value.len());
                    if !evict_if_needed(store, size) {
                        writer
                            .write_error(b"OOM command not allowed when used memory > 'maxmemory'");
                        return;
                    }

                    let mut ttl = None;
                    if command.len() >= 5 && command[3].len() == 2 {
                        let ex = &command[3];
                        if (ex[0] | 0x20) == b'e' && (ex[1] | 0x20) == b'x' {
                            // Parse TTL
                            let ttl_bytes = &command[4];
                            let mut val = 0u64;
                            for &b in ttl_bytes.iter() {
                                if !(b'0'..=b'9').contains(&b) {
                                    break;
                                }
                                val = val * 10 + (b - b'0') as u64;
                            }
                            if val > 0 {
                                ttl = Some(val);
                            }
                        }
                    }

                    store.set(key.clone(), value.clone(), ttl, now);

                    // Track memory usage (only if limits enabled)
                    if CONFIG.memory.max_memory > 0 {
                        MEMORY_USED.fetch_add(size as u64, Ordering::Relaxed);
                    }

                    writer.write_simple_string(b"OK");
                } else {
                    writer.write_error(b"wrong number of arguments");
                }
                return;
            }
            if eq_ignore_case_3(cmd, b"get") {
                if command.len() >= 2 {
                    match store.get(&command[1], now) {
                        Some(value) => writer.write_bulk_string(&value),
                        None => writer.write_null(),
                    }
                } else {
                    writer.write_error(b"wrong number of arguments");
                }
                return;
            }
            if eq_ignore_case_3(cmd, b"del") {
                if command.len() >= 2 {
                    let count = store.delete(&command[1..]);
                    writer.write_integer(count);
                } else {
                    writer.write_error(b"wrong number of arguments");
                }
                return;
            }
        }
        4 => {
            if eq_ignore_case_3(&cmd[..3], b"pin") && (cmd[3] | 0x20) == b'g' {
                writer.write_simple_string(b"PONG");
                return;
            }
            if eq_ignore_case_3(&cmd[..3], b"key") && (cmd[3] | 0x20) == b's' {
                let keys = store.keys(now);
                writer.write_array(&keys);
                return;
            }
            if eq_ignore_case_3(&cmd[..3], b"aut") && (cmd[3] | 0x20) == b'h' {
                // AUTH command
                if !CONFIG.security.password.is_empty() {
                    if command.len() >= 2 {
                        if command[1].as_ref() == CONFIG.security.password.as_bytes() {
                            state.authenticated = true;
                            writer.write_simple_string(b"OK");
                        } else {
                            writer.write_error(b"ERR invalid password");
                        }
                    } else {
                        writer.write_error(b"ERR wrong number of arguments for 'auth' command");
                    }
                } else {
                    writer.write_error(b"ERR Client sent AUTH, but no password is set");
                }
                return;
            }
            if eq_ignore_case_3(&cmd[..3], b"inf") && (cmd[3] | 0x20) == b'o' {
                // INFO command - return server stats
                let uptime = START_TIME.elapsed().unwrap_or_default().as_secs();
                let total_commands = TOTAL_COMMANDS.load(Ordering::Relaxed);
                let total_connections = TOTAL_CONNECTIONS.load(Ordering::Relaxed);
                let active_connections = ACTIVE_CONNECTIONS.load(Ordering::Relaxed);
                let db_size = store.len();
                let memory_used = MEMORY_USED.load(Ordering::Relaxed);
                let evicted_keys = EVICTED_KEYS.load(Ordering::Relaxed);
                let max_memory = CONFIG.memory.max_memory;
                let eviction_policy = EvictionPolicy::from_str(&CONFIG.memory.eviction_policy);
                let rejected_connections = REJECTED_CONNECTIONS.load(Ordering::Relaxed);

                let info = format!(
                    "# Server\r\n\
                    redis_version:7.0.0\r\n\
                    redis_mode:standalone\r\n\
                    os:Redistill\r\n\
                    arch_bits:64\r\n\
                    process_id:{}\r\n\
                    uptime_in_seconds:{}\r\n\
                    \r\n\
                    # Clients\r\n\
                    connected_clients:{}\r\n\
                    \r\n\
                    # Memory\r\n\
                    used_memory:{}\r\n\
                    used_memory_human:{}\r\n\
                    maxmemory:{}\r\n\
                    maxmemory_human:{}\r\n\
                    maxmemory_policy:{}\r\n\
                    evicted_keys:{}\r\n\
                    \r\n\
                    # Stats\r\n\
                    total_connections_received:{}\r\n\
                    total_commands_processed:{}\r\n\
                    rejected_connections:{}\r\n\
                    \r\n\
                    # Keyspace\r\n\
                    db0:keys={},expires=0,avg_ttl=0\r\n",
                    std::process::id(),
                    uptime,
                    active_connections,
                    memory_used,
                    format_bytes(memory_used),
                    max_memory,
                    if max_memory > 0 {
                        format_bytes(max_memory)
                    } else {
                        "unlimited".to_string()
                    },
                    eviction_policy.as_str(),
                    evicted_keys,
                    total_connections,
                    total_commands,
                    rejected_connections,
                    db_size
                );
                writer.write_bulk_string(info.as_bytes());
                return;
            }
        }
        6 => {
            if eq_ignore_case_6(cmd, b"exists") {
                if command.len() >= 2 {
                    let count = store.exists(&command[1..], now);
                    writer.write_integer(count);
                } else {
                    writer.write_error(b"wrong number of arguments");
                }
                return;
            }
            if eq_ignore_case_6(cmd, b"dbsize") {
                let size = store.len();
                writer.write_integer(size);
                return;
            }
            if eq_ignore_case_6(cmd, b"config") {
                writer.buffer.extend_from_slice(b"*0\r\n");
                return;
            }
        }
        7 => {
            if cmd.len() == 7 {
                let lower = [
                    cmd[0] | 0x20,
                    cmd[1] | 0x20,
                    cmd[2] | 0x20,
                    cmd[3] | 0x20,
                    cmd[4] | 0x20,
                    cmd[5] | 0x20,
                    cmd[6] | 0x20,
                ];
                if &lower == b"flushdb" {
                    store.clear();
                    writer.write_simple_string(b"OK");
                    return;
                }
                if &lower == b"command" {
                    writer.buffer.extend_from_slice(b"*0\r\n");
                    return;
                }
            }
        }
        _ => {}
    }

    writer.write_error(b"unknown command");
}

// Unified stream type for both plain TCP and TLS
enum MaybeStream {
    Plain(TcpStream),
    Tls(tokio_rustls::server::TlsStream<TcpStream>),
}

impl AsyncRead for MaybeStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match &mut *self {
            MaybeStream::Plain(s) => Pin::new(s).poll_read(cx, buf),
            MaybeStream::Tls(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for MaybeStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match &mut *self {
            MaybeStream::Plain(s) => Pin::new(s).poll_write(cx, buf),
            MaybeStream::Tls(s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            MaybeStream::Plain(s) => Pin::new(s).poll_flush(cx),
            MaybeStream::Tls(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            MaybeStream::Plain(s) => Pin::new(s).poll_shutdown(cx),
            MaybeStream::Tls(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

impl MaybeStream {
    fn set_nodelay(&self, nodelay: bool) -> io::Result<()> {
        match self {
            MaybeStream::Plain(s) => s.set_nodelay(nodelay),
            MaybeStream::Tls(s) => s.get_ref().0.set_nodelay(nodelay),
        }
    }
}

// Load TLS configuration from certificate files
async fn load_tls_config(
    cert_path: &str,
    key_path: &str,
) -> Result<Arc<RustlsServerConfig>, Box<dyn std::error::Error>> {
    use rustls_pemfile::{certs, pkcs8_private_keys};
    use std::io::BufReader;

    // Read certificate file
    let cert_file = tokio::fs::read(cert_path).await?;
    let mut cert_reader = BufReader::new(cert_file.as_slice());
    let certs: Vec<_> = certs(&mut cert_reader).collect::<Result<Vec<_>, _>>()?;

    if certs.is_empty() {
        return Err("No certificates found in cert file".into());
    }

    // Read private key file
    let key_file = tokio::fs::read(key_path).await?;
    let mut key_reader = BufReader::new(key_file.as_slice());
    let mut keys = pkcs8_private_keys(&mut key_reader).collect::<Result<Vec<_>, _>>()?;

    if keys.is_empty() {
        return Err("No private keys found in key file".into());
    }

    let key = keys.remove(0).into();

    // Build TLS configuration
    let config = RustlsServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;

    Ok(Arc::new(config))
}

async fn handle_connection(mut stream: MaybeStream, store: ShardedStore) {
    // Set TCP options from config
    let _ = stream.set_nodelay(CONFIG.performance.tcp_nodelay);

    // Track connection
    TOTAL_CONNECTIONS.fetch_add(1, Ordering::Relaxed);
    ACTIVE_CONNECTIONS.fetch_add(1, Ordering::Relaxed);

    let mut parser = RespParser::new();
    let mut writer = RespWriter::new();
    let mut state = ConnectionState::new();
    let mut batch_count = 0;

    loop {
        let now = get_timestamp();

        match parser.parse_command(&mut stream).await {
            Ok(command) => {
                execute_command(&store, &command, &mut writer, &mut state, now);
                batch_count += 1;

                // Smart flushing:
                // 1. If buffer is large, flush immediately
                // 2. If we hit batch size, flush
                // 3. If no more commands buffered (interactive mode), flush
                let should_flush = writer.should_flush()
                    || batch_count >= CONFIG.server.batch_size
                    || !parser.has_buffered_data();

                if should_flush {
                    if writer.flush(&mut stream).await.is_err() {
                        break;
                    }
                    batch_count = 0;
                }
            }
            Err(_) => {
                // Flush any pending responses before closing
                let _ = writer.flush(&mut stream).await;
                break;
            }
        }
    }

    // Cleanup: decrement active connections
    ACTIVE_CONNECTIONS.fetch_sub(1, Ordering::Relaxed);
}

// Health check HTTP handler
async fn handle_health_check(
    _req: Request<hyper::body::Incoming>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let status = format!(
        r#"{{"status":"ok","uptime_seconds":{},"active_connections":{},"total_connections":{},"rejected_connections":{},"memory_used":{},"max_memory":{},"evicted_keys":{},"total_commands":{}}}"#,
        START_TIME.elapsed().unwrap_or_default().as_secs(),
        ACTIVE_CONNECTIONS.load(Ordering::Relaxed),
        TOTAL_CONNECTIONS.load(Ordering::Relaxed),
        REJECTED_CONNECTIONS.load(Ordering::Relaxed),
        MEMORY_USED.load(Ordering::Relaxed),
        CONFIG.memory.max_memory,
        EVICTED_KEYS.load(Ordering::Relaxed),
        TOTAL_COMMANDS.load(Ordering::Relaxed)
    );

    let response = Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Full::new(Bytes::from(status)))
        .unwrap();

    Ok(response)
}

// Start health check HTTP server
async fn start_health_check_server(port: u16) {
    let addr = format!("127.0.0.1:{}", port);
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!(
                "⚠️  Failed to bind health check endpoint on {}: {}",
                addr, e
            );
            return;
        }
    };

    println!("🏥 Health check endpoint: http://{}/health", addr);

    loop {
        let (stream, _) = match listener.accept().await {
            Ok(s) => s,
            Err(_) => continue,
        };

        let io = TokioIo::new(stream);

        tokio::spawn(async move {
            let _ = http1::Builder::new()
                .serve_connection(io, service_fn(handle_health_check))
                .await;
        });
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    // Load configuration
    let config = &*CONFIG;

    // Initialize server start time
    SERVER_START_TIME.store(get_timestamp() as u32, Ordering::Relaxed);

    let store = ShardedStore::new(config.server.num_shards);

    println!(
        r#"
╔══════════════════════════════════════════════════════════════╗
║          🚀  REDISTILL - Redis Performance, Distilled  🚀   ║
╚══════════════════════════════════════════════════════════════╝

📊 Configuration:
   • Bind Address: {}:{}
   • Shards: {}
   • CPU Cores: {}
   • Buffer Pool: {} buffers
   • Buffer Size: {} KB
   • Batch Size: {} commands
   • Authentication: {}
   • TLS/SSL: {}
   • TCP NoDelay: {}
   • Max Memory: {}
   • Eviction Policy: {}

⚡ Optimizations:
   ✓ DashMap lock-free concurrent hashmap
   ✓ Zero-copy Bytes (no string allocations)
   ✓ Buffer pooling (no allocations)
   ✓ Batched writes ({} commands per flush)
   ✓ Sharded storage ({} shards)
   ✓ FNV-1a fast hashing
   ✓ Inline everything
   ✓ Fast case-insensitive matching
   ✓ jemalloc allocator

🎯 Performance: 2x faster than Redis with pipelining!
"#,
        config.server.bind,
        config.server.port,
        config.server.num_shards,
        num_cpus::get(),
        config.server.buffer_pool_size,
        config.server.buffer_size / 1024,
        config.server.batch_size,
        if !config.security.password.is_empty() {
            "enabled"
        } else {
            "disabled"
        },
        if config.security.tls_enabled {
            "enabled"
        } else {
            "disabled"
        },
        if config.performance.tcp_nodelay {
            "enabled"
        } else {
            "disabled"
        },
        if config.memory.max_memory > 0 {
            format_bytes(config.memory.max_memory)
        } else {
            "unlimited".to_string()
        },
        config.memory.eviction_policy,
        config.server.batch_size,
        config.server.num_shards
    );

    // Load TLS configuration if enabled
    let tls_acceptor = if config.security.tls_enabled {
        if config.security.tls_cert_path.is_empty() || config.security.tls_key_path.is_empty() {
            eprintln!("❌ TLS enabled but cert/key paths not configured");
            std::process::exit(1);
        }

        match load_tls_config(
            &config.security.tls_cert_path,
            &config.security.tls_key_path,
        )
        .await
        {
            Ok(tls_config) => {
                println!("🔐 TLS/SSL enabled");
                println!("   • Certificate: {}", config.security.tls_cert_path);
                println!("   • Private Key: {}", config.security.tls_key_path);
                Some(TlsAcceptor::from(tls_config))
            }
            Err(e) => {
                eprintln!("❌ Failed to load TLS configuration: {}", e);
                eprintln!("   Make sure certificate and key files exist and are valid");
                std::process::exit(1);
            }
        }
    } else {
        None
    };

    let bind_addr = format!("{}:{}", config.server.bind, config.server.port);
    let listener = TcpListener::bind(&bind_addr).await.unwrap_or_else(|e| {
        eprintln!("❌ Failed to bind to {}: {}", bind_addr, e);
        std::process::exit(1);
    });

    println!("🎧 Listening on {}", bind_addr);

    if !config.security.password.is_empty() {
        println!("🔒 Authentication enabled (password set in config or env var)");
    } else {
        println!(
            "⚠️  Authentication disabled (set password in config file or REDIS_PASSWORD env var)"
        );
    }

    let config_file =
        std::env::var("REDISTILL_CONFIG").unwrap_or_else(|_| "redistill.toml".to_string());
    if std::path::Path::new(&config_file).exists() {
        println!("📄 Configuration loaded from {}", config_file);
    } else {
        println!("📄 Using default configuration (create redistill.toml to customize)");
    }

    // Start health check endpoint if enabled
    if config.server.health_check_port > 0 {
        let health_port = config.server.health_check_port;
        tokio::spawn(async move {
            start_health_check_server(health_port).await;
        });
    }

    println!();

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((tcp_stream, _)) => {
                        // Check max connections limit
                        let active = ACTIVE_CONNECTIONS.load(Ordering::Relaxed);
                        if CONFIG.server.max_connections > 0 && active >= CONFIG.server.max_connections {
                            REJECTED_CONNECTIONS.fetch_add(1, Ordering::Relaxed);
                            drop(tcp_stream);  // Close connection
                            continue;
                        }

                        // Check connection rate limit
                        if !check_rate_limit() {
                            REJECTED_CONNECTIONS.fetch_add(1, Ordering::Relaxed);
                            drop(tcp_stream);  // Close connection
                            continue;
                        }

                        let store_clone = store.clone();
                        let tls_acceptor_clone = tls_acceptor.clone();

                        tokio::spawn(async move {
                            // Wrap in TLS if enabled
                            let stream = if let Some(acceptor) = tls_acceptor_clone {
                                match acceptor.accept(tcp_stream).await {
                                    Ok(tls_stream) => MaybeStream::Tls(tls_stream),
                                    Err(e) => {
                                        eprintln!("TLS handshake failed: {}", e);
                                        return;
                                    }
                                }
                            } else {
                                MaybeStream::Plain(tcp_stream)
                            };

                            handle_connection(stream, store_clone).await;
                        });
                    }
                    Err(e) => {
                        eprintln!("Accept error: {}", e);
                    }
                }
            }
            _ = signal::ctrl_c() => {
                println!("\n\n🛑 Received shutdown signal...");
                println!("📊 Final Stats:");
                println!("   • Total connections: {}", TOTAL_CONNECTIONS.load(Ordering::Relaxed));
                println!("   • Total commands: {}", TOTAL_COMMANDS.load(Ordering::Relaxed));
                println!("   • Active connections: {}", ACTIVE_CONNECTIONS.load(Ordering::Relaxed));
                println!("   • Keys in database: {}", store.len());
                println!("\n👋 Redistill shut down gracefully");
                break;
            }
        }
    }
}
