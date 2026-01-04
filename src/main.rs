// Global allocator - jemalloc for performance
#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: jemallocator::Jemalloc = jemallocator::Jemalloc;

use ahash::AHasher;
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
use std::hash::Hasher;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::signal;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig as RustlsServerConfig;
use subtle::ConstantTimeEq;

// Security limits for RESP protocol parsing (prevent DoS attacks)
const MAX_ARRAY_LEN: usize = 1_000_000;      // Max 1M commands in array
const MAX_STRING_LEN: usize = 512_000_000;   // Max 512MB per string (Redis default)
const MAX_BUFFER_SIZE: usize = 1_073_741_824; // Max 1GB buffer per connection (DoS protection)

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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistenceConfig {
    #[serde(default)]
    enabled: bool,
    #[serde(default = "default_snapshot_path")]
    snapshot_path: String,
    #[serde(default = "default_true")]
    save_on_shutdown: bool,
    #[serde(default = "default_snapshot_interval")]
    snapshot_interval: u64, // seconds, 0 = disabled
}

fn default_snapshot_path() -> String {
    "redistill.rdb".to_string()
}

fn default_snapshot_interval() -> u64 {
    300 // 5 minutes
}

impl Default for PersistenceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            snapshot_path: default_snapshot_path(),
            save_on_shutdown: true,
            snapshot_interval: default_snapshot_interval(),
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
    #[serde(default)]
    persistence: PersistenceConfig,
}

// Default functions
fn default_bind() -> String {
    "0.0.0.0".to_string()
}
fn default_port() -> u16 {
    6379
}
fn default_num_shards() -> usize {
    2048  // Optimized for extreme concurrency - best balance of performance and memory
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
            persistence: PersistenceConfig::default(),
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
                    "Config file '{}' not found, using defaults",
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

        if let Ok(health_port) = std::env::var("REDIS_HEALTH_CHECK_PORT") {
            if let Ok(p) = health_port.parse() {
                config.server.health_check_port = p;
            }
        }

        // Performance tuning via environment variables
        if let Ok(num_shards) = std::env::var("REDIS_NUM_SHARDS") {
            if let Ok(n) = num_shards.parse() {
                config.server.num_shards = n;
            }
        }

        if let Ok(batch_size) = std::env::var("REDIS_BATCH_SIZE") {
            if let Ok(b) = batch_size.parse() {
                config.server.batch_size = b;
            }
        }

        if let Ok(buffer_size) = std::env::var("REDIS_BUFFER_SIZE") {
            if let Ok(b) = buffer_size.parse() {
                config.server.buffer_size = b;
            }
        }

        if let Ok(buffer_pool_size) = std::env::var("REDIS_BUFFER_POOL_SIZE") {
            if let Ok(b) = buffer_pool_size.parse() {
                config.server.buffer_pool_size = b;
            }
        }

        if let Ok(max_connections) = std::env::var("REDIS_MAX_CONNECTIONS") {
            if let Ok(m) = max_connections.parse() {
                config.server.max_connections = m;
            }
        }

        // Memory management via environment variables
        if let Ok(max_memory) = std::env::var("REDIS_MAX_MEMORY") {
            if let Ok(m) = max_memory.parse() {
                config.memory.max_memory = m;
            }
        }

        if let Ok(eviction_policy) = std::env::var("REDIS_EVICTION_POLICY") {
            config.memory.eviction_policy = eviction_policy;
        }

        // Performance settings via environment variables
        if let Ok(tcp_nodelay) = std::env::var("REDIS_TCP_NODELAY") {
            config.performance.tcp_nodelay = tcp_nodelay.parse().unwrap_or(true);
        }

        if let Ok(tcp_keepalive) = std::env::var("REDIS_TCP_KEEPALIVE") {
            if let Ok(k) = tcp_keepalive.parse() {
                config.performance.tcp_keepalive = k;
            }
        }

        // Persistence settings via environment variables
        if let Ok(enabled) = std::env::var("REDIS_PERSISTENCE_ENABLED") {
            config.persistence.enabled = enabled.parse().unwrap_or(false);
        }

        if let Ok(path) = std::env::var("REDIS_SNAPSHOT_PATH") {
            config.persistence.snapshot_path = path;
        }

        if let Ok(interval) = std::env::var("REDIS_SNAPSHOT_INTERVAL") {
            if let Ok(i) = interval.parse() {
                config.persistence.snapshot_interval = i;
            }
        }

        if let Ok(save_on_shutdown) = std::env::var("REDIS_SAVE_ON_SHUTDOWN") {
            config.persistence.save_on_shutdown = save_on_shutdown.parse().unwrap_or(true);
        }

        // Validate configuration
        config.validate()?;

        Ok(config)
    }

    fn validate(&self) -> Result<(), Box<dyn std::error::Error>> {
        // Prevent division by zero and other critical errors
        if self.server.num_shards == 0 {
            return Err("num_shards must be greater than 0".into());
        }
        if self.server.buffer_size == 0 {
            return Err("buffer_size must be greater than 0".into());
        }
        if self.server.batch_size == 0 {
            return Err("batch_size must be greater than 0".into());
        }
        if self.server.port == 0 {
            return Err("port must be greater than 0".into());
        }
        
        // Eviction config validation
        if self.memory.max_memory > 0 && self.memory.eviction_sample_size == 0 {
            return Err("eviction_sample_size must be > 0 when max_memory is set".into());
        }
        
        // TLS config validation
        if self.security.tls_enabled {
            if self.security.tls_cert_path.is_empty() {
                return Err("tls_cert_path is required when TLS is enabled".into());
            }
            if self.security.tls_key_path.is_empty() {
                return Err("tls_key_path is required when TLS is enabled".into());
            }
        }

        Ok(())
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

// Persistence state
static LAST_SAVE_TIME: AtomicU64 = AtomicU64::new(0);
static SAVE_IN_PROGRESS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

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

    // Fast AHash with hardware acceleration (AES-NI)
    #[inline(always)]
    fn hash(&self, key: &[u8]) -> usize {
        let mut hasher = AHasher::default();
        hasher.write(key);
        hasher.finish() as usize % self.num_shards
    }

    /// Set a key-value pair. Returns the old entry's size if it existed (for memory tracking).
    #[inline(always)]
    fn set(&self, key: Bytes, value: Bytes, ttl: Option<u64>, now: u64) -> Option<usize> {
        let expiry = ttl.map(|s| now + s);
        let key_len = key.len();
        let shard = &self.shards[self.hash(&key)];
        
        // Skip LRU timestamp update 90% of the time for better write performance
        // Still accurate enough for eviction purposes
        let timestamp = if fastrand::u32(..) % 10 == 0 {
            get_uptime_seconds()
        } else {
            0  // Use 0 to skip atomic update, will be updated on read if needed
        };
        
        // insert() returns the old value atomically - no race condition
        let old_entry = shard.insert(
            key,
            Entry {
                value,
                expiry,
                last_accessed: AtomicU32::new(timestamp),
            },
        );
        
        // Return old entry size for memory tracking
        old_entry.map(|e| entry_size(key_len, e.value.len()))
    }

    #[inline(always)]
    fn get(&self, key: &[u8], now: u64) -> Option<Bytes> {
        let shard = &self.shards[self.hash(key)];

        // Try read-only access first
        if let Some(entry) = shard.get(key) {
            if let Some(expiry) = entry.expiry {
                if now >= expiry {
                    // Expired - need to remove
                    let key_len = key.len();
                    let value_len = entry.value.len();
                    drop(entry);
                    
                    // Only decrement memory if we actually removed the key
                    // This prevents double-decrement race with eviction
                    if shard.remove(key).is_some() {
                        if CONFIG.memory.max_memory > 0 {
                            let size = entry_size(key_len, value_len);
                            MEMORY_USED.fetch_sub(size as u64, Ordering::Relaxed);
                        }
                    }
                    return None;
                }
            }

            // Update access time approximately (90% skip for performance)
            maybe_update_access_time(&entry);

            return Some(entry.value.clone());
        }
        None
    }

    /// Delete keys. Returns (count_deleted, bytes_freed) for memory tracking.
    #[inline(always)]
    fn delete(&self, keys: &[Bytes]) -> (usize, usize) {
        // Group by shard for efficiency
        let mut shard_keys: Vec<Vec<&Bytes>> = vec![Vec::new(); self.num_shards];
        for key in keys {
            shard_keys[self.hash(key)].push(key);
        }

        let mut count = 0;
        let mut bytes_freed = 0;
        for (idx, keys_in_shard) in shard_keys.iter().enumerate() {
            if !keys_in_shard.is_empty() {
                let shard = &self.shards[idx];
                for key in keys_in_shard {
                    if let Some((k, entry)) = shard.remove(*key) {
                        count += 1;
                        bytes_freed += entry_size(k.len(), entry.value.len());
                    }
                }
            }
        }
        (count, bytes_freed)
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
                    // DoS protection: reject connections with excessively large buffers
                    if self.buffer.len() > MAX_BUFFER_SIZE {
                        return Err(());
                    }
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
            
            // Security: Prevent DoS via massive array allocation
            if array_len > MAX_ARRAY_LEN {
                return Err(());
            }
            
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
                
                // Security: Prevent DoS via massive string allocation
                if str_len > MAX_STRING_LEN {
                    return Err(());
                }
                
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
    fn write_signed_integer(&mut self, i: i64) {
        self.buffer.push(b':');
        if i < 0 {
            self.buffer.push(b'-');
            self.write_u64((-i) as u64);
        } else {
            self.write_u64(i as u64);
        }
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

// Check connection rate limit (TOCTOU race fixed with compare_exchange)
#[inline]
fn check_rate_limit() -> bool {
    let rate_limit = CONFIG.server.connection_rate_limit;

    // No rate limit
    if rate_limit == 0 {
        return true;
    }

    let now = get_timestamp();
    let last_check = LAST_CONNECTION_CHECK.load(Ordering::Acquire);

    // New second - atomically reset counter
    if now > last_check {
        // Try to atomically update the timestamp
        match LAST_CONNECTION_CHECK.compare_exchange(
            last_check,
            now,
            Ordering::AcqRel,
            Ordering::Acquire
        ) {
            Ok(_) => {
                // We won the race - reset counter
                CONNECTIONS_THIS_SECOND.store(1, Ordering::Release);
                return true;
            }
            Err(_) => {
                // Someone else reset it - fall through to increment
            }
        }
    }

    // Same second - check limit
    let count = CONNECTIONS_THIS_SECOND.fetch_add(1, Ordering::AcqRel);
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

// Parse bytes as u64 with overflow protection
#[inline(always)]
fn parse_u64(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() {
        return None;
    }
    let mut val = 0u64;
    for &b in bytes.iter() {
        if !(b'0'..=b'9').contains(&b) {
            return None;
        }
        val = val.checked_mul(10)?.checked_add((b - b'0') as u64)?;
    }
    Some(val)
}

// Parse bytes as i64 with overflow protection
#[inline(always)]
fn parse_i64(bytes: &[u8]) -> Option<i64> {
    if bytes.is_empty() {
        return None;
    }
    let (negative, start) = if bytes[0] == b'-' {
        (true, 1)
    } else {
        (false, 0)
    };
    if start >= bytes.len() {
        return None;
    }
    let mut val = 0i64;
    for &b in bytes[start..].iter() {
        if !(b'0'..=b'9').contains(&b) {
            return None;
        }
        val = val.checked_mul(10)?.checked_add((b - b'0') as i64)?;
    }
    if negative {
        Some(-val)
    } else {
        Some(val)
    }
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

// Passive key expiration: scan random keys and remove expired ones
// This runs in a background task to clean up keys that are never accessed
fn expire_random_keys(store: &ShardedStore, sample_size: usize) -> usize {
    let now = get_timestamp();
    let mut expired_count = 0;
    
    for _ in 0..sample_size {
        // Pick a random shard
        let shard_idx = fastrand::usize(..store.num_shards);
        let shard = &store.shards[shard_idx];
        
        // Check first entry in the shard
        if let Some(entry) = shard.iter().next() {
            if let Some(expiry) = entry.value().expiry {
                if now >= expiry {
                    let key = entry.key().clone();
                    let key_len = key.len();
                    let value_len = entry.value().value.len();
                    drop(entry);
                    
                    // Remove expired key
                    if shard.remove(&key).is_some() {
                        expired_count += 1;
                        if CONFIG.memory.max_memory > 0 {
                            let size = entry_size(key_len, value_len);
                            MEMORY_USED.fetch_sub(size as u64, Ordering::Relaxed);
                        }
                    }
                }
            }
        }
    }
    
    expired_count
}

// Background task for passive key expiration
async fn expiration_task(store: ShardedStore) {
    // Run every 100ms, check 20 random keys per iteration
    // This is similar to Redis's passive expiration strategy
    let mut interval = tokio::time::interval(Duration::from_millis(100));
    
    loop {
        interval.tick().await;
        expire_random_keys(&store, 20);
    }
}

// ==================== Persistence / Snapshot ====================

// Snapshot file format:
// - Magic: "RDST" (4 bytes)
// - Version: u8
// - Timestamp: u64
// - Entry count: u64
// - Entries: [key_len: u32, key: bytes, value_len: u32, value: bytes, expiry: Option<u64>]...
const SNAPSHOT_MAGIC: &[u8; 4] = b"RDST";
const SNAPSHOT_VERSION: u8 = 1;

// Entry structure for serialization (avoid atomic types)
#[derive(Serialize, Deserialize)]
struct SnapshotEntry {
    key: Vec<u8>,
    value: Vec<u8>,
    expiry: Option<u64>,
}

/// Save snapshot synchronously (blocking) - used by SAVE command
fn save_snapshot_sync(store: &ShardedStore, path: &str) -> Result<usize, String> {
    use std::io::{BufWriter, Write};
    
    // Prevent concurrent saves
    if SAVE_IN_PROGRESS.swap(true, Ordering::SeqCst) {
        return Err("Background save already in progress".to_string());
    }
    
    let result = (|| {
        let now = get_timestamp();
        let temp_path = format!("{}.tmp", path);
        
        // Create file with buffered writer for streaming
        let file = std::fs::File::create(&temp_path)
            .map_err(|e| format!("Failed to create snapshot file: {}", e))?;
        let mut writer = BufWriter::with_capacity(64 * 1024, file); // 64KB buffer
        
        // Write header
        writer.write_all(SNAPSHOT_MAGIC)
            .map_err(|e| format!("Failed to write header: {}", e))?;
        writer.write_all(&[SNAPSHOT_VERSION])
            .map_err(|e| format!("Failed to write version: {}", e))?;
        writer.write_all(&now.to_le_bytes())
            .map_err(|e| format!("Failed to write timestamp: {}", e))?;
        
        // Count entries (approximate, may change during iteration)
        let entry_count_pos = writer.stream_position()
            .map_err(|e| format!("Failed to get position: {}", e))?;
        writer.write_all(&0u64.to_le_bytes()) // Placeholder
            .map_err(|e| format!("Failed to write entry count: {}", e))?;
        
        // Stream entries from all shards
        let mut count: u64 = 0;
        for shard in &store.shards {
            for entry in shard.iter() {
                let (key, val) = entry.pair();
                
                // Skip expired entries
                if let Some(expiry) = val.expiry {
                    if now >= expiry {
                        continue;
                    }
                }
                
                // Serialize entry using bincode
                let snapshot_entry = SnapshotEntry {
                    key: key.to_vec(),
                    value: val.value.to_vec(),
                    expiry: val.expiry,
                };
                
                let encoded = bincode::serialize(&snapshot_entry)
                    .map_err(|e| format!("Failed to serialize entry: {}", e))?;
                
                // Write length-prefixed entry
                writer.write_all(&(encoded.len() as u32).to_le_bytes())
                    .map_err(|e| format!("Failed to write entry length: {}", e))?;
                writer.write_all(&encoded)
                    .map_err(|e| format!("Failed to write entry: {}", e))?;
                
                count += 1;
            }
        }
        
        // Flush and get file handle back
        writer.flush().map_err(|e| format!("Failed to flush: {}", e))?;
        let mut file = writer.into_inner()
            .map_err(|e| format!("Failed to get file: {}", e))?;
        
        // Go back and write actual entry count
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(entry_count_pos))
            .map_err(|e| format!("Failed to seek: {}", e))?;
        file.write_all(&count.to_le_bytes())
            .map_err(|e| format!("Failed to write final count: {}", e))?;
        
        // Sync to disk
        file.sync_all().map_err(|e| format!("Failed to sync: {}", e))?;
        drop(file);
        
        // Atomic rename
        std::fs::rename(&temp_path, path)
            .map_err(|e| format!("Failed to rename snapshot: {}", e))?;
        
        // Update last save time
        LAST_SAVE_TIME.store(now, Ordering::Relaxed);
        
        Ok(count as usize)
    })();
    
    SAVE_IN_PROGRESS.store(false, Ordering::SeqCst);
    result
}

/// Load snapshot from disk - called on startup
fn load_snapshot(store: &ShardedStore, path: &str) -> Result<usize, String> {
    use std::io::{BufReader, Read};
    
    if !std::path::Path::new(path).exists() {
        return Ok(0); // No snapshot to load
    }
    
    let file = std::fs::File::open(path)
        .map_err(|e| format!("Failed to open snapshot: {}", e))?;
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    
    // Read and verify header
    let mut magic = [0u8; 4];
    reader.read_exact(&mut magic)
        .map_err(|e| format!("Failed to read magic: {}", e))?;
    if &magic != SNAPSHOT_MAGIC {
        return Err("Invalid snapshot file (bad magic)".to_string());
    }
    
    let mut version = [0u8; 1];
    reader.read_exact(&mut version)
        .map_err(|e| format!("Failed to read version: {}", e))?;
    if version[0] != SNAPSHOT_VERSION {
        return Err(format!("Unsupported snapshot version: {}", version[0]));
    }
    
    let mut timestamp_bytes = [0u8; 8];
    reader.read_exact(&mut timestamp_bytes)
        .map_err(|e| format!("Failed to read timestamp: {}", e))?;
    let _snapshot_time = u64::from_le_bytes(timestamp_bytes);
    
    let mut count_bytes = [0u8; 8];
    reader.read_exact(&mut count_bytes)
        .map_err(|e| format!("Failed to read entry count: {}", e))?;
    let entry_count = u64::from_le_bytes(count_bytes);
    
    let now = get_timestamp();
    let mut loaded: usize = 0;
    let mut skipped_expired: usize = 0;
    
    // Read entries
    for _ in 0..entry_count {
        // Read entry length
        let mut len_bytes = [0u8; 4];
        if reader.read_exact(&mut len_bytes).is_err() {
            break; // End of file
        }
        let entry_len = u32::from_le_bytes(len_bytes) as usize;
        
        // Read entry data
        let mut entry_data = vec![0u8; entry_len];
        reader.read_exact(&mut entry_data)
            .map_err(|e| format!("Failed to read entry data: {}", e))?;
        
        // Deserialize
        let snapshot_entry: SnapshotEntry = bincode::deserialize(&entry_data)
            .map_err(|e| format!("Failed to deserialize entry: {}", e))?;
        
        // Skip expired entries
        if let Some(expiry) = snapshot_entry.expiry {
            if now >= expiry {
                skipped_expired += 1;
                continue;
            }
        }
        
        // Calculate remaining TTL
        let ttl = snapshot_entry.expiry.and_then(|exp| {
            if exp > now { Some(exp - now) } else { None }
        });
        
        // Insert into store
        let key = Bytes::from(snapshot_entry.key);
        let value = Bytes::from(snapshot_entry.value);
        
        // Track memory if needed
        if CONFIG.memory.max_memory > 0 {
            let size = entry_size(key.len(), value.len());
            MEMORY_USED.fetch_add(size as u64, Ordering::Relaxed);
        }
        
        store.set(key, value, ttl, now);
        loaded += 1;
    }
    
    // Update last save time to snapshot time
    LAST_SAVE_TIME.store(now, Ordering::Relaxed);
    
    if skipped_expired > 0 {
        eprintln!("   Skipped {} expired keys", skipped_expired);
    }
    
    Ok(loaded)
}

/// Background task for periodic snapshots
async fn snapshot_task(store: ShardedStore, interval_secs: u64, path: String) {
    if interval_secs == 0 {
        return; // Disabled
    }
    
    let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
    interval.tick().await; // Skip first immediate tick
    
    loop {
        interval.tick().await;
        
        // Clone store reference for background save
        let store_clone = store.clone();
        let path_clone = path.clone();
        
        // Spawn blocking task for I/O
        tokio::task::spawn_blocking(move || {
            match save_snapshot_sync(&store_clone, &path_clone) {
                Ok(count) => {
                    eprintln!("Background snapshot saved: {} keys to {}", count, path_clone);
                }
                Err(e) => {
                    eprintln!("Background snapshot failed: {}", e);
                }
            }
        });
    }
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
    // Batch counter updates to reduce atomic operation overhead
    // Update global counter every 256 operations instead of every operation
    thread_local! {
        static LOCAL_CMD_COUNT: std::cell::Cell<u64> = std::cell::Cell::new(0);
    }
    
    LOCAL_CMD_COUNT.with(|count| {
        let new_count = count.get() + 1;
        if new_count >= 256 {
            TOTAL_COMMANDS.fetch_add(256, Ordering::Relaxed);
            count.set(0);
        } else {
            count.set(new_count);
        }
    });
    
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

                    // Parse options: EX, PX, NX, XX, GET
                    let mut ttl: Option<u64> = None;
                    let mut nx = false;  // Only set if Not eXists
                    let mut xx = false;  // Only set if eXists
                    let mut get = false; // Return old value
                    
                    let mut i = 3;
                    while i < command.len() {
                        let opt = &command[i];
                        let opt_len = opt.len();
                        
                        if opt_len == 2 {
                            let o0 = opt[0] | 0x20;
                            let o1 = opt[1] | 0x20;
                            
                            if o0 == b'e' && o1 == b'x' {
                                // EX seconds
                                if i + 1 >= command.len() {
                                    writer.write_error(b"syntax error");
                                    return;
                                }
                                i += 1;
                                match parse_u64(&command[i]) {
                                    Some(v) if v > 0 => ttl = Some(v),
                                    _ => {
                                        writer.write_error(b"value is not an integer or out of range");
                                        return;
                                    }
                                }
                            } else if o0 == b'p' && o1 == b'x' {
                                // PX milliseconds
                                if i + 1 >= command.len() {
                                    writer.write_error(b"syntax error");
                                    return;
                                }
                                i += 1;
                                match parse_u64(&command[i]) {
                                    Some(v) if v > 0 => {
                                        // Convert ms to seconds (round up)
                                        ttl = Some((v + 999) / 1000);
                                    }
                                    _ => {
                                        writer.write_error(b"value is not an integer or out of range");
                                        return;
                                    }
                                }
                            } else if o0 == b'n' && o1 == b'x' {
                                nx = true;
                            } else if o0 == b'x' && o1 == b'x' {
                                xx = true;
                            } else {
                                writer.write_error(b"syntax error");
                                return;
                            }
                        } else if opt_len == 3 && (opt[0] | 0x20) == b'g' && (opt[1] | 0x20) == b'e' && (opt[2] | 0x20) == b't' {
                            get = true;
                        } else {
                            writer.write_error(b"syntax error");
                            return;
                        }
                        i += 1;
                    }
                    
                    // NX and XX are mutually exclusive
                    if nx && xx {
                        writer.write_error(b"XX and NX options at the same time are not compatible");
                        return;
                    }
                    
                    // Check NX/XX conditions
                    let shard = &store.shards[store.hash(key)];
                    let old_value = if nx || xx || get {
                        shard.get(key.as_ref()).and_then(|entry| {
                            // Check if expired
                            if let Some(expiry) = entry.expiry {
                                if now >= expiry {
                                    return None;
                                }
                            }
                            Some(entry.value.clone())
                        })
                    } else {
                        None
                    };
                    
                    let key_exists = old_value.is_some();
                    
                    // NX: only set if key doesn't exist
                    if nx && key_exists {
                        if get {
                            writer.write_bulk_string(&old_value.unwrap());
                        } else {
                            writer.write_null();
                        }
                        return;
                    }
                    
                    // XX: only set if key exists
                    if xx && !key_exists {
                        if get {
                            writer.write_null();
                        } else {
                            writer.write_null();
                        }
                        return;
                    }

                    // Atomic set - returns old entry size if key existed
                    let old_size = store.set(key.clone(), value.clone(), ttl, now);

                    // Track memory usage (only if limits enabled)
                    if CONFIG.memory.max_memory > 0 {
                        if let Some(old) = old_size {
                            MEMORY_USED.fetch_sub(old as u64, Ordering::Relaxed);
                        }
                        MEMORY_USED.fetch_add(size as u64, Ordering::Relaxed);
                    }

                    if get {
                        match old_value {
                            Some(v) => writer.write_bulk_string(&v),
                            None => writer.write_null(),
                        }
                    } else {
                        writer.write_simple_string(b"OK");
                    }
                } else {
                    writer.write_error(b"wrong number of arguments");
                }
                return;
            }
            if eq_ignore_case_3(cmd, b"ttl") {
                // TTL key - returns remaining time in seconds
                if command.len() >= 2 {
                    let key = &command[1];
                    let shard = &store.shards[store.hash(key)];
                    
                    match shard.get(key.as_ref()) {
                        Some(entry) => {
                            match entry.expiry {
                                Some(expiry) => {
                                    if now >= expiry {
                                        // Key expired
                                        drop(entry);
                                        shard.remove(key.as_ref());
                                        writer.write_signed_integer(-2);
                                    } else {
                                        writer.write_signed_integer((expiry - now) as i64);
                                    }
                                }
                                None => {
                                    // Key exists but has no TTL
                                    writer.write_signed_integer(-1);
                                }
                            }
                        }
                        None => {
                            // Key doesn't exist
                            writer.write_signed_integer(-2);
                        }
                    }
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
                    let (count, bytes_freed) = store.delete(&command[1..]);
                    // Track memory freed (only if limits enabled)
                    if CONFIG.memory.max_memory > 0 && bytes_freed > 0 {
                        MEMORY_USED.fetch_sub(bytes_freed as u64, Ordering::Relaxed);
                    }
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
            if eq_ignore_case_3(&cmd[..3], b"sav") && (cmd[3] | 0x20) == b'e' {
                // SAVE - synchronous snapshot (blocks until complete)
                if !CONFIG.persistence.enabled {
                    writer.write_error(b"persistence is disabled");
                    return;
                }
                match save_snapshot_sync(store, &CONFIG.persistence.snapshot_path) {
                    Ok(count) => {
                        eprintln!("Snapshot saved: {} keys", count);
                        writer.write_simple_string(b"OK");
                    }
                    Err(e) => {
                        writer.write_error(e.as_bytes());
                    }
                }
                return;
            }
            if eq_ignore_case_3(&cmd[..3], b"inc") && (cmd[3] | 0x20) == b'r' {
                // INCR key
                if command.len() >= 2 {
                    let key = &command[1];
                    let shard = &store.shards[store.hash(key)];
                    
                    // Get current value or default to 0
                    let current = match shard.get(key.as_ref()) {
                        Some(entry) => {
                            // Check if expired
                            if let Some(expiry) = entry.expiry {
                                if now >= expiry {
                                    drop(entry);
                                    shard.remove(key.as_ref());
                                    0i64
                                } else {
                                    match parse_i64(&entry.value) {
                                        Some(v) => v,
                                        None => {
                                            writer.write_error(b"value is not an integer or out of range");
                                            return;
                                        }
                                    }
                                }
                            } else {
                                match parse_i64(&entry.value) {
                                    Some(v) => v,
                                    None => {
                                        writer.write_error(b"value is not an integer or out of range");
                                        return;
                                    }
                                }
                            }
                        }
                        None => 0i64,
                    };
                    
                    let new_val = match current.checked_add(1) {
                        Some(v) => v,
                        None => {
                            writer.write_error(b"increment would produce overflow");
                            return;
                        }
                    };
                    
                    let val_bytes = Bytes::from(new_val.to_string());
                    let size = entry_size(key.len(), val_bytes.len());
                    
                    if !evict_if_needed(store, size) {
                        writer.write_error(b"OOM command not allowed when used memory > 'maxmemory'");
                        return;
                    }
                    
                    // Preserve existing TTL
                    let existing_ttl = shard.get(key.as_ref()).and_then(|e| {
                        e.expiry.map(|exp| if exp > now { exp - now } else { 0 })
                    });
                    
                    let old_size = store.set(key.clone(), val_bytes, existing_ttl, now);
                    
                    if CONFIG.memory.max_memory > 0 {
                        if let Some(old) = old_size {
                            MEMORY_USED.fetch_sub(old as u64, Ordering::Relaxed);
                        }
                        MEMORY_USED.fetch_add(size as u64, Ordering::Relaxed);
                    }
                    
                    writer.write_signed_integer(new_val);
                } else {
                    writer.write_error(b"wrong number of arguments");
                }
                return;
            }
            if eq_ignore_case_3(&cmd[..3], b"dec") && (cmd[3] | 0x20) == b'r' {
                // DECR key
                if command.len() >= 2 {
                    let key = &command[1];
                    let shard = &store.shards[store.hash(key)];
                    
                    let current = match shard.get(key.as_ref()) {
                        Some(entry) => {
                            if let Some(expiry) = entry.expiry {
                                if now >= expiry {
                                    drop(entry);
                                    shard.remove(key.as_ref());
                                    0i64
                                } else {
                                    match parse_i64(&entry.value) {
                                        Some(v) => v,
                                        None => {
                                            writer.write_error(b"value is not an integer or out of range");
                                            return;
                                        }
                                    }
                                }
                            } else {
                                match parse_i64(&entry.value) {
                                    Some(v) => v,
                                    None => {
                                        writer.write_error(b"value is not an integer or out of range");
                                        return;
                                    }
                                }
                            }
                        }
                        None => 0i64,
                    };
                    
                    let new_val = match current.checked_sub(1) {
                        Some(v) => v,
                        None => {
                            writer.write_error(b"decrement would produce overflow");
                            return;
                        }
                    };
                    
                    let val_bytes = Bytes::from(new_val.to_string());
                    let size = entry_size(key.len(), val_bytes.len());
                    
                    if !evict_if_needed(store, size) {
                        writer.write_error(b"OOM command not allowed when used memory > 'maxmemory'");
                        return;
                    }
                    
                    let existing_ttl = shard.get(key.as_ref()).and_then(|e| {
                        e.expiry.map(|exp| if exp > now { exp - now } else { 0 })
                    });
                    
                    let old_size = store.set(key.clone(), val_bytes, existing_ttl, now);
                    
                    if CONFIG.memory.max_memory > 0 {
                        if let Some(old) = old_size {
                            MEMORY_USED.fetch_sub(old as u64, Ordering::Relaxed);
                        }
                        MEMORY_USED.fetch_add(size as u64, Ordering::Relaxed);
                    }
                    
                    writer.write_signed_integer(new_val);
                } else {
                    writer.write_error(b"wrong number of arguments");
                }
                return;
            }
            if eq_ignore_case_3(&cmd[..3], b"ptt") && (cmd[3] | 0x20) == b'l' {
                // PTTL key - returns remaining time in milliseconds
                if command.len() >= 2 {
                    let key = &command[1];
                    let shard = &store.shards[store.hash(key)];
                    
                    match shard.get(key.as_ref()) {
                        Some(entry) => {
                            match entry.expiry {
                                Some(expiry) => {
                                    if now >= expiry {
                                        drop(entry);
                                        shard.remove(key.as_ref());
                                        writer.write_signed_integer(-2);
                                    } else {
                                        // Convert seconds to milliseconds
                                        writer.write_signed_integer(((expiry - now) * 1000) as i64);
                                    }
                                }
                                None => {
                                    writer.write_signed_integer(-1);
                                }
                            }
                        }
                        None => {
                            writer.write_signed_integer(-2);
                        }
                    }
                } else {
                    writer.write_error(b"wrong number of arguments");
                }
                return;
            }
            if eq_ignore_case_3(&cmd[..3], b"mge") && (cmd[3] | 0x20) == b't' {
                // MGET key [key ...]
                if command.len() >= 2 {
                    // Write array header
                    writer.buffer.push(b'*');
                    writer.write_u64((command.len() - 1) as u64);
                    writer.buffer.extend_from_slice(b"\r\n");
                    
                    for key in &command[1..] {
                        match store.get(key, now) {
                            Some(value) => writer.write_bulk_string(&value),
                            None => writer.write_null(),
                        }
                    }
                } else {
                    writer.write_error(b"wrong number of arguments");
                }
                return;
            }
            if eq_ignore_case_3(&cmd[..3], b"mse") && (cmd[3] | 0x20) == b't' {
                // MSET key value [key value ...]
                if command.len() >= 3 && (command.len() - 1) % 2 == 0 {
                    let pairs = (command.len() - 1) / 2;
                    
                    // Check memory for all pairs first
                    let mut total_size = 0;
                    for i in 0..pairs {
                        let key = &command[1 + i * 2];
                        let value = &command[2 + i * 2];
                        total_size += entry_size(key.len(), value.len());
                    }
                    
                    if !evict_if_needed(store, total_size) {
                        writer.write_error(b"OOM command not allowed when used memory > 'maxmemory'");
                        return;
                    }
                    
                    // Set all pairs
                    for i in 0..pairs {
                        let key = &command[1 + i * 2];
                        let value = &command[2 + i * 2];
                        let size = entry_size(key.len(), value.len());
                        
                        let old_size = store.set(key.clone(), value.clone(), None, now);
                        
                        if CONFIG.memory.max_memory > 0 {
                            if let Some(old) = old_size {
                                MEMORY_USED.fetch_sub(old as u64, Ordering::Relaxed);
                            }
                            MEMORY_USED.fetch_add(size as u64, Ordering::Relaxed);
                        }
                    }
                    
                    writer.write_simple_string(b"OK");
                } else {
                    writer.write_error(b"wrong number of arguments for MSET");
                }
                return;
            }
            if eq_ignore_case_3(&cmd[..3], b"aut") && (cmd[3] | 0x20) == b'h' {
                // AUTH command
                if !CONFIG.security.password.is_empty() {
                    if command.len() >= 2 {
                        // Use constant-time comparison to prevent timing attacks
                        let provided = command[1].as_ref();
                        let expected = CONFIG.security.password.as_bytes();
                        let is_valid = provided.ct_eq(expected).into();
                        if is_valid {
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
            if eq_ignore_case_6(cmd, b"bgsave") {
                // BGSAVE - background snapshot (returns immediately)
                if !CONFIG.persistence.enabled {
                    writer.write_error(b"persistence is disabled");
                    return;
                }
                if SAVE_IN_PROGRESS.load(Ordering::Relaxed) {
                    writer.write_error(b"Background save already in progress");
                    return;
                }
                
                // Clone store for background task
                let store_clone = store.clone();
                let path = CONFIG.persistence.snapshot_path.clone();
                
                // Spawn background save
                std::thread::spawn(move || {
                    match save_snapshot_sync(&store_clone, &path) {
                        Ok(count) => {
                            eprintln!("Background snapshot saved: {} keys", count);
                        }
                        Err(e) => {
                            eprintln!("Background snapshot failed: {}", e);
                        }
                    }
                });
                
                writer.write_simple_string(b"Background saving started");
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
            if eq_ignore_case_6(cmd, b"incrby") {
                // INCRBY key increment
                if command.len() >= 3 {
                    let key = &command[1];
                    let increment = match parse_i64(&command[2]) {
                        Some(v) => v,
                        None => {
                            writer.write_error(b"value is not an integer or out of range");
                            return;
                        }
                    };
                    
                    let shard = &store.shards[store.hash(key)];
                    
                    let current = match shard.get(key.as_ref()) {
                        Some(entry) => {
                            if let Some(expiry) = entry.expiry {
                                if now >= expiry {
                                    drop(entry);
                                    shard.remove(key.as_ref());
                                    0i64
                                } else {
                                    match parse_i64(&entry.value) {
                                        Some(v) => v,
                                        None => {
                                            writer.write_error(b"value is not an integer or out of range");
                                            return;
                                        }
                                    }
                                }
                            } else {
                                match parse_i64(&entry.value) {
                                    Some(v) => v,
                                    None => {
                                        writer.write_error(b"value is not an integer or out of range");
                                        return;
                                    }
                                }
                            }
                        }
                        None => 0i64,
                    };
                    
                    let new_val = match current.checked_add(increment) {
                        Some(v) => v,
                        None => {
                            writer.write_error(b"increment or decrement would overflow");
                            return;
                        }
                    };
                    
                    let val_bytes = Bytes::from(new_val.to_string());
                    let size = entry_size(key.len(), val_bytes.len());
                    
                    if !evict_if_needed(store, size) {
                        writer.write_error(b"OOM command not allowed when used memory > 'maxmemory'");
                        return;
                    }
                    
                    let existing_ttl = shard.get(key.as_ref()).and_then(|e| {
                        e.expiry.map(|exp| if exp > now { exp - now } else { 0 })
                    });
                    
                    let old_size = store.set(key.clone(), val_bytes, existing_ttl, now);
                    
                    if CONFIG.memory.max_memory > 0 {
                        if let Some(old) = old_size {
                            MEMORY_USED.fetch_sub(old as u64, Ordering::Relaxed);
                        }
                        MEMORY_USED.fetch_add(size as u64, Ordering::Relaxed);
                    }
                    
                    writer.write_signed_integer(new_val);
                } else {
                    writer.write_error(b"wrong number of arguments");
                }
                return;
            }
            if eq_ignore_case_6(cmd, b"decrby") {
                // DECRBY key decrement
                if command.len() >= 3 {
                    let key = &command[1];
                    let decrement = match parse_i64(&command[2]) {
                        Some(v) => v,
                        None => {
                            writer.write_error(b"value is not an integer or out of range");
                            return;
                        }
                    };
                    
                    let shard = &store.shards[store.hash(key)];
                    
                    let current = match shard.get(key.as_ref()) {
                        Some(entry) => {
                            if let Some(expiry) = entry.expiry {
                                if now >= expiry {
                                    drop(entry);
                                    shard.remove(key.as_ref());
                                    0i64
                                } else {
                                    match parse_i64(&entry.value) {
                                        Some(v) => v,
                                        None => {
                                            writer.write_error(b"value is not an integer or out of range");
                                            return;
                                        }
                                    }
                                }
                            } else {
                                match parse_i64(&entry.value) {
                                    Some(v) => v,
                                    None => {
                                        writer.write_error(b"value is not an integer or out of range");
                                        return;
                                    }
                                }
                            }
                        }
                        None => 0i64,
                    };
                    
                    let new_val = match current.checked_sub(decrement) {
                        Some(v) => v,
                        None => {
                            writer.write_error(b"increment or decrement would overflow");
                            return;
                        }
                    };
                    
                    let val_bytes = Bytes::from(new_val.to_string());
                    let size = entry_size(key.len(), val_bytes.len());
                    
                    if !evict_if_needed(store, size) {
                        writer.write_error(b"OOM command not allowed when used memory > 'maxmemory'");
                        return;
                    }
                    
                    let existing_ttl = shard.get(key.as_ref()).and_then(|e| {
                        e.expiry.map(|exp| if exp > now { exp - now } else { 0 })
                    });
                    
                    let old_size = store.set(key.clone(), val_bytes, existing_ttl, now);
                    
                    if CONFIG.memory.max_memory > 0 {
                        if let Some(old) = old_size {
                            MEMORY_USED.fetch_sub(old as u64, Ordering::Relaxed);
                        }
                        MEMORY_USED.fetch_add(size as u64, Ordering::Relaxed);
                    }
                    
                    writer.write_signed_integer(new_val);
                } else {
                    writer.write_error(b"wrong number of arguments");
                }
                return;
            }
            if eq_ignore_case_6(cmd, b"expire") {
                // EXPIRE key seconds
                if command.len() >= 3 {
                    let key = &command[1];
                    let seconds = match parse_i64(&command[2]) {
                        Some(v) if v > 0 => v as u64,
                        Some(_) => {
                            // Negative or zero TTL = delete the key
                            let (count, bytes_freed) = store.delete(&[command[1].clone()]);
                            if CONFIG.memory.max_memory > 0 && bytes_freed > 0 {
                                MEMORY_USED.fetch_sub(bytes_freed as u64, Ordering::Relaxed);
                            }
                            writer.write_integer(count);
                            return;
                        }
                        None => {
                            writer.write_error(b"value is not an integer or out of range");
                            return;
                        }
                    };
                    
                    let shard = &store.shards[store.hash(key)];
                    
                    // Check if key exists and update its expiry
                    if let Some(mut entry) = shard.get_mut(key.as_ref()) {
                        // Check if expired
                        if let Some(expiry) = entry.expiry {
                            if now >= expiry {
                                drop(entry);
                                shard.remove(key.as_ref());
                                writer.write_integer(0);
                                return;
                            }
                        }
                        // Update expiry
                        entry.expiry = Some(now + seconds);
                        writer.write_integer(1);
                    } else {
                        writer.write_integer(0);
                    }
                } else {
                    writer.write_error(b"wrong number of arguments");
                }
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
                    MEMORY_USED.store(0, Ordering::Relaxed);
                    writer.write_simple_string(b"OK");
                    return;
                }
                if &lower == b"command" {
                    writer.buffer.extend_from_slice(b"*0\r\n");
                    return;
                }
                if &lower == b"persist" {
                    // PERSIST key - remove TTL from key
                    if command.len() >= 2 {
                        let key = &command[1];
                        let shard = &store.shards[store.hash(key)];
                        
                        if let Some(mut entry) = shard.get_mut(key.as_ref()) {
                            if let Some(expiry) = entry.expiry {
                                if now >= expiry {
                                    drop(entry);
                                    shard.remove(key.as_ref());
                                    writer.write_integer(0);
                                } else if entry.expiry.is_some() {
                                    entry.expiry = None;
                                    writer.write_integer(1);
                                } else {
                                    writer.write_integer(0);
                                }
                            } else {
                                // Key has no TTL
                                writer.write_integer(0);
                            }
                        } else {
                            writer.write_integer(0);
                        }
                    } else {
                        writer.write_error(b"wrong number of arguments");
                    }
                    return;
                }
            }
        }
        8 => {
            // LASTSAVE - return timestamp of last successful save
            if cmd.len() == 8 {
                let lower = [
                    cmd[0] | 0x20,
                    cmd[1] | 0x20,
                    cmd[2] | 0x20,
                    cmd[3] | 0x20,
                    cmd[4] | 0x20,
                    cmd[5] | 0x20,
                    cmd[6] | 0x20,
                    cmd[7] | 0x20,
                ];
                if &lower == b"lastsave" {
                    let last_save = LAST_SAVE_TIME.load(Ordering::Relaxed);
                    writer.write_integer(last_save as usize);
                    return;
                }
                if &lower == b"flushall" {
                    store.clear();
                    MEMORY_USED.store(0, Ordering::Relaxed);
                    writer.write_simple_string(b"OK");
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
    
    // Connection idle timeout (0 = disabled)
    let timeout_duration = if CONFIG.server.connection_timeout > 0 {
        Some(Duration::from_secs(CONFIG.server.connection_timeout))
    } else {
        None
    };

    loop {
        let now = get_timestamp();

        // Apply idle timeout if configured
        let parse_result = if let Some(timeout) = timeout_duration {
            match tokio::time::timeout(timeout, parser.parse_command(&mut stream)).await {
                Ok(result) => result,
                Err(_) => {
                    // Timeout - close idle connection
                    break;
                }
            }
        } else {
            parser.parse_command(&mut stream).await
        };

        match parse_result {
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
    let addr = format!("0.0.0.0:{}", port);
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!(
                "Failed to bind health check endpoint on {}: {}",
                addr, e
            );
            return;
        }
    };

    println!("Health check endpoint: http://{}/health", addr);

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
        
 /$$$$$$$                  /$$ /$$             /$$     /$$ /$$ /$$
| $$__  $$                | $$|__/            | $$    |__/| $$| $$
| $$  \ $$  /$$$$$$   /$$$$$$$ /$$  /$$$$$$$ /$$$$$$   /$$| $$| $$
| $$$$$$$/ /$$__  $$ /$$__  $$| $$ /$$_____/|_  $$_/  | $$| $$| $$
| $$__  $$| $$$$$$$$| $$  | $$| $$|  $$$$$$   | $$    | $$| $$| $$
| $$  \ $$| $$_____/| $$  | $$| $$ \____  $$  | $$ /$$| $$| $$| $$
| $$  | $$|  $$$$$$$|  $$$$$$$| $$ /$$$$$$$/  |  $$$$/| $$| $$| $$
|__/  |__/ \_______/ \_______/|__/|_______/    \___/  |__/|__/|__/
                                                                  
══════════════════════════════════════════════════════════════════
Configuration:
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
   • Persistence: {}

Performance: 2x faster than Redis with pipelining!
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
        if config.persistence.enabled {
            format!("enabled (interval: {}s, path: {})", 
                config.persistence.snapshot_interval,
                config.persistence.snapshot_path)
        } else {
            "disabled".to_string()
        }
    );

    // Load snapshot if persistence is enabled
    if config.persistence.enabled {
        print!("Loading snapshot from {}... ", config.persistence.snapshot_path);
        match load_snapshot(&store, &config.persistence.snapshot_path) {
            Ok(0) => println!("no snapshot found"),
            Ok(count) => println!("loaded {} keys", count),
            Err(e) => {
                eprintln!("failed: {}", e);
                eprintln!("Starting with empty database");
            }
        }
    }

    // Load TLS configuration if enabled
    let tls_acceptor = if config.security.tls_enabled {
        if config.security.tls_cert_path.is_empty() || config.security.tls_key_path.is_empty() {
            eprintln!("TLS enabled but cert/key paths not configured");
            std::process::exit(1);
        }

        match load_tls_config(
            &config.security.tls_cert_path,
            &config.security.tls_key_path,
        )
        .await
        {
            Ok(tls_config) => {
                println!("TLS/SSL enabled");
                println!("   • Certificate: {}", config.security.tls_cert_path);
                println!("   • Private Key: {}", config.security.tls_key_path);
                Some(TlsAcceptor::from(tls_config))
            }
            Err(e) => {
                eprintln!("Failed to load TLS configuration: {}", e);
                eprintln!("Make sure certificate and key files exist and are valid");
                std::process::exit(1);
            }
        }
    } else {
        None
    };

    let bind_addr = format!("{}:{}", config.server.bind, config.server.port);
    let listener = TcpListener::bind(&bind_addr).await.unwrap_or_else(|e| {
        eprintln!("Failed to bind to {}: {}", bind_addr, e);
        std::process::exit(1);
    });

    println!("Listening on {}", bind_addr);

    if !config.security.password.is_empty() {
        println!("Authentication enabled (password set in config or env var)");
    } else {
        println!(
            "Authentication disabled (set password in config file or REDIS_PASSWORD env var)"
        );
    }

    let config_file =
        std::env::var("REDISTILL_CONFIG").unwrap_or_else(|_| "redistill.toml".to_string());
    if std::path::Path::new(&config_file).exists() {
        println!("Configuration loaded from {}", config_file);
    } else {
        println!("Using default configuration (create redistill.toml to customize)");
    }

    // Start health check endpoint if enabled
    if config.server.health_check_port > 0 {
        let health_port = config.server.health_check_port;
        tokio::spawn(async move {
            start_health_check_server(health_port).await;
        });
    }

    // Start passive key expiration background task
    let expiration_store = store.clone();
    tokio::spawn(async move {
        expiration_task(expiration_store).await;
    });

    // Start periodic snapshot background task if enabled
    if config.persistence.enabled && config.persistence.snapshot_interval > 0 {
        let snapshot_store = store.clone();
        let interval = config.persistence.snapshot_interval;
        let path = config.persistence.snapshot_path.clone();
        println!("Periodic snapshots enabled: every {} seconds to {}", interval, path);
        tokio::spawn(async move {
            snapshot_task(snapshot_store, interval, path).await;
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
                println!("\n\nReceived shutdown signal...");
                
                // Save snapshot on shutdown if enabled
                if CONFIG.persistence.enabled && CONFIG.persistence.save_on_shutdown {
                    print!("Saving final snapshot... ");
                    match save_snapshot_sync(&store, &CONFIG.persistence.snapshot_path) {
                        Ok(count) => println!("saved {} keys", count),
                        Err(e) => eprintln!("failed: {}", e),
                    }
                }
                
                println!("Final Stats:");
                println!("   • Total connections: {}", TOTAL_CONNECTIONS.load(Ordering::Relaxed));
                println!("   • Total commands: {}", TOTAL_COMMANDS.load(Ordering::Relaxed));
                println!("   • Active connections: {}", ACTIVE_CONNECTIONS.load(Ordering::Relaxed));
                println!("   • Keys in database: {}", store.len());
                if CONFIG.persistence.enabled {
                    println!("   • Last save: {}", LAST_SAVE_TIME.load(Ordering::Relaxed));
                }
                println!("\nRedistill shut down gracefully");
                break;
            }
        }
    }
}
