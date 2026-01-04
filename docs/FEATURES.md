# Redistill Features

## Implemented Features

### Core Functionality

**Protocol**: Full Redis RESP protocol implementation
- Compatible with all Redis clients
- Works with redis-cli
- Supports pipelining

**Storage**: In-memory key-value store
- Fast hash-based lookups
- Sharded architecture (256 shards default)
- Zero-copy operations

**TTL Support**: Automatic key expiration
- `SET key value EX seconds`
- Lazy deletion on access
- Memory reclamation

### Supported Commands

#### Data Commands
- `SET key value [EX seconds | PX milliseconds] [NX | XX] [GET]` - Store with options
  - `EX seconds` - Set expiry in seconds
  - `PX milliseconds` - Set expiry in milliseconds
  - `NX` - Only set if key does **not** exist (distributed locks)
  - `XX` - Only set if key **does** exist (update only)
  - `GET` - Return the old value before setting
- `GET key` - Retrieve value
- `DEL key [key ...]` - Delete one or more keys
- `EXISTS key [key ...]` - Check key existence
- `MSET key value [key value ...]` - Set multiple keys atomically
- `MGET key [key ...]` - Get multiple keys in one call
- `KEYS` - List all keys (use with caution in production)
- `DBSIZE` - Get total key count
- `FLUSHDB` - Clear all keys

#### Counter Commands
- `INCR key` - Increment integer value by 1 (creates key with value 1 if not exists)
- `DECR key` - Decrement integer value by 1 (creates key with value -1 if not exists)
- `INCRBY key increment` - Increment integer value by specified amount
- `DECRBY key decrement` - Decrement integer value by specified amount

#### TTL Commands
- `EXPIRE key seconds` - Set timeout on existing key
- `TTL key` - Get remaining time to live in seconds (-1 = no TTL, -2 = key doesn't exist)
- `PTTL key` - Get remaining time to live in milliseconds
- `PERSIST key` - Remove the timeout from a key (make it permanent)

#### Server Commands
- `PING` - Health check
- `AUTH password` - Authenticate connection
- `INFO` - Server statistics
- `CONFIG GET` - Configuration stub (compatibility)
- `COMMAND` - Command list stub (compatibility)

### Security Features

**Authentication**:
- Password-based authentication
- Configurable via file or environment variable
- Per-connection state tracking

**TLS/SSL**:
- Optional TLS encryption
- Certificate-based security
- Compatible with Let's Encrypt

**Connection Management**:
- Maximum connection limits
- Connection rate limiting
- Automatic rejection on limit breach

### Memory Management

**Memory Limits**:
- Configurable maximum memory
- Real-time usage tracking
- Memory-based admission control

**Eviction Policies**:
- **allkeys-lru**: Least Recently Used eviction
- **allkeys-random**: Random key eviction
- **noeviction**: Reject writes when full

**Monitoring**:
- Current memory usage
- Eviction statistics
- Memory efficiency metrics

### Reliability Features

**Graceful Shutdown**:
- Signal handling (SIGTERM, SIGINT)
- Connection cleanup
- Final statistics reporting

**Health Monitoring**:
- HTTP health check endpoint
- JSON status response
- Integration with load balancers

**Error Handling**:
- Protocol error recovery
- Connection error handling
- Timeout management

### Performance Optimizations

**Concurrency**:
- Lock-free concurrent hash map (DashMap)
- Sharded architecture
- Multi-threaded request handling

**Memory Efficiency**:
- Zero-copy byte operations
- Buffer pooling
- Minimal allocations

**I/O Optimization**:
- Batched writes (configurable)
- Smart flushing
- TCP tuning (nodelay, keepalive)

### Monitoring and Observability

**Metrics Tracked**:
- Total connections (lifetime)
- Active connections (current)
- Rejected connections
- Total commands processed
- Memory usage
- Evicted keys
- Server uptime

**Access Methods**:
- `INFO` command (Redis protocol)
- HTTP health endpoint (JSON)
- Real-time statistics

### Persistence (Optional)

Redistill supports **optional snapshot persistence** for warm restarts. Disabled by default for maximum performance.

**Commands:**
- `SAVE` - Synchronous snapshot (blocks until complete)
- `BGSAVE` - Background snapshot (returns immediately)
- `LASTSAVE` - Unix timestamp of last successful save
- `FLUSHALL` - Clear all keys

**Configuration:**
```toml
[persistence]
enabled = false                   # Enable persistence (default: false)
snapshot_path = "redistill.rdb"   # Snapshot file path
snapshot_interval = 300           # Auto-save interval in seconds (0 = disabled)
save_on_shutdown = true           # Save on graceful shutdown
```

**Features:**
- Streaming serialization
- Atomic file writes
- Automatic snapshot loading on startup
- Background saves don't block request handling
- Skips expired keys during save/load

**Note:** This is snapshot-only persistence (RDB-like). No AOF/write-ahead logging. Data loss possible between snapshots.

## Not Implemented

The following Redis features are intentionally not implemented:

### Write-Ahead Logging (AOF)

**Excluded**: AOF append-only file, fsync modes

**Rationale**: AOF adds write latency. Snapshot persistence is sufficient for warm restarts.

### Replication

**Excluded**: Master-replica replication, REPLICAOF, Sentinel

**Rationale**: Focus on single-instance performance. Use client-side sharding or proxies for distribution.

### Data Structures

**Excluded**: Lists, Sets, Sorted Sets, Hashes, Streams, Bitmaps, HyperLogLog

**Rationale**: Core key-value operations provide maximum performance. Advanced data structures add complexity and overhead.

### Pub/Sub

**Excluded**: PUBLISH, SUBSCRIBE, PSUBSCRIBE, channels

**Rationale**: Different architecture required. Use dedicated message broker if needed.

### Transactions

**Excluded**: MULTI, EXEC, WATCH, DISCARD

**Rationale**: Adds coordination overhead. Most caching use cases don't require transactions.

### Scripting

**Excluded**: Lua scripting (EVAL, EVALSHA)

**Rationale**: Complex feature with security implications. Not needed for high-performance caching.

### Cluster Mode

**Excluded**: Cluster commands, hash slots, cluster topology

**Rationale**: Single-node optimization first. Use client-side sharding or proxies for scaling.

## Future Considerations

Potential additions based on user feedback:

### High Priority
- Key scanning (`SCAN` for safer iteration than KEYS)
- `APPEND key value` - Append to string
- `STRLEN key` - Get string length
- `INCRBYFLOAT key increment` - Float counter support

### Medium Priority
- Basic list operations (LPUSH, RPUSH, LPOP, RPOP, LRANGE)
- Basic set operations (SADD, SMEMBERS, SREM)
- CLIENT command (list/kill connections)
- Pub/Sub (PUBLISH, SUBSCRIBE)

### Low Priority
- Clustering support (hash slots, node discovery)
- Optional persistence mode (behind feature flag)
- Replication support
- Basic hash operations (HSET, HGET, HGETALL)

**Note**: New features will only be added if they don't compromise core performance objectives.

## Performance Characteristics

| Metric | Value |
|--------|-------|
| Throughput (P=16) | 2.1M ops/s |
| Throughput (P=64) | 4.7M ops/s |
| Latency (p50) | 0.20ms |
| Memory overhead | Minimal |
| CPU scaling | Linear with cores |
| Startup time | < 1 second |

## Use Case Fit

### Excellent Fit

**Session Storage**: High read/write ratio, TTL support, high throughput

**API Caching**: Fast reads, TTL-based expiration, memory limits

**Rate Limiting**: Counter operations, TTL support, low latency

**Temporary Data**: High churn rate, no persistence needed

### Poor Fit

**Persistent Storage**: No disk persistence

**Complex Queries**: Only key-based lookup

**Large Datasets**: In-memory only

**Multi-DC**: No built-in replication

## Comparison with Redis

| Feature | Redistill | Redis |
|---------|-----------|-------|
| Protocol | RESP | RESP |
| Throughput | 9.07M ops/s | 2.03M ops/s |
| Latency (p50) | 0.48ms | 2.38ms |
| Data types | String + Counters | String, List, Set, Hash, etc. |
| Counter commands | INCR, DECR, INCRBY, DECRBY | Full set |
| TTL commands | EXPIRE, TTL, PTTL, PERSIST | Full set |
| Bulk operations | MGET, MSET | Full set |
| Conditional SET | NX, XX, GET options | Full set |
| Persistence | None | AOF, RDB |
| Replication | None | Master-replica |
| Clustering | None | Redis Cluster |
| Memory management | LRU eviction | Multiple policies |
| TLS | Yes | Yes |
| Authentication | Password | Password, ACL |
| Use case | High-performance cache | General purpose |

## Technical Implementation

**Language**: Rust
**Async Runtime**: Tokio
**Concurrency**: DashMap (lock-free)
**Memory**: jemalloc allocator
**Protocol**: RESP (Redis Serialization Protocol)

**Design Principles**:
- Zero-copy where possible
- Minimize allocations
- Lock-free data structures
- Batched I/O operations
- No blocking operations

## Version Compatibility

**Redis Protocol**: Compatible with RESP2
**Client Libraries**: Any Redis client library
**redis-cli**: Fully compatible
**redis-benchmark**: Fully compatible

**Tested With**:
- redis-py (Python)
- ioredis (Node.js)
- go-redis (Go)
- Jedis (Java)
- redis-cli 7.0+
