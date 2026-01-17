# Redistill Configuration Reference

Complete reference for all configuration options.

## Configuration File

Redistill reads configuration from `redistill.toml` in the current working directory. If not found, defaults are used.

### Custom Configuration File

```bash
# Specify custom config file
REDISTILL_CONFIG=custom.toml ./redistill
```

## Default Configuration

```toml
[server]
bind = "127.0.0.1"
port = 6379
num_shards = 256
batch_size = 16
buffer_size = 16384
buffer_pool_size = 1024
max_connections = 10000
connection_timeout = 300
connection_rate_limit = 0
health_check_port = 0

[security]
password = ""
tls_enabled = false
tls_cert_path = ""
tls_key_path = ""

[memory]
max_memory = 0
eviction_policy = "allkeys-lru"
eviction_sample_size = 5

[logging]
level = "info"
format = "text"

[performance]
tcp_nodelay = true
tcp_keepalive = 60

[persistence]
enabled = false
snapshot_path = "redistill.rdb"
snapshot_interval = 300
save_on_shutdown = true
```

## Configuration Sections

### Server Configuration

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `bind` | string | "127.0.0.1" | IP address to bind (use "0.0.0.0" for all interfaces) |
| `port` | integer | 6379 | Port to listen on |
| `num_shards` | integer | 256 | Number of internal shards (power of 2 recommended) |
| `batch_size` | integer | 16 | Commands batched before flushing (higher = better throughput) |
| `buffer_size` | integer | 16384 | Per-connection buffer size in bytes |
| `buffer_pool_size` | integer | 1024 | Number of pre-allocated buffers |
| `max_connections` | integer | 10000 | Maximum concurrent connections (0 = unlimited) |
| `connection_timeout` | integer | 300 | Idle connection timeout in seconds (0 = no timeout) |
| `connection_rate_limit` | integer | 0 | Maximum new connections per second (0 = unlimited) |
| `health_check_port` | integer | 0 | HTTP health check port (0 = disabled) |

### Security Configuration

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `password` | string | "" | Authentication password (empty = no auth) |
| `tls_enabled` | boolean | false | Enable TLS encryption |
| `tls_cert_path` | string | "" | Path to TLS certificate file (PEM format) |
| `tls_key_path` | string | "" | Path to TLS private key file (PEM format) |

### Memory Configuration

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `max_memory` | integer | 0 | Maximum memory in bytes (0 = unlimited) |
| `eviction_policy` | string | "allkeys-lru" | Eviction policy: allkeys-lru, allkeys-random, allkeys-s3fifo, noeviction |
| `eviction_sample_size` | integer | 5 | Number of keys sampled for eviction (higher = better, slower) |

### Logging Configuration

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `level` | string | "info" | Log level: error, warn, info, debug, trace |
| `format` | string | "text" | Log format: text, json |

### Performance Configuration

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `tcp_nodelay` | boolean | true | Disable Nagle's algorithm (recommended for low latency) |
| `tcp_keepalive` | integer | 60 | TCP keepalive interval in seconds (0 = disabled) |

### Persistence Configuration

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `enabled` | boolean | false | Enable snapshot-based persistence (disabled by default for max speed) |
| `snapshot_path` | string | "redistill.rdb" | Path to snapshot file |
| `snapshot_interval` | integer | 300 | Auto-save interval in seconds (0 = disabled, default: 300 = 5 min) |
| `save_on_shutdown` | boolean | true | Save snapshot on graceful shutdown (SIGTERM/Ctrl+C) |

## Environment Variables

Environment variables override configuration file settings:

| Variable | Overrides | Example |
|----------|-----------|---------|
| `REDISTILL_CONFIG` | Config file path | `REDISTILL_CONFIG=/etc/redistill.toml` |
| `REDIS_PASSWORD` | `security.password` | `REDIS_PASSWORD=secret` |
| `REDIS_PORT` | `server.port` | `REDIS_PORT=6380` |
| `REDIS_BIND` | `server.bind` | `REDIS_BIND=0.0.0.0` |
| `REDIS_HEALTH_CHECK_PORT` | `server.health_check_port` | `REDIS_HEALTH_CHECK_PORT=8080` |
| `REDIS_NUM_SHARDS` | `server.num_shards` | `REDIS_NUM_SHARDS=2048` |
| `REDIS_BATCH_SIZE` | `server.batch_size` | `REDIS_BATCH_SIZE=256` |
| `REDIS_BUFFER_SIZE` | `server.buffer_size` | `REDIS_BUFFER_SIZE=16384` |
| `REDIS_BUFFER_POOL_SIZE` | `server.buffer_pool_size` | `REDIS_BUFFER_POOL_SIZE=2048` |
| `REDIS_MAX_CONNECTIONS` | `server.max_connections` | `REDIS_MAX_CONNECTIONS=10000` |
| `REDIS_MAX_MEMORY` | `memory.max_memory` | `REDIS_MAX_MEMORY=2147483648` |
| `REDIS_EVICTION_POLICY` | `memory.eviction_policy` | `REDIS_EVICTION_POLICY=allkeys-lru` |
| `REDIS_TCP_NODELAY` | `performance.tcp_nodelay` | `REDIS_TCP_NODELAY=true` |
| `REDIS_TCP_KEEPALIVE` | `performance.tcp_keepalive` | `REDIS_TCP_KEEPALIVE=60` |
| `REDIS_PERSISTENCE_ENABLED` | `persistence.enabled` | `REDIS_PERSISTENCE_ENABLED=true` |
| `REDIS_SNAPSHOT_PATH` | `persistence.snapshot_path` | `REDIS_SNAPSHOT_PATH=/data/redistill.rdb` |
| `REDIS_SNAPSHOT_INTERVAL` | `persistence.snapshot_interval` | `REDIS_SNAPSHOT_INTERVAL=600` |
| `REDIS_SAVE_ON_SHUTDOWN` | `persistence.save_on_shutdown` | `REDIS_SAVE_ON_SHUTDOWN=true` |

## Example Configurations

### Production High-Performance

```toml
[server]
bind = "0.0.0.0"
port = 6379
num_shards = 512
batch_size = 32
max_connections = 50000
connection_rate_limit = 5000
health_check_port = 8080

[security]
password = "your_secure_password"
tls_enabled = true
tls_cert_path = "/etc/letsencrypt/live/domain.com/fullchain.pem"
tls_key_path = "/etc/letsencrypt/live/domain.com/privkey.pem"

[memory]
max_memory = 8589934592  # 8GB
eviction_policy = "allkeys-lru"

[performance]
tcp_nodelay = true
tcp_keepalive = 120

[persistence]
enabled = true
snapshot_path = "/data/redistill.rdb"
snapshot_interval = 300
save_on_shutdown = true
```

### Development Local

```toml
[server]
bind = "127.0.0.1"
port = 6379
num_shards = 64

[security]
password = ""  # No auth for local dev

[memory]
max_memory = 1073741824  # 1GB

[logging]
level = "debug"
```

### Low Latency

```toml
[server]
bind = "0.0.0.0"
port = 6379
num_shards = 128
batch_size = 1  # Immediate flush
buffer_size = 8192

[security]
password = "your_password"

[performance]
tcp_nodelay = true  # Critical for low latency
```

## Performance Tuning

### num_shards

- **Higher values**: Better parallelism, reduced contention
- **Lower values**: Less memory overhead
- **Recommended**: 256 for most workloads, 512 for high concurrency
- **Note**: Use power of 2 for optimal hash distribution

### batch_size

- **Higher values** (32-64): Better throughput with pipelining
- **Lower values** (1-4): Lower latency for interactive commands
- **Recommended**: 16 for balanced performance

### buffer_size

- **Larger buffers** (32KB): Better for large values, fewer allocations
- **Smaller buffers** (8KB): Lower memory per connection
- **Recommended**: 16KB (16384 bytes)

### max_connections

- Set based on expected load
- Monitor `rejected_connections` in INFO command
- Consider system file descriptor limits (`ulimit -n`)

### connection_rate_limit

- Protects against connection floods
- Set to 10x normal connection rate
- Monitor `rejected_connections` for tuning

## Persistence

### Snapshot-Based Persistence

Redistill supports optional snapshot-based persistence for warm restarts. When enabled, snapshots are saved to disk periodically and on graceful shutdown.

**Key Features:**
- **Zero performance impact when disabled** (default)
- Background snapshots (non-blocking)
- Automatic snapshot loading on startup
- Configurable snapshot interval
- Save on graceful shutdown

**When to Enable:**
- Need warm restarts after server restarts
- Can tolerate slight performance overhead
- Want to preserve data across deployments

**When to Keep Disabled:**
- Maximum performance is critical
- Data can be regenerated
- Using external persistence layer

### Snapshot Configuration

```toml
[persistence]
enabled = true                    # Enable persistence
snapshot_path = "redistill.rdb"   # Snapshot file path
snapshot_interval = 300           # Auto-save every 5 minutes (0 = disabled)
save_on_shutdown = true           # Save on graceful shutdown
```

**Snapshot Interval:**
- `0`: Disable automatic snapshots (only manual SAVE/BGSAVE)
- `300`: Save every 5 minutes (recommended)
- `600`: Save every 10 minutes (lower overhead)
- `60`: Save every minute (higher overhead, more frequent saves)

**Snapshot Commands:**
- `SAVE`: Synchronous snapshot (blocks until complete)
- `BGSAVE`: Background snapshot (returns immediately)
- `LASTSAVE`: Get timestamp of last successful save

## Memory Management

### Memory Limit Calculation

```
max_memory = (available_RAM * 0.8) - (overhead)

Example: 10GB RAM server
max_memory = (10GB * 0.8) - 1GB = 7GB = 7516192768
```

### Eviction Policies

**allkeys-lru**:
- Evicts least recently used keys
- Best for caching workloads
- Recommended for most use cases

**allkeys-random**:
- Evicts random keys
- Faster than LRU
- Use when all keys have equal importance

**allkeys-s3fifo**:
- Uses S3-FIFO (Simple and Scalable caching with three Static FIFO queues) algorithm
- Up to 72% lower miss ratios than LRU
- Lock-free operations for high throughput
- Best for workloads with many one-hit wonders
- Uses three queues: Small (10%), Main (90%), and Ghost (metadata)

**noeviction**:
- Returns error when memory full
- Use when data loss is unacceptable
- Application must handle OOM errors

## Security Best Practices

1. **Always set password in production**
2. **Use TLS for external access**
3. **Bind to specific IP when possible**
4. **Set connection limits**
5. **Enable rate limiting**
6. **Use strong passwords** (16+ characters, random)
7. **Rotate credentials regularly**
8. **Monitor rejected_connections** for attack attempts

## Monitoring Configuration

### Key Metrics to Watch

1. `active_connections` - Current load
2. `rejected_connections` - Security/capacity issues
3. `used_memory` / `max_memory` - Memory pressure
4. `evicted_keys` - Cache efficiency
5. `total_commands` - Throughput

### Health Check Endpoint

Enable HTTP health check:

```toml
[server]
health_check_port = 8080
```

Access at: `http://localhost:8080/health`

Returns JSON with server status, useful for:
- Load balancers
- Kubernetes probes
- Monitoring systems

## Troubleshooting

### High Memory Usage

```toml
[memory]
max_memory = 2147483648  # Set appropriate limit
eviction_policy = "allkeys-lru"
```

### Low Throughput

```toml
[server]
num_shards = 512  # More parallelism
batch_size = 32   # Larger batches
buffer_size = 32768
```

### High Latency

```toml
[server]
batch_size = 1  # Immediate flush

[performance]
tcp_nodelay = true  # Must be enabled
```

### Connection Issues

```toml
[server]
max_connections = 50000  # Increase limit
connection_rate_limit = 10000  # Allow more connections/sec
```

Check system limits:
```bash
ulimit -n  # Check file descriptor limit
ulimit -n 100000  # Increase if needed
```

## Configuration Validation

Redistill validates configuration on startup. Common errors:

- **Invalid bind address**: Check IP format
- **Port in use**: Another service using the port
- **TLS cert not found**: Verify certificate paths
- **Invalid memory value**: Must be positive integer or 0
- **Invalid eviction policy**: Check spelling

Check logs for detailed error messages.
