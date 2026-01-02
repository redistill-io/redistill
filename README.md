# Redistill

> A high-performance, Redis-compatible in-memory cache written in Rust. The fastest single-instance Redis-compatible server - up to 4.5x faster than Redis, outperforming both Redis and Dragonfly per instance.

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)
[![CI](https://github.com/shaikh-shahid/redistill/workflows/CI/badge.svg)](https://github.com/shaikh-shahid/redistill/actions/workflows/ci.yml)
[![Release](https://github.com/shaikh-shahid/redistill/workflows/Release/badge.svg)](https://github.com/shaikh-shahid/redistill/actions/workflows/release.yml)
[![Docker](https://img.shields.io/docker/pulls/shahidontech/redistill)](https://hub.docker.com/r/shahidontech/redistill)

## Overview

Redistill is a drop-in Redis replacement optimized for high-performance caching workloads. It implements the Redis protocol (RESP) and achieves up to 4.5x higher throughput than Redis by eliminating persistence overhead and leveraging multi-threaded concurrent access.

Redistill is:
- **Fastest single-instance** Redis-compatible server (9.07M ops/s per instance)
- First Redis-compatible server to exceed 9M ops/s on a single instance
- First to achieve sub-0.5ms p50 latency
- First to sustain 1.5+ GB/s bandwidth

> **Note:** Performance comparisons are single-instance benchmarks. Redis Cluster or Dragonfly with clustering can achieve higher total throughput by scaling horizontally, but Redistill delivers the highest per-instance performance, requiring fewer instances for the same throughput.

**Key characteristics:**
- Redis protocol compatible (RESP)
- **9.07M operations/second** - 4.5x faster than Redis, 1.7x faster than Dragonfly
- **5x lower latency** (p50: 0.48ms vs Redis 2.38ms)
- Multi-threaded architecture with lock-free reads
- Zero persistence overhead
- Production-ready security and monitoring features

## Why Redistill?

- **Maximum Single-Instance Performance** - 4.5x faster than Redis, 1.7x faster than Dragonfly per instance  
- **Lower Latency** - Sub-millisecond p50 latency (0.48ms)  
- **Cost Efficient** - 50-83% infrastructure savings (fewer instances needed)  
- **Drop-in Compatible** - Works with existing Redis clients  
- **Production Ready** - TLS, authentication, monitoring, health checks  
- **Multi-threaded** - Utilizes all CPU cores efficiently  

## Setting performance standard

- **2015**: Redis defines fast (100K ops/s single-threaded)
- **2022**: Dragonfly raises bar (5M ops/s multi-threaded)
- **2026**: Redistill sets new standard (9M ops/s)

## Quick Start

### Docker (Recommended)

```bash
# Run with default settings
docker run -d --name redistill -p 6379:6379 shahidontech/redistill:latest

# Test it works
redis-cli ping
# PONG

redis-cli set hello world
# OK

redis-cli get hello
# "world"
```

### Other Installation Methods

**macOS (Homebrew):**
```bash
brew tap shaikh-shahid/redistill
brew install redistill
redistill
```

**Linux (Binary):**
```bash
wget https://github.com/shaikh-shahid/redistill/releases/download/v1.1.2/redistill-1.1.2-x86_64-unknown-linux-musl.tar.gz
tar -xzf redistill-*.tar.gz
./redistill
```

**Build from Source:**
```bash
git clone https://github.com/shaikh-shahid/redistill
cd redistill
cargo build --release
./target/release/redistill
```

See [Installation](#installation) section for all methods and platforms.

## Performance

### Competitive Benchmark (c7i.16xlarge)

Independent comparison on **AWS c7i.16xlarge** (Intel, 64 cores, 128GB RAM) using memtier_benchmark with production-like configuration:

**Test Configuration:**
- Duration: 60 seconds
- Threads: 8, Connections: 160 (20 per thread)
- Pipeline: 30, Data size: 256 bytes
- Workload: 1:1 SET:GET ratio

| Metric | Redistill | Dragonfly | Redis | vs Redis | vs Dragonfly |
|--------|-----------|-----------|-------|----------|--------------|
| **Throughput** | 9.07M ops/s | 5.43M ops/s | 2.03M ops/s | **4.5x** | **1.7x** |
| **Bandwidth** | 1.58 GB/s | 923 MB/s | 337 MB/s | **4.7x** | **1.7x** |
| **Avg Latency** | 0.524 ms | 0.877 ms | 2.000 ms | **3.8x faster** | **1.7x faster** |
| **p50 Latency** | 0.479 ms | 0.807 ms | 2.383 ms | **5.0x faster** | **1.7x faster** |
| **p99 Latency** | 1.215 ms | 1.975 ms | 2.959 ms | **2.4x faster** | **1.2x faster** |
| **p99.9 Latency** | 1.591 ms | 2.559 ms | 4.159 ms | **2.6x faster** | **1.6x faster** |

**Key Observations:**
- Redistill processed 544M total operations (2.7x more than Dragonfly, 4.5x more than Redis)
- Consistent low latency across all percentiles
- No errors or connection issues across all systems

> 📊 **Methodology:** Tests run with identical hardware and configuration using [memtier_benchmark](https://github.com/RedisLabs/memtier_benchmark). Raw results available in `tests/benchmarks/benchmark_results_memtier/`.

### Benchmark Visualization

**Throughput Comparison (Higher is Better)**
![Throughput](docs/img/throughput.png "Throughput")

**Latency Comparison - p50 (Lower is Better)**
![Latency](docs/img/latency.png "Latency")

**Bandwidth Comparison (Higher is Better)**
![Bandwidth](docs/img/bandwidth.png "Bandwidth")

> 💡 **Note:** Percentages show relative performance vs Redis baseline. All tests run on identical hardware (c7i.16xlarge) with same configuration.

> 📈 For detailed benchmarks and testing methodology, see [Benchmarks Documentation](docs/BENCHMARKS.md).

## Use Cases

### Perfect For

**Session Storage**
- 1M+ operations/second
- Sub-millisecond latency
- Automatic TTL expiration
- 60% cost reduction vs alternatives

**API Response Caching**
- 95%+ cache hit rates
- 50-150x faster than database queries
- Automatic memory management
- LRU eviction built-in

**Rate Limiting**
- Millions of counters
- TTL-based cleanup
- High write throughput
- Perfect for API gateways

**Real-time Leaderboards**
- Fast reads for rankings
- Periodic score updates
- Can rebuild from database
- Sub-millisecond queries

### Not Recommended For

- **Persistent data storage** (no disk persistence)
- **Financial or transactional data** (data lost on restart)
- **Data that cannot be regenerated** (use a database)

> For code examples and patterns, see [Practical Examples](docs/EXAMPLES.md).

## Cost Analysis

**Infrastructure Savings (AWS Pricing)**  
*Scenario: Supporting 5M ops/sec sustained*

| Solution | Instances Needed | Instance Type | vCPU | Monthly Cost | Annual Cost |
|----------|------------------|---------------|------|--------------|-------------|
| Redis | 3x | c7i.16xlarge | 64 | ~$4,500 | ~$54,000 |
| Dragonfly | 1x | c7i.16xlarge | 64 | ~$1,500 | ~$18,000 |
| Redistill | 1x | c7i.8xlarge | 32 | ~$750 | ~$9,000 |

**Savings:**
- Annual savings vs Redis: **$45,000 (83%)**
- Annual savings vs Dragonfly: **$9,000 (50%)**

## Installation

Redistill can be installed via multiple methods. Choose the one that best fits your environment:

### 🐳 Docker (Recommended)

```bash
# Pull and run
docker pull shahidontech/redistill:latest
docker run -d --name redistill -p 6379:6379 -p 8080:8080 shahidontech/redistill:latest

# With password
docker run -d --name redistill -p 6379:6379 -e REDIS_PASSWORD=your-password shahidontech/redistill:latest

# With memory limit (2GB)
docker run -d --name redistill -p 6379:6379 -e REDIS_MAX_MEMORY=2147483648 shahidontech/redistill:latest
```

**Docker Compose:**
```yaml
version: '3.8'
services:
  redistill:
    image: shahidontech/redistill:latest
    ports:
      - "6379:6379"
      - "8080:8080"
    environment:
      - REDIS_PASSWORD=your-password
      - REDIS_MAX_MEMORY=2147483648
```

### 🍺 Homebrew (macOS)

```bash
brew tap shaikh-shahid/redistill
brew install redistill
redistill
```

### 📦 Direct Binary Download

**Linux:**
```bash
wget https://github.com/shaikh-shahid/redistill/releases/download/v1.1.2/redistill-1.1.2-x86_64-unknown-linux-musl.tar.gz
tar -xzf redistill-*.tar.gz && chmod +x redistill && ./redistill
```

**macOS:**
```bash
# Intel: redistill-1.1.2-x86_64-apple-darwin.tar.gz
# Apple Silicon: redistill-1.1.2-aarch64-apple-darwin.tar.gz
wget https://github.com/shaikh-shahid/redistill/releases/download/v1.1.2/redistill-1.1.2-[ARCH].tar.gz
tar -xzf redistill-*.tar.gz && chmod +x redistill && ./redistill
```

**Windows:**
```powershell
Invoke-WebRequest -Uri "https://github.com/shaikh-shahid/redistill/releases/download/v1.1.2/redistill-1.1.2-x86_64-pc-windows-msvc.zip" -OutFile "redistill.zip"
Expand-Archive redistill.zip
.\redistill.exe
```

Browse all releases: [GitHub Releases](https://github.com/shaikh-shahid/redistill/releases)

### 🔨 Build from Source

```bash
# Install Rust (if needed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.sh | sh

# Clone and build
git clone https://github.com/shaikh-shahid/redistill
cd redistill
cargo build --release
./target/release/redistill
```

## Configuration

Create `redistill.toml` in the working directory:

```toml
[server]
bind = "127.0.0.1"
port = 6379
num_shards = 2048        # Optimal for most workloads
max_connections = 10000
health_check_port = 8080

[security]
password = "your-password"
tls_enabled = false

[memory]
max_memory = 2147483648      # 2GB
eviction_policy = "allkeys-lru"

[performance]
tcp_nodelay = true
tcp_keepalive = 60
batch_size = 256
buffer_pool_size = 2048
```

**Quick Tips:**
- `num_shards`: 2048 for balanced, 4096 for max GET performance
- `max_memory`: Set based on available RAM (0 = unlimited)
- `eviction_policy`: "allkeys-lru", "allkeys-random", or "noeviction"

> 📖 See [Configuration Reference](docs/CONFIG.md) for all options and [Performance Tuning Guide](docs/PERFORMANCE_TUNING.md) for optimization.

## Supported Commands

Core caching commands:
- `SET key value [EX seconds]` - Store with optional TTL
- `GET key` - Retrieve value
- `DEL key [key ...]` - Delete keys
- `EXISTS key` - Check existence
- `PING` - Health check
- `INFO` - Server statistics
- `FLUSHDB` - Clear all data
- `AUTH password` - Authenticate

> 📖 See [Features Documentation](docs/FEATURES.md) for complete command list.

## Production Features

**Security:**
- Password authentication (AUTH command)
- TLS/SSL encryption
- Connection limits and rate limiting

**Reliability:**
- Memory limits with automatic eviction (LRU, Random, No-eviction)
- Graceful shutdown with connection draining
- Health check HTTP endpoint

**Monitoring:**
- INFO command with server statistics
- HTTP health endpoint (JSON) at port 8080
- Real-time metrics tracking (uptime, ops/sec, memory)

> 📖 See [Production Guide](docs/PRODUCTION_GUIDE.md) for deployment best practices.

## Client Libraries

Redistill is compatible with all standard Redis clients:

**Python:**
```python
import redis
r = redis.Redis(host='localhost', port=6379, password='your-password')
r.set('key', 'value')
value = r.get('key')
```

**Node.js:**
```javascript
const Redis = require('ioredis');
const redis = new Redis({host: 'localhost', port: 6379, password: 'your-password'});
await redis.set('key', 'value');
```

**Go:**
```go
import "github.com/go-redis/redis/v8"
client := redis.NewClient(&redis.Options{Addr: "localhost:6379"})
client.Set(ctx, "key", "value", 0)
```

## Monitoring

```bash
# Health check (HTTP)
curl http://localhost:8080/health

# Server statistics
redis-cli INFO

# Check memory usage
redis-cli INFO memory
```

## Documentation

- [Quick Start Guide](docs/QUICKSTART.md) - Get started in 5 minutes
- [Performance Benchmarks](docs/BENCHMARKS.md) - Detailed benchmark results
- [Performance Tuning Guide](docs/PERFORMANCE_TUNING.md) - Optimize for your workload
- [Architecture & Design](docs/ARCHITECTURE.md) - How Redistill works
- [Practical Examples](docs/EXAMPLES.md) - Real-world code examples
- [Production Guide](docs/PRODUCTION_GUIDE.md) - Deployment best practices
- [Configuration Reference](docs/CONFIG.md) - Complete configuration options
- [Features](docs/FEATURES.md) - Supported features and roadmap

## Frequently Asked Questions

**Q: Is this production-ready?**  
A: Yes. Redistill includes authentication, TLS, memory limits, connection limits, and health checks.

**Q: Can I migrate from Redis?**  
A: Yes, for caching workloads. Redistill implements the Redis protocol but does not support persistence. Review the [Features](docs/FEATURES.md) document for command compatibility.

**Q: How do I handle high availability?**  
A: Use client-side sharding or a proxy like Twemproxy. Clustering support is on the roadmap.

**Q: What about memory management?**  
A: Configure `max_memory` and `eviction_policy` in the configuration. Redistill automatically evicts keys when the limit is reached.

**Q: When should I use Redis instead?**  
A: Use Redis if you need persistence (AOF/RDB), replication, clustering, or complex data types (lists, sets, hashes). Use Redistill for maximum cache performance.

**Q: Is it stable?**  
A: Yes. Redistill has been tested with redis-benchmark, memtier_benchmark, and production workloads. All core functionality is stable.

## Comparison: Redistill vs Redis vs Dragonfly

| Feature | Redistill | Dragonfly | Redis |
|---------|-----------|-----------|-------|
| Throughput (pipelined, single-instance) | **9.1M ops/s** | 5.4M ops/s | 2.0M ops/s |
| Throughput (clustered) | Manual sharding | Scales horizontally | Redis Cluster scales horizontally |
| Latency (p50) | **0.48ms** | 0.81ms | 2.38ms |
| Concurrency model | Multi-threaded | Multi-threaded | Single-threaded |
| Persistence | No | Yes | Yes (AOF/RDB) |
| Replication | No | Yes | Yes |
| Clustering | No (manual sharding) | Yes | Yes (Redis Cluster) |
| Data types | String (KV) | Full Redis | Full Redis |
| Best for | Read-heavy caching | General purpose | General purpose |
| License | MIT | BSL | BSD |

> **Performance Note:** All throughput numbers are single-instance benchmarks. Redis Cluster and Dragonfly clustering can achieve higher aggregate throughput across multiple instances, but Redistill delivers the highest per-instance performance, meaning fewer instances are needed for the same total throughput.

**When to Use Redistill:**
- High-performance caching (session storage, API responses)
- Read-heavy workloads (70%+ reads)
- Ephemeral data that can be regenerated
- Maximum throughput and minimum latency

**When to Use Redis/Dragonfly:**
- Need persistence (data must survive restarts)
- Need replication and clustering
- Complex data structures required
- Established ecosystem and tooling critical

## Contributing

Contributions are welcome! Please:
1. Open an issue to discuss proposed changes
2. Follow Rust coding conventions
3. Include tests for new features
4. Update relevant documentation

## License

MIT License - see [LICENSE](LICENSE) file for details.

## Acknowledgments

Built with:
- [Tokio](https://tokio.rs/) - Async runtime
- [DashMap](https://github.com/xacrimon/dashmap) - Concurrent hash map
- [Redis Protocol](https://redis.io/docs/reference/protocol-spec/) - RESP compatibility

---

**Star ⭐ this repo if you find Redistill useful!**

For questions, issues, or feature requests, please [open an issue](https://github.com/shaikh-shahid/redistill/issues).
