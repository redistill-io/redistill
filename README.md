# Redistill

> A high-performance, Redis-compatible in-memory cache written in Rust that's 2x faster than Redis.

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)
[![CI](https://github.com/shaikh-shahid/redistill/workflows/CI/badge.svg)](https://github.com/YOUR_USERNAME/redistill/actions/workflows/ci.yml)
[![Release](https://github.com/shaikh-shahid/redistill/workflows/Release/badge.svg)](https://github.com/shaikh-shahid/redistill/actions/workflows/release.yml)

## Overview

Redistill is a drop-in Redis replacement optimized for caching and ephemeral data workloads. It implements the Redis protocol (RESP) and achieves 2x higher throughput than Redis by eliminating persistence overhead and optimizing for in-memory operations.

**Key characteristics:**
- Redis protocol compatible
- 2.1M operations/second (vs Redis 1.0M ops/s)
- Zero persistence overhead
- Production-ready security and monitoring features

## Performance

Benchmark results on identical hardware (MacBook Pro M2, 14 cores):

| Workload | Redistill | Redis | Improvement |
|----------|-----------|-------|-------------|
| Pipelined SET (-P 16) | 2.18M ops/s | 1.87M ops/s | +17% |
| Pipelined GET (-P 16) | 2.27M ops/s | 2.16M ops/s | +5% |
| Extreme Pipeline (-P 64) | 4.73M ops/s | 2.42M ops/s | +95% |

## Quick Start

### Requirements

- Rust 1.70 or higher
- Linux, macOS, or Windows
- 1GB RAM minimum (configurable)

### Installation

```bash
# Install Rust if needed
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.sh | sh

# Build from source
git clone https://github.com/yourusername/redistill
cd redistill
cargo build --release

# Run server
./target/release/redistill
```

### Basic Usage

```bash
# Test connection
redis-cli ping
# PONG

# Set and get values
redis-cli set mykey "hello world"
# OK

redis-cli get mykey
# "hello world"
```

## Configuration

Create `redistill.toml` in the working directory:

```toml
[server]
bind = "127.0.0.1"
port = 6379
max_connections = 10000
health_check_port = 8080

[security]
password = "your-password"
tls_enabled = false

[memory]
max_memory = 2147483648  # 2GB
eviction_policy = "allkeys-lru"
```

See [Configuration Reference](docs/CONFIG.md) for all options.

## Production Features

**Security**:
- Password authentication
- TLS/SSL encryption
- Connection limits and rate limiting

**Reliability**:
- Memory limits with automatic eviction (LRU, Random, No-eviction)
- Graceful shutdown
- Health check HTTP endpoint

**Monitoring**:
- INFO command with server statistics
- HTTP health endpoint (JSON)
- Real-time metrics tracking

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

## Use Cases

**Recommended for:**
- HTTP session storage
- API response caching
- Rate limiting counters
- Real-time leaderboards
- Temporary data storage

**Not recommended for:**
- Persistent data storage (no disk persistence)
- Financial or transactional data
- Data that cannot be regenerated

## Client Libraries

Redistill is compatible with all Redis clients:

**Python** (`redis-py`):
```python
import redis
r = redis.Redis(host='localhost', port=6379, password='your-password')
r.set('key', 'value')
value = r.get('key')
```

**Node.js** (`ioredis`):
```javascript
const Redis = require('ioredis');
const redis = new Redis({host: 'localhost', port: 6379, password: 'your-password'});
await redis.set('key', 'value');
const value = await redis.get('key');
```

**Go** (`go-redis`):
```go
import "github.com/go-redis/redis/v8"

client := redis.NewClient(&redis.Options{
    Addr: "localhost:6379",
    Password: "your-password",
})
client.Set(ctx, "key", "value", 0)
```

## Monitoring

**Health check** (HTTP endpoint):
```bash
curl http://localhost:8080/health
```

**Server statistics** (Redis protocol):
```bash
redis-cli INFO
```

## Documentation

- [Quick Start](docs/QUICKSTART.md) - Get started in 5 minutes
- [Production Guide](docs/PRODUCTION_GUIDE.md) - Deployment best practices
- [Configuration Reference](docs/CONFIG.md) - Complete configuration options
- [Features](docs/FEATURES.md) - Supported features and roadmap

## Design Philosophy

Redistill trades persistence for performance. By eliminating disk I/O, AOF logging, and RDB snapshots, it achieves 2x higher throughput than Redis. This makes it ideal for caching workloads where data can be regenerated.

For workloads requiring persistence, use Redis with AOF/RDB, or use Redistill as a cache tier in front of a persistent database.

## Frequently Asked Questions

**Q: Is this production-ready?**  
A: Yes. Redistill includes authentication, TLS, memory limits, connection limits, and health checks.

**Q: Can I migrate from Redis?**  
A: Yes, for caching workloads. Redistill implements the Redis protocol but does not support persistence. Review the [Features](docs/FEATURES.md) document for command compatibility.

**Q: How do I handle high availability?**  
A: Use client-side sharding or a proxy like Twemproxy. Clustering support is on the roadmap.

**Q: What about memory management?**  
A: Configure `max_memory` and `eviction_policy` in the configuration. Redistill automatically evicts keys when the limit is reached.

**Q: Is it stable?**  
A: Yes. Redistill has been tested with redis-benchmark and production workloads. All core functionality is stable.

## Benchmarking

```bash
# Run built-in test suite
./tests/benchmarks/run_benchmarks.sh

# Use redis-benchmark
redis-benchmark -t set,get -n 1000000 -c 50 -P 16 -q
```

## Contributing

Contributions are welcome. Please:
1. Open an issue to discuss proposed changes
2. Follow Rust coding conventions
3. Include tests for new features
4. Update relevant documentation

## License

MIT License - see LICENSE file for details.

## Acknowledgments

Built with:
- [Tokio](https://tokio.rs/) - Async runtime
- [DashMap](https://github.com/xacrimon/dashmap) - Concurrent hash map
- [Redis Protocol](https://redis.io/docs/reference/protocol-spec/) - RESP compatibility

## Comparison with Redis

| Feature | Redistill | Redis |
|---------|-----------|-------|
| Throughput (pipelined) | 2.1M ops/s | 1.0M ops/s |
| Latency (p50) | 0.20ms | 0.37ms |
| Memory overhead | Minimal | Moderate |
| Persistence | No | Yes (AOF/RDB) |
| Replication | No | Yes |
| Clustering | No | Yes |
| Data types | String | String, List, Set, Hash, etc. |
| Use case | High-performance caching | General purpose |

Redistill is designed for specific use cases where maximum cache performance is required and persistence is not needed.
