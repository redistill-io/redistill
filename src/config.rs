// Configuration module - loaded once at startup (cold path)
// All config-related code is here to keep hot paths clean

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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
    #[serde(default = "default_s3fifo_small_ratio")]
    pub s3fifo_small_ratio: f64,
    #[serde(default = "default_s3fifo_ghost_ratio")]
    pub s3fifo_ghost_ratio: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistenceConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_snapshot_path")]
    pub snapshot_path: String,
    #[serde(default = "default_true")]
    pub save_on_shutdown: bool,
    #[serde(default = "default_snapshot_interval")]
    pub snapshot_interval: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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
    #[serde(default)]
    pub persistence: PersistenceConfig,
}

// ==================== Default Functions ====================

fn default_bind() -> String {
    "0.0.0.0".to_string()
}
fn default_port() -> u16 {
    6379
}
fn default_num_shards() -> usize {
    2048
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
fn default_s3fifo_small_ratio() -> f64 {
    0.1
}
fn default_s3fifo_ghost_ratio() -> f64 {
    0.1
}
fn default_snapshot_path() -> String {
    "redistill.rdb".to_string()
}
fn default_snapshot_interval() -> u64 {
    300
}

// ==================== Default Implementations ====================

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
            s3fifo_small_ratio: default_s3fifo_small_ratio(),
            s3fifo_ghost_ratio: default_s3fifo_ghost_ratio(),
        }
    }
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

// ==================== Eviction Policy ====================

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum EvictionPolicy {
    NoEviction,
    #[default]
    AllKeysLru,
    AllKeysRandom,
    AllKeysS3Fifo,
}

impl FromStr for EvictionPolicy {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_lowercase().as_str() {
            "allkeys-lru" => EvictionPolicy::AllKeysLru,
            "allkeys-random" => EvictionPolicy::AllKeysRandom,
            "allkeys-s3fifo" => EvictionPolicy::AllKeysS3Fifo,
            "noeviction" => EvictionPolicy::NoEviction,
            _ => EvictionPolicy::AllKeysLru,
        })
    }
}

impl EvictionPolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            EvictionPolicy::NoEviction => "noeviction",
            EvictionPolicy::AllKeysLru => "allkeys-lru",
            EvictionPolicy::AllKeysRandom => "allkeys-random",
            EvictionPolicy::AllKeysS3Fifo => "allkeys-s3fifo",
        }
    }
}

// ==================== Config Loading ====================

impl Config {
    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let config_path =
            std::env::var("REDISTILL_CONFIG").unwrap_or_else(|_| "redistill.toml".to_string());

        let mut config: Config = if std::path::Path::new(&config_path).exists() {
            let contents = std::fs::read_to_string(&config_path)?;
            toml::from_str(&contents)?
        } else {
            if config_path != "redistill.toml" {
                eprintln!("Config file '{}' not found, using defaults", config_path);
            }
            Config::default()
        };

        // Override with environment variables
        if let Ok(password) = std::env::var("REDIS_PASSWORD") {
            config.security.password = password;
        }

        if let Ok(port) = std::env::var("REDIS_PORT")
            && let Ok(p) = port.parse()
        {
            config.server.port = p;
        }

        if let Ok(bind) = std::env::var("REDIS_BIND") {
            config.server.bind = bind;
        }

        if let Ok(health_port) = std::env::var("REDIS_HEALTH_CHECK_PORT")
            && let Ok(p) = health_port.parse()
        {
            config.server.health_check_port = p;
        }

        if let Ok(num_shards) = std::env::var("REDIS_NUM_SHARDS")
            && let Ok(n) = num_shards.parse()
        {
            config.server.num_shards = n;
        }

        if let Ok(batch_size) = std::env::var("REDIS_BATCH_SIZE")
            && let Ok(b) = batch_size.parse()
        {
            config.server.batch_size = b;
        }

        if let Ok(buffer_size) = std::env::var("REDIS_BUFFER_SIZE")
            && let Ok(b) = buffer_size.parse()
        {
            config.server.buffer_size = b;
        }

        if let Ok(buffer_pool_size) = std::env::var("REDIS_BUFFER_POOL_SIZE")
            && let Ok(b) = buffer_pool_size.parse()
        {
            config.server.buffer_pool_size = b;
        }

        if let Ok(max_connections) = std::env::var("REDIS_MAX_CONNECTIONS")
            && let Ok(m) = max_connections.parse()
        {
            config.server.max_connections = m;
        }

        if let Ok(max_memory) = std::env::var("REDIS_MAX_MEMORY")
            && let Ok(m) = max_memory.parse()
        {
            config.memory.max_memory = m;
        }

        if let Ok(eviction_policy) = std::env::var("REDIS_EVICTION_POLICY") {
            config.memory.eviction_policy = eviction_policy;
        }

        if let Ok(tcp_nodelay) = std::env::var("REDIS_TCP_NODELAY") {
            config.performance.tcp_nodelay = tcp_nodelay.parse().unwrap_or(true);
        }

        if let Ok(tcp_keepalive) = std::env::var("REDIS_TCP_KEEPALIVE")
            && let Ok(k) = tcp_keepalive.parse()
        {
            config.performance.tcp_keepalive = k;
        }

        if let Ok(enabled) = std::env::var("REDIS_PERSISTENCE_ENABLED") {
            config.persistence.enabled = enabled.parse().unwrap_or(false);
        }

        if let Ok(path) = std::env::var("REDIS_SNAPSHOT_PATH") {
            config.persistence.snapshot_path = path;
        }

        if let Ok(interval) = std::env::var("REDIS_SNAPSHOT_INTERVAL")
            && let Ok(i) = interval.parse()
        {
            config.persistence.snapshot_interval = i;
        }

        if let Ok(save_on_shutdown) = std::env::var("REDIS_SAVE_ON_SHUTDOWN") {
            config.persistence.save_on_shutdown = save_on_shutdown.parse().unwrap_or(true);
        }

        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), Box<dyn std::error::Error>> {
        if self.server.num_shards == 0 {
            return Err("num_shards must be greater than 0".into());
        }
        if self.server.buffer_size == 0 {
            return Err("buffer_size must be greater than 0".into());
        }
        if self.server.port == 0 {
            return Err("port must be greater than 0".into());
        }
        if self.memory.max_memory > 0 && self.memory.eviction_sample_size == 0 {
            return Err("eviction_sample_size must be > 0 when max_memory is set".into());
        }
        Ok(())
    }
}

// ==================== Global Config ====================

pub static CONFIG: Lazy<Config> = Lazy::new(|| Config::load().expect("Failed to load config"));

// ==================== Helper Functions ====================

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
