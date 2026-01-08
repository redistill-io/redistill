# Benchmarks

Comprehensive benchmark results for Redistill across different configurations and workloads.

## Table of Contents

- [Quick Summary](#quick-summary)
- [Competitive Benchmark (c7i.16xlarge)](#competitive-benchmark-c7i16xlarge)
- [Detailed Results (c7i.8xlarge)](#detailed-results-c7i8xlarge)
- [Configuration Impact](#configuration-impact)
- [How to Run Benchmarks](#how-to-run-benchmarks)

## Quick Summary

**Best Performance (c7i.16xlarge with memtier_benchmark):**
- **Throughput:** 9.07M ops/s (4.5x faster than Redis, 1.7x faster than Dragonfly)
- **Latency (p50):** 0.479ms (5x faster than Redis, 1.7x faster than Dragonfly)
- **Bandwidth:** 1.58 GB/s (4.7x faster than Redis, 1.7x faster than Dragonfly)

## Competitive Benchmark (c7i.16xlarge)

Independent comparison on **AWS c7i.16xlarge** (Intel, 64 cores, 128GB RAM) using memtier_benchmark with production-like configuration.

### Test Configuration

- Duration: 60 seconds
- Threads: 8, Connections: 160 (20 per thread)
- Pipeline: 30, Data size: 256 bytes
- Workload: 1:1 SET:GET ratio

### Results

| Metric | Redistill | Dragonfly | Redis | vs Redis | vs Dragonfly |
|--------|-----------|-----------|-------|----------|--------------|
| **Throughput** | 9.07M ops/s | 5.43M ops/s | 2.03M ops/s | **4.5x** | **1.7x** |
| **Bandwidth** | 1.58 GB/s | 923 MB/s | 337 MB/s | **4.7x** | **1.7x** |
| **Avg Latency** | 0.524 ms | 0.877 ms | 2.000 ms | **3.8x faster** | **1.7x faster** |
| **p50 Latency** | 0.479 ms | 0.807 ms | 2.383 ms | **5.0x faster** | **1.7x faster** |
| **p99 Latency** | 1.215 ms | 1.975 ms | 2.959 ms | **2.4x faster** | **1.2x faster** |
| **p99.9 Latency** | 1.591 ms | 2.559 ms | 4.159 ms | **2.6x faster** | **1.6x faster** |

### Key Observations

- Redistill processed 544M total operations (2.7x more than Dragonfly, 4.5x more than Redis)
- Consistent low latency across all percentiles
- No errors or connection issues across all systems
- Dragonfly showed higher max latency (23.167ms) compared to Redistill (6.431ms) and Redis (7.551ms)

### Visualization

**Throughput Comparison (Higher is Better)**

```
Redis       ████████████ 2.0M ops/s  (100%)
Dragonfly   ████████████████████████████████ 5.4M ops/s  (270%)
Redistill   ████████████████████████████████████████████████████████ 9.1M ops/s  (455%) ⭐
```

**Latency Comparison - p50 (Lower is Better)**

```
Redistill   ████████████ 0.479 ms  (100%) ⭐ Best
Dragonfly   ████████████████████████████ 0.807 ms  (168%)
Redis       ████████████████████████████████████████████████████████ 2.383 ms  (497%)
```

**Bandwidth Comparison (Higher is Better)**

```
Redis       ████████████ 338 MB/s  (100%)
Dragonfly   ████████████████████████████████ 923 MB/s  (273%)
Redistill   ████████████████████████████████████████████████████████ 1,580 MB/s  (467%) ⭐
```

> 📊 **Methodology:** Tests run with identical hardware and configuration using [memtier_benchmark](https://github.com/RedisLabs/memtier_benchmark). Raw results available in `tests/benchmarks/benchmark_results_memtier/`.

## Detailed Results (c7i.8xlarge)

Comprehensive benchmarks on **AWS c7i.8xlarge** (Intel, 32 cores) with optimal configuration using redis-benchmark.

### Configuration

- `num_shards=2048`
- `batch_size=256`
- `buffer_pool_size=2048`
- Batched atomic counters
- Probabilistic LRU updates

### Results by Workload

| Workload | Redistill | Redis | Improvement |
|----------|-----------|-------|-------------|
| Interactive (-c 1, -P 1) | 39K ops/s | 39K ops/s | Similar |
| Production SET (-c 50, -P 16) | 2.08M ops/s | 1.69M ops/s | **+23%** ✅ |
| Production GET (-c 50, -P 16) | 2.05M ops/s | 1.94M ops/s | **+6%** ✅ |
| High Concurrency SET (-c 300, -P 32) | 2.67M ops/s | 2.23M ops/s | **+20%** ✅ |
| High Concurrency GET (-c 300, -P 32) | 3.47M ops/s | 2.71M ops/s | **+28%** ✅ |
| Extreme SET (-c 100, -P 64) | 2.72M ops/s | 2.58M ops/s | **+5%** ✅ |
| Extreme GET (-c 100, -P 64) | 5.32M ops/s | 3.06M ops/s | **+74%** 🚀 |
| Ultra SET (-c 500, -P 128) | **2.74M ops/s** | **2.74M ops/s** | **Equal** ✅ |
| Ultra GET (-c 500, -P 128) | **6.87M ops/s** | **3.47M ops/s** | **+98%** 🚀 |

### Key Findings

**Redistill Matches or Beats Redis on ALL Workloads:**
- **GET operations: +74% to +98% faster** (nearly 2x!)
- **Production SET: +23% faster**
- **High concurrency SET: +20% faster**
- **Extreme concurrency SET: Matches Redis exactly**

**Perfect For:**
- Read-heavy caching (80-95% reads) - **massive advantage**
- Session storage, API response caching, rate limiting
- Production workloads with any concurrency level
- Scenarios requiring maximum GET throughput

## Configuration Impact

### Shard Count Impact

Tested on c7i.8xlarge with `batch_size=256` and `buffer_pool_size=2048`:

| Shards | Ultra SET | Ultra GET | Memory | Best For |
|--------|-----------|-----------|--------|----------|
| 256 | 2.03M ops/s | 6.25M ops/s | Low | Memory-constrained |
| **2048** | **2.74M ops/s** | **6.87M ops/s** | **Moderate** | **Recommended** |
| 4096 | 2.71M ops/s | 7.52M ops/s | High | GET-heavy workloads |

**Analysis:**
- **256 shards:** Lower memory but higher contention (250 ops/shard)
- **2048 shards:** Optimal balance - 31 ops/shard, best overall performance
- **4096 shards:** Maximum GET performance but diminishing returns on SET

### Key Optimizations (v1.0.4)

- **2048 shards:** 8x less contention (31 ops/shard vs 250 ops/shard with 256 shards)
- **Batched atomic counters:** 256x fewer atomic operations
- **Probabilistic LRU:** 90% skip rate on timestamp updates
- **Result:** Matches Redis at extreme concurrency, dominates on GETs

### Tuning Principles

- **`num_shards`**: 2048 for balanced workloads, 4096 for max GET performance
- **`batch_size`**: Match your pipeline depth (256 optimal for P > 64)
- **`buffer_pool_size`**: 2048 for best tail latency

> For detailed tuning guidance, see [Performance Tuning Guide](PERFORMANCE_TUNING.md).

## Benchmark Methodology

### Hardware Specifications

**AWS c7i.16xlarge (Competitive Benchmark):**
- Intel Xeon (4th Gen Sapphire Rapids)
- 64 vCPU, 128GB RAM
- Up to 50 Gbps network
- EBS-optimized

**AWS c7i.8xlarge (Detailed Benchmarks):**
- Intel Xeon (4th Gen Sapphire Rapids)
- 32 vCPU, 64GB RAM
- Up to 12.5 Gbps network
- EBS-optimized

### Software Versions

- Redis: 7.0.x (latest stable)
- Dragonfly: Latest stable release
- Redistill: v1.1.2
- memtier_benchmark: Latest version
- redis-benchmark: From Redis 7.0.x

### Test Parameters

**memtier_benchmark (c7i.16xlarge):**
```bash
memtier_benchmark \
  --server=localhost \
  --port=6379 \
  --protocol=redis \
  --threads=8 \
  --clients=20 \
  --pipeline=30 \
  --data-size=256 \
  --ratio=1:1 \
  --test-time=60 \
  --key-pattern=R:R
```

**redis-benchmark (c7i.8xlarge):**
```bash
# Production workload example
redis-benchmark -t set,get -n 1000000 -c 50 -P 16 -q

# Extreme workload example
redis-benchmark -t set,get -n 1000000 -c 100 -P 64 -q

# Ultra workload example
redis-benchmark -t set,get -n 1000000 -c 500 -P 128 -q
```

## How to Run Benchmarks

### Using Built-in Test Suite

```bash
cd tests/benchmarks
./run_benchmarks.sh
```

This will run benchmarks against Redis and Redistill and generate a comparison report.

### Using memtier_benchmark

```bash
# Install memtier_benchmark
git clone https://github.com/RedisLabs/memtier_benchmark.git
cd memtier_benchmark
autoreconf -ivf
./configure
make
sudo make install

# Run benchmark
memtier_benchmark \
  --server=localhost \
  --port=6379 \
  --protocol=redis \
  --threads=8 \
  --clients=20 \
  --pipeline=30 \
  --data-size=256 \
  --ratio=1:1 \
  --test-time=60 \
  --key-pattern=R:R \
  --json-out-file=results.json
```

### Using redis-benchmark

```bash
# Basic test
redis-benchmark -h localhost -p 6379 -t set,get -n 1000000 -q

# Production workload
redis-benchmark -h localhost -p 6379 -t set,get -n 1000000 -c 50 -P 16 -q

# High concurrency
redis-benchmark -h localhost -p 6379 -t set,get -n 1000000 -c 300 -P 32 -q
```

### Custom Benchmark Script

```python
import redis
import time
import statistics

def benchmark_operations(client, num_ops=100000):
    """Benchmark SET and GET operations"""
    
    # Warmup
    for i in range(1000):
        client.set(f"key_{i}", f"value_{i}")
    
    # Benchmark SET
    set_times = []
    for i in range(num_ops):
        start = time.perf_counter()
        client.set(f"bench_key_{i}", f"bench_value_{i}")
        set_times.append((time.perf_counter() - start) * 1000)
    
    # Benchmark GET
    get_times = []
    for i in range(num_ops):
        start = time.perf_counter()
        client.get(f"bench_key_{i}")
        get_times.append((time.perf_counter() - start) * 1000)
    
    print(f"SET - Avg: {statistics.mean(set_times):.3f}ms, "
          f"p50: {statistics.median(set_times):.3f}ms, "
          f"p99: {statistics.quantiles(set_times, n=100)[98]:.3f}ms")
    
    print(f"GET - Avg: {statistics.mean(get_times):.3f}ms, "
          f"p50: {statistics.median(get_times):.3f}ms, "
          f"p99: {statistics.quantiles(get_times, n=100)[98]:.3f}ms")

# Run benchmark
r = redis.Redis(host='localhost', port=6379)
benchmark_operations(r)
```

## Performance Tips

1. **Disable persistence** on Redis for fair comparison (Redistill has none)
2. **Use pipelining** for realistic workloads (P=16-30 typical)
3. **Match client count** to production (50-300 clients typical)
4. **Warm up the cache** before measuring
5. **Run multiple iterations** and average results
6. **Monitor system resources** (CPU, memory, network)
7. **Use same hardware** for all tests

## Interpreting Results

### Throughput
- **<100K ops/s**: Single-threaded/interactive workload
- **100K-1M ops/s**: Typical production workload
- **1M-5M ops/s**: High-performance workload
- **5M+ ops/s**: Extreme performance workload

### Latency
- **<1ms**: Excellent for caching
- **1-5ms**: Good for most applications
- **5-10ms**: Acceptable for non-critical paths
- **>10ms**: Investigate bottlenecks

### When Redistill Excels
- **Read-heavy workloads** (70%+ GETs)
- **Pipeline depth ≥16**
- **Multiple concurrent clients** (50-300)
- **Medium to large data sizes** (256B-10KB)

## See Also

- [Performance Tuning Guide](PERFORMANCE_TUNING.md) - Optimization tips
- [Configuration Reference](CONFIG.md) - Configuration options
- [Production Guide](PRODUCTION_GUIDE.md) - Deployment best practices

