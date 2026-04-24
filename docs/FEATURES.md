# Redistill Features

## Implemented Features

### Core Functionality

**Protocol**: Full Redis RESP protocol implementation
- Compatible with all Redis clients
- Works with redis-cli
- Supports pipelining

**Storage**: In-memory key-value store
- Fast hash-based lookups
- Sharded architecture (256 shards default, 2048 recommended for production)
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
- `KEYS pattern` - List all keys matching pattern (use with caution in production - blocks server)
- `SCAN cursor [MATCH pattern] [COUNT count]` - Iterate keys safely (non-blocking)
  - `cursor` - Cursor value (0 to start, or previous cursor from last scan)
  - `MATCH pattern` - Optional glob pattern (e.g., `user:*`, `session:?*`)
  - `COUNT count` - Optional hint for number of keys to return (default: 10)
  - Returns: `[cursor, [key1, key2, ...]]` - cursor 0 indicates completion
- `DBSIZE` - Get total key count
- `FLUSHDB` - Clear all keys

#### Counter Commands
- `INCR key` - Increment integer value by 1 (creates key with value 1 if not exists)
- `DECR key` - Decrement integer value by 1 (creates key with value -1 if not exists)
- `INCRBY key increment` - Increment integer value by specified amount
- `DECRBY key decrement` - Decrement integer value by specified amount

#### Hash Commands
- `HSET key field value [field value ...]` - Set one or more field-value pairs in a hash
  - Returns: Integer count of fields that were newly set (1 if new, 0 if updated)
  - Creates hash if key doesn't exist
  - Updates existing fields if they already exist
  - Example: `HSET user:1001 name "John" age "30" city "NYC"` returns `3`
- `HGET key field` - Get value of a field in a hash
  - Returns: Bulk string value or null if field/key doesn't exist
  - Example: `HGET user:1001 name` returns `"John"`
- `HGETALL key` - Get all field-value pairs from a hash
  - Returns: Array of alternating field-value pairs `[field1, value1, field2, value2, ...]`
  - Returns empty array if key doesn't exist or is empty hash
  - Example: `HGETALL user:1001` returns `["name", "John", "age", "30", "city", "NYC"]`
  - **Note**: Field order is not guaranteed (matches Redis behavior)

**Hash Features:**
- Thread-safe concurrent access to hash fields
- Memory-efficient storage with automatic cleanup
- Full persistence support (saved/loaded in snapshots)
- Type safety: Operations return `WRONGTYPE` error if key exists as different type

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
- **allkeys-s3fifo**: S3-FIFO algorithm (up to 72% lower miss ratios than LRU)
- **noeviction**: Reject writes when full

**Monitoring**:
- Current memory usage
- Eviction statistics
- Memory efficiency metrics

### Reliability Features

**Graceful Shutdown**:
- Signal handling for both **SIGTERM** (Kubernetes/systemd) and **SIGINT** (Ctrl-C) on Unix; Ctrl-C on Windows
- Listener is dropped immediately → no new connections accepted
- In-flight connections are drained within `shutdown_grace_period_secs` (default 30s); handlers finish their current command and exit on the next read
- Background tasks (expiration, periodic snapshot, AOF everysec, AOF rewrite supervisor, health check) stop cleanly on a shared shutdown watch channel
- A **second** SIGTERM/SIGINT forces immediate abort of in-flight connections for fast pod rotation
- Final AOF fsync + final RDB snapshot (when enabled) run before exit, each with its own timeout bound so a huge dataset can't hang the process
- Final statistics printed to stdout

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

Redistill supports **two independent persistence mechanisms**: RDB snapshots and AOF. Both are disabled by default for maximum performance. You can enable either, both, or neither.

**On startup** (matches Redis semantics):
1. If `persistence.enabled`, load the RDB snapshot.
2. If `persistence.aof_enabled`, replay the AOF on top. The AOF is fed through the same command dispatch code path as live traffic, so replay semantics can never diverge from live semantics.
3. A malformed AOF aborts startup rather than silently losing data.

#### RDB (point-in-time snapshots)

**Commands:**
- `SAVE` - Synchronous snapshot (blocks until complete)
- `BGSAVE` - Background snapshot (returns immediately)
- `LASTSAVE` - Unix timestamp of last successful save

**Configuration:**
```toml
[persistence]
enabled = false                   # Enable RDB
snapshot_path = "redistill.rdb"   # Snapshot file path
snapshot_interval = 300           # Auto-save interval in seconds (0 = disabled)
save_on_shutdown = true           # Save on graceful shutdown
```

**Features:**
- Streaming bincode serialization
- Atomic file writes via rename
- Automatic snapshot loading on startup
- Background saves don't block request handling
- Skips expired keys during save/load

**Loss window:** up to one `snapshot_interval` (default 5 min) on hard crash. Acceptable for cache workloads; pair with AOF if unacceptable.

#### AOF (append-only file)

Every successful write command (SET, DEL, INCR/DECR/INCRBY/DECRBY, MSET, EXPIRE, PERSIST, HSET, HDEL) is appended to a log file in RESP format. On startup the log is replayed to reconstruct live state.

**Configuration:**
```toml
[persistence]
aof_enabled = false               # Enable AOF
aof_path = "redistill.aof"        # AOF file path
aof_fsync = "everysec"            # Durability policy (see below)
aof_rewrite_min_size = 67108864   # Min bytes before auto-rewrite is considered (64 MiB)
aof_rewrite_percentage = 100      # Auto-rewrite when log has grown this % past the last-rewrite size (0 = manual only)
```

**Fsync policies:**

| Policy | Loss window on crash | Write-path cost | When to use |
|---|---|---|---|
| `always` | Zero | ~550× slower (per-write fsync) | Financial-grade durability |
| `everysec` (default) | ≤1 second | ~0% at P=1, ~20% at P=16+ | **The sweet spot** — matches Redis default |
| `no` | Kernel dependent (~30s on Linux) | Negligible | Maximum speed with best-effort durability |

**Commands:**
- `BGREWRITEAOF` - Trigger asynchronous log compaction. Errors with `-AOF is disabled` or `-…already in progress` as appropriate. Replies `+Background append only file rewriting started` immediately; the rewrite runs on a blocking task.

**AOF rewrite (compaction):**

Without compaction, the AOF grows unboundedly: 10M `INCR foo` commands = 10M log entries for 8 bytes of state. Rewrite walks the live store and emits a single `SET`/`HSET` per key (plus `EXPIRE` for keys with TTL), atomically replacing the file.

- **Manual trigger:** `BGREWRITEAOF` command.
- **Automatic trigger:** background supervisor polls every 10s; rewrites when `size ≥ last_rewrite_size × (1 + pct/100)` AND `size ≥ aof_rewrite_min_size`. Set `aof_rewrite_percentage = 0` to disable auto-rewrite.
- **Current implementation is stop-the-world:** writes block for the duration of the snapshot walk (O(live keys)). Acceptable at the default 64 MiB trigger. Concurrent rewrite (diff-during-snapshot) is a future optimization.

**Known performance characteristics** (measured, MacBook-class SSD, `-n 500000 -c 50 -d 64`):

| Mode | SET P=1 | SET P=16 | SET P=128 | GET (any P) |
|------|--------:|---------:|----------:|-------------|
| RDB only / none | 150–170k rps | 1.9M | 3.1M | unchanged |
| `aof-no` / `aof-everysec` | 170k (≈none) | ~1.5M (−20%) | ~1.7M (−45%) | unchanged |
| `aof-always` | ~270 rps | unusable | unusable | unchanged |

Write-path overhead at P=16+ comes from the single `Mutex<BufWriter<File>>` serializing appends. At interactive pipeline depths (P=1–8) the mutex is effectively uncontended. The matrix is reproducible via `tests/benchmarks/persistence_bench.sh`.

## Not Implemented

The following Redis features are intentionally not implemented:

### Replication

**Excluded**: Master-replica replication, REPLICAOF, Sentinel

**Rationale**: Focus on single-instance performance. Use client-side sharding or proxies for distribution.

### Data Structures

**Excluded**: Lists, Sets, Sorted Sets, Streams, Bitmaps, HyperLogLog

**Rationale**: Core key-value operations and hashes provide maximum performance. Additional data structures add complexity and overhead.

**Note**: Hash data structure is now implemented (HSET, HGET, HGETALL) for storing field-value pairs within a key.

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
- Additional hash operations (HDEL, HKEYS, HVALS, HLEN, HEXISTS, HMSET, HMGET)

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

**Complex Queries**: Only key-based lookup

**Large Datasets**: In-memory only

**Multi-DC**: No built-in replication

## Comparison with Redis

| Feature | Redistill | Redis |
|---------|-----------|-------|
| Protocol | RESP | RESP |
| Throughput | 9.07M ops/s | 2.03M ops/s |
| Latency (p50) | 0.48ms | 2.38ms |
| Data types | String + Counters + Hash | String, List, Set, Hash, etc. |
| Counter commands | INCR, DECR, INCRBY, DECRBY | Full set |
| TTL commands | EXPIRE, TTL, PTTL, PERSIST | Full set |
| Bulk operations | MGET, MSET | Full set |
| Key scanning | SCAN (non-blocking), KEYS (blocking) | Full set |
| Conditional SET | NX, XX, GET options | Full set |
| Persistence | RDB + AOF (both optional, independent) | AOF, RDB |
| Replication | None | Master-replica |
| Clustering | None | Redis Cluster |
| Memory management | LRU, Random, S3-FIFO eviction | Multiple policies |
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
