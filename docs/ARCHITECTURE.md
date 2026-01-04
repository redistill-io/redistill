# Architecture & Design Philosophy

This document explains the design decisions, architecture, and trade-offs that make Redistill a high-performance Redis-compatible cache.

## Table of Contents

- [Design Philosophy](#design-philosophy)
- [Architecture Overview](#architecture-overview)
- [Performance Optimizations](#performance-optimizations)
- [Trade-offs](#trade-offs)
- [Use Cases](#use-cases)
- [When to Use Redistill vs Redis](#when-to-use-redistill-vs-redis)

## Design Philosophy

Redistill is built on a simple principle: **performance first, optional persistence**. By default, Redistill eliminates persistence overhead to maximize speed, throughput, and low latency. For users who need storage, optional snapshot persistence is available without impacting the hot path.

### Core Principles

1. **Optional Persistence** - Zero disk I/O overhead by default; snapshot support available when needed
2. **Multi-threaded** - Leverage all CPU cores
3. **Lock-free Reads** - Concurrent access without blocking
4. **Zero-copy** - Minimize memory allocations
5. **Production-ready** - Security, monitoring, and reliability built-in

## Architecture Overview

![Redistill Architecture](img/Redistill_Architecture.png "Redistill Architecture")

## Performance Optimizations

### 1. Optional Persistence Layer

**What Redis Does:**
- Writes to AOF (Append-Only File) on every write
- Periodic RDB snapshots
- fsync() calls for durability

**What Redistill Does (Default):**
- No persistence by default. All data lives in memory only.
- Zero disk I/O overhead on the hot path

**What Redistill Does (Optional):**
- Snapshot-based persistence (disabled by default)
- Background snapshots don't block request handling
- Streaming serialization
- Automatic snapshot loading on startup

**Impact:**
- **Default (disabled)**: Eliminates all disk I/O, no fsync() blocking, simpler code path
- **Optional (enabled)**: Warm restarts supported, zero impact on GET/SET performance, background saves only
- Data loss possible between snapshots (by design - not real-time durability)

### 2. Multi-threaded Architecture

**What Redis Does:**
- Single-threaded event loop
- All commands processed sequentially
- Background threads only for slow operations (BGSAVE, etc.)

**What Redistill Does:**
- Tokio multi-threaded runtime
- Concurrent command processing across all cores
- Work-stealing scheduler for load balancing

**Impact:**
- Utilizes all CPU cores
- Higher throughput under load
- Better resource utilization
- Slightly higher memory overhead for thread pools

### 3. Lock-free Reads with DashMap

**What Redis Does:**
- Single thread = no locks needed
- Linear access to hash table

**What Redistill Does:**
- DashMap: Concurrent hash map with fine-grained locking
- Multiple shards (default 2048) for reduced contention
- Read operations don't block each other
- AHash for hardware-accelerated hashing (AES-NI instructions)

**Impact:**
- Concurrent GET operations scale linearly with cores
- Minimal contention on different keys
- Optimized for read-heavy workloads
- 2-4x faster hashing than traditional algorithms
- Slightly higher memory per shard

**Shard Count Impact:**
```
256 shards:   High contention, lower memory
2048 shards:  Optimal balance (Recommended)
4096 shards:  Maximum GET performance, higher memory
```

### 4. Zero-copy Design with Bytes

**What Redis Does:**
- String allocations for each value
- Memory copying on reads

**What Redistill Does:**
- Uses `Bytes` crate for reference-counted buffers
- Cheap cloning via reference counting
- No data copying on reads

**Impact:**
- Reduced memory allocations
- Faster reads (no memcpy)
- Lower GC pressure
- Better cache locality

### 5. Batched Writes

**What Redis Does:**
- Processes pipelined commands individually
- Atomic counters updated per operation

**What Redistill Does:**
- Batches operations internally
- Probabilistic LRU (90% skip rate)
- Batched counter updates (256x reduction in atomics)

**Impact:**
- Reduced atomic operation overhead
- Better CPU cache utilization
- Matches Redis performance at extreme concurrency
- LRU timestamps slightly less accurate (acceptable trade-off)

### 6. Async I/O with Tokio

**What Redis Does:**
- epoll/kqueue for event loop
- Custom networking code

**What Redistill Does:**
- Tokio for async networking
- Buffered I/O with per-connection async tasks

**Impact:**
- Battle-tested async runtime
- Efficient syscall management
- Better error handling
- Future-proof (Tokio improvements benefit Redistill)

### 7. Optional Snapshot Persistence

**What Redis Does:**
- AOF (Append-Only File) with fsync() calls
- RDB snapshots with fork() overhead
- Both enabled by default

**What Redistill Does:**
- **Default**: No persistence (zero overhead)
- **Optional**: Snapshot-based persistence (disabled by default)
  - Streaming serialization (constant memory)
  - Background saves (non-blocking)
  - Atomic file writes (temp file + rename)
  - Automatic loading on startup

**Impact:**
- **When disabled (default)**: Zero disk I/O, maximum performance
- **When enabled**: Warm restarts supported, zero impact on GET/SET hot path
- Background snapshots run in separate task, don't block requests
- Snapshot format: Binary (bincode) for fast serialization

**Configuration:**
```toml
[persistence]
enabled = false              # Default: OFF for max performance
snapshot_path = "redistill.rdb"
snapshot_interval = 300      # Auto-save every 5 minutes (0 = disabled)
save_on_shutdown = true      # Save on graceful shutdown
```

## Trade-offs

### What You Gain

| Feature | Benefit |
|---------|---------|
| **4.5x faster throughput** | More requests per server |
| **5x lower latency (p50)** | Better user experience |
| **Read-heavy optimization** | Perfect for caching workloads |
| **Multi-core utilization** | Better hardware efficiency |
| **Lower infrastructure cost** | 50-83% cost savings vs alternatives |

**Important Note on Performance Claims:**
- Redistill is the **fastest single-instance** Redis-compatible server (9.07M ops/s per instance)
- Performance comparisons (4.5x vs Redis, 1.7x vs Dragonfly) are **single-instance** benchmarks
- Redis Cluster or Dragonfly with clustering can achieve higher **total throughput** by scaling horizontally across multiple instances
- However, **per-instance**, Redistill delivers the highest performance, meaning you need fewer instances to achieve the same throughput

### What You Lose

| Feature | Impact | Mitigation |
|---------|--------|-----------|
| **Persistence (default)** | Data lost on restart | Optional snapshot persistence available; use as cache, not source of truth |
| **Real-time Durability** | No AOF/write-ahead logging | Snapshot persistence for warm restarts (optional) |
| **Replication** | No built-in HA | Use client-side sharding/proxy |
| **Clustering** | No automatic sharding | Manual sharding or proxy |
| **Complex data types** | Only strings | Fine for 90% of cache use cases |
| **Modules** | No Redis modules | Built-in features cover most needs |

### When Redistill Excels

**Read-heavy workloads** (70%+ reads)
- GET operations are 2-4x faster than Redis
- Lock-free reads scale with cores

**High concurrency** (50-300 clients)
- Multi-threaded architecture shines
- Better resource utilization

**Pipelined workloads** (P≥16)
- Batched operations reduce overhead
- Optimized buffer management

**Medium-large values** (256B-10KB)
- Zero-copy design benefits larger values
- Less overhead relative to data size

### When Redis is Better

**Need real-time durability**
- Redis has AOF (write-ahead logging) for real-time durability
- Redistill has optional snapshots (warm restarts) but not real-time durability

**Need replication/clustering**
- Redis has built-in clustering (can scale to higher total throughput with multiple instances)
- Dragonfly supports clustering (can achieve higher aggregate throughput across cluster)
- Redistill requires manual sharding (no built-in clustering, but single-instance performance is highest)

**Complex data structures**
- Redis supports Lists, Sets, Hashes, Sorted Sets
- Redistill only supports strings (KV pairs)

**Write-heavy extreme loads** (>50% writes, 500+ clients)
- Redis's single-threaded model can be faster at extreme write concurrency
- Redistill's multi-threading has coordination overhead

## Use Cases

### Perfect For

**1. HTTP Session Storage**
- Read-heavy (sessions read on every request)
- Ephemeral data (can regenerate from auth service)
- High throughput requirements
- TTL support built-in

**2. API Response Caching**
- 95%+ cache hit rates typical
- Database queries 50-150ms vs cache 1-2ms
- Can regenerate from database
- Perfect for read-heavy workloads

**3. Rate Limiting**
- Simple counters with TTL
- High write throughput needed
- Loss of counter on restart acceptable
- Sub-millisecond latency required

**4. Real-time Leaderboards**
- Frequent reads, periodic writes
- Can rebuild from database
- Performance critical for UX
- TTL for automatic cleanup

**5. Feature Flags**
- Read-heavy (checked on every request)
- Small data size
- Occasional updates
- Low latency critical

### Not Recommended For

**1. Primary Data Store**
- Reason: No persistence
- Alternative: Use PostgreSQL, MongoDB, etc.

**2. Financial Transactions**
- Reason: Data loss on restart unacceptable
- Alternative: Use ACID-compliant database

**3. Long-term Data Storage**
- Reason: In-memory only, data evicted
- Alternative: Use database or object storage

**4. Complex Queries**
- Reason: No secondary indexes, no filtering
- Alternative: Use database with proper indexing

## When to Use Redistill vs Redis

| Requirement | Redistill | Redis |
|-------------|-----------|-------|
| Maximum throughput (single-instance) | **Best choice** (9.07M ops/s) | Slower (2.03M ops/s) |
| Maximum throughput (clustered) | Manual sharding required | Redis Cluster scales horizontally |
| Lowest latency (p50) | **Best choice** (0.48ms) | Higher p50/p99 (2.38ms) |
| Read-heavy workload (80%+ reads) | 2-4x faster per instance | Slower per instance |
| Write-heavy workload (>50% writes) | Good | Better at extreme scale |
| Need persistence | Not supported | AOF/RDB |
| Need replication | Not built-in | Built-in |
| Need clustering | Manual sharding | Redis Cluster (built-in) |
| Complex data types | Strings only | Lists, Sets, Hashes, etc. |
| Drop-in Redis replacement | For caching | Check feature compatibility |
| Cost efficiency | 50-83% savings (fewer instances needed) | Higher costs |
| Multi-core utilization | All cores | Single threaded |

## Technical Implementation Details

### Concurrency Model

```rust
// Simplified architecture concept

// DashMap provides concurrent access
let store = DashMap::new();

// Read operations (lock-free)
async fn handle_get(key: &str) -> Option<Bytes> {
    store.get(key).map(|entry| entry.value().clone())
}

// Write operations (fine-grained locking)
async fn handle_set(key: String, value: Bytes) {
    store.insert(key, value);
}

// Multiple concurrent reads don't block each other
// Writes only lock the specific shard containing the key
```

### Memory Management

```
Key anatomy in memory:
┌────────────────────────────────────────┐
│ Key (Bytes)                            │  Variable (key length)
├────────────────────────────────────────┤
│ Value (Bytes - reference counted)      │  Variable (value length)
├────────────────────────────────────────┤
│ Timestamp (u32)                        │  4 bytes (AtomicU32)
├────────────────────────────────────────┤
│ TTL (Option<u64>)                      │  16 bytes
├────────────────────────────────────────┤
│ DashMap overhead                       │  ~32 bytes
├────────────────────────────────────────┤
│ Entry struct overhead                  │  ~12 bytes
└────────────────────────────────────────┘

Fixed overhead per key: ~64 bytes
Total size: key_len + value_len + 64 bytes
```

### Eviction Strategy

**LRU (Least Recently Used):**
- Probabilistic updates (90% skip rate)
- On-demand eviction during write operations
- Configurable memory limit

**Algorithm:**
1. On SET operation, check if memory limit would be exceeded
2. If eviction needed, sample random shards (default: 5 samples)
3. From each shard, examine first entry (effectively random due to hash distribution)
4. Evict key with oldest `last_accessed` timestamp
5. Repeat until sufficient memory is freed
6. If no eviction policy set, reject write with OOM error

**Note:** Eviction is synchronous and happens during write operations, not in a background task. TTL expiration runs separately in a background task every 100ms.

## Future Roadmap

### Planned Features

- [ ] **Clustering support** - Built-in sharding
- [ ] **Replication** - Master-slave replication
- [x] **Snapshot support** - Optional persistence (✅ Implemented)
- [ ] **Pub/Sub** - Message broadcasting
- [ ] **Lua scripting** - Custom commands
- [ ] **More data types** - Lists, Sets, Hashes

### Performance Improvements

- [ ] **DPDK support** - Kernel bypass networking
- [ ] **NUMA awareness** - Better multi-socket performance
- [ ] **jemalloc tuning** - Optimized allocator settings
- [ ] **CPU pinning** - Thread affinity for better cache locality

## Contributing

We welcome contributions! Areas that need help:

1. **Performance testing** - More real-world benchmarks
2. **Feature implementation** - See roadmap above
3. **Documentation** - Usage examples, tutorials
4. **Bug reports** - Edge cases, race conditions
5. **Client libraries** - Language-specific optimizations

## See Also

- [Benchmarks](BENCHMARKS.md) - Performance testing methodology
- [Performance Tuning Guide](PERFORMANCE_TUNING.md) - Optimization tips
- [Production Guide](PRODUCTION_GUIDE.md) - Deployment best practices
- [Configuration Reference](CONFIG.md) - All configuration options

