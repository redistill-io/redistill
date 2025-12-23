# Redistill

> A high-performance, Redis-compatible in-memory cache written in Rust. Up to 2x faster than Redis for read-heavy workloads.

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)
[![CI](https://github.com/shaikh-shahid/redistill/workflows/CI/badge.svg)](https://github.com/shaikh-shahid/redistill/actions/workflows/ci.yml)
[![Release](https://github.com/shaikh-shahid/redistill/workflows/Release/badge.svg)](https://github.com/shaikh-shahid/redistill/actions/workflows/release.yml)

## Overview

Redistill is a drop-in Redis replacement optimized for read-heavy caching workloads. It implements the Redis protocol (RESP) and achieves up to 2x higher throughput than Redis for GET operations by eliminating persistence overhead and leveraging multi-threaded concurrent access.

**Key characteristics:**
- Redis protocol compatible (RESP)
- Up to **6.8M GET operations/second** (vs Redis 3.4M ops/s - **+100%**)
- **9-49% faster** for production workloads (50-300 concurrent clients)
- Multi-threaded architecture with lock-free reads
- Zero persistence overhead
- Production-ready security and monitoring features

## Performance

Benchmark on **AWS c7i.8xlarge** (Intel, 32 cores) with optimal configuration:
- `num_shards=2048`
- `batch_size=256`
- `buffer_pool_size=2048`
- Batched atomic counters
- Probabilistic LRU updates

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

### Key Results

✅ **Redistill Matches or Beats Redis on ALL Workloads:**
- **GET operations: +74% to +98% faster** (nearly 2x!)
- **Production SET: +23% faster**
- **High concurrency SET: +20% faster**
- **Extreme concurrency SET: Matches Redis exactly**

✅ **Perfect For:**
- Read-heavy caching (80-95% reads) - **massive advantage**
- Session storage, API response caching, rate limiting
- Production workloads with any concurrency level
- Scenarios requiring maximum GET throughput

**Redistill is the fastest Redis-compatible cache for read-heavy workloads.**

> 💡 See [Performance Tuning Guide](docs/PERFORMANCE_TUNING.md) for optimization tips and advanced configurations.

## Recommended Configuration

**Optimal configuration** (based on extensive benchmarking on AWS c7i.8xlarge):

```toml
[server]
num_shards = 2048        # Optimized for extreme concurrency (best balance)
batch_size = 256         # Match pipeline depth for deep pipelines
buffer_size = 16384      # 16KB per buffer
buffer_pool_size = 2048  # Optimal for tail latency

[performance]
tcp_nodelay = true       # Lower latency
tcp_keepalive = 60       # Connection reuse

[memory]
max_memory = 0                  # Unlimited (or set to your limit)
eviction_policy = "allkeys-lru" # LRU eviction
```

**This configuration delivers:**
- **6.87M GET ops/s** (98% faster than Redis)
- **2.74M SET ops/s** (matches Redis at extreme concurrency)
- **Best balance** of performance and memory efficiency

> 💡 **For GET-heavy workloads:** Set `num_shards = 4096` for an extra 9% GET performance (7.52M ops/s). See [Performance Tuning Guide](docs/PERFORMANCE_TUNING.md) for details.

### Configuration Impact

**Shard Count (tested on c7i.8xlarge with batch=256, pool=2048):**

| Shards | Ultra SET | Ultra GET | Memory | Best For |
|--------|-----------|-----------|--------|----------|
| 256 | 2.03M ops/s | 6.25M ops/s | Low | Memory-constrained |
| **2048** | **2.74M ops/s** | **6.87M ops/s** | **Moderate** | **Recommended** |
| 4096 | 2.71M ops/s | 7.52M ops/s | High | GET-heavy workloads |

**Key Optimizations in v1.0.4:**
- ✅ **2048 shards:** 8x less contention (31 ops/shard vs 250 ops/shard)
- ✅ **Batched atomic counters:** 256x fewer atomic operations
- ✅ **Probabilistic LRU:** 90% skip rate on timestamp updates
- ✅ **Result:** Matches Redis at extreme concurrency, dominates on GETs

**Tuning Principles:**
- `num_shards`: 2048 for balanced workloads, 4096 for max GET performance
- `batch_size`: Match your pipeline depth (256 optimal for P > 64)
- `buffer_pool_size`: 2048 for best tail latency

## Quick Start

Download the suitable binaries for your machine and operating system from [Release](https://github.com/shaikh-shahid/Redistill/releases) page.

Or you can build the project from source code.

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

## Practical Examples

### Data Sizes and Basic Operations

```bash
# Small key-value (typical session ID)
# Key: ~32 bytes, Value: ~128 bytes
redis-cli SET "session:user:a1b2c3d4" "user_id=12345,logged_in=true,role=admin"

# Medium JSON response (API cache)
# Key: ~64 bytes, Value: ~2-5KB
redis-cli SET "api:users:12345" '{"id":12345,"name":"John","email":"john@example.com",...}' EX 300

# Large HTML page cache
# Key: ~128 bytes, Value: ~50-200KB
redis-cli SET "page:/products/12345" "<html>...</html>" EX 3600
```

**Typical Data Sizes:**
- Session tokens: 32-256 bytes
- API responses (JSON): 1-10 KB
- HTML pages: 50-200 KB
- Cached images/assets: Not recommended (use CDN instead)

### HTTP Response Caching (Python + Flask)

```python
import redis
import json
from flask import Flask, jsonify
from functools import wraps

app = Flask(__name__)
cache = redis.Redis(host='localhost', port=6379, decode_responses=True)

def cache_response(ttl=300):
    """Cache decorator for API responses"""
    def decorator(f):
        @wraps(f)
        def decorated_function(*args, **kwargs):
            # Create cache key from endpoint and args
            cache_key = f"api:{f.__name__}:{':'.join(map(str, args))}"
            
            # Try to get from cache
            cached = cache.get(cache_key)
            if cached:
                return json.loads(cached), 200, {'X-Cache': 'HIT'}
            
            # Execute function and cache result
            result = f(*args, **kwargs)
            cache.setex(cache_key, ttl, json.dumps(result))
            return result, 200, {'X-Cache': 'MISS'}
        
        return decorated_function
    return decorator

@app.route('/api/users/<int:user_id>')
@cache_response(ttl=300)  # Cache for 5 minutes
def get_user(user_id):
    # Expensive database query
    user = db.query(f"SELECT * FROM users WHERE id = {user_id}")
    return {
        'id': user_id,
        'name': user.name,
        'email': user.email,
        'created_at': user.created_at
    }

# Example: First request → Cache MISS (300ms)
# Example: Second request → Cache HIT (2ms) - 150x faster!
```

### JSON API Response Caching (Node.js + Express)

```javascript
const express = require('express');
const Redis = require('ioredis');

const app = express();
const redis = new Redis({ host: 'localhost', port: 6379 });

// Cache middleware
const cacheMiddleware = (ttl = 300) => {
  return async (req, res, next) => {
    const cacheKey = `api:${req.originalUrl}`;
    
    try {
      const cached = await redis.get(cacheKey);
      if (cached) {
        res.set('X-Cache', 'HIT');
        return res.json(JSON.parse(cached));
      }
      
      // Store original res.json
      const originalJson = res.json.bind(res);
      
      // Override res.json to cache response
      res.json = (data) => {
        redis.setex(cacheKey, ttl, JSON.stringify(data));
        res.set('X-Cache', 'MISS');
        return originalJson(data);
      };
      
      next();
    } catch (err) {
      next();
    }
  };
};

// Apply caching to routes
app.get('/api/products', cacheMiddleware(600), async (req, res) => {
  // Expensive database query (~200ms)
  const products = await db.query('SELECT * FROM products LIMIT 100');
  res.json(products);
});

// Typical response: ~5KB JSON, 600s TTL
// Cache HIT: ~2ms (100x faster!)
```

### Session Storage (Redis-compatible)

```python
from flask import Flask, session
from flask_session import Session

app = Flask(__name__)
app.config['SESSION_TYPE'] = 'redis'
app.config['SESSION_REDIS'] = redis.Redis(
    host='localhost', 
    port=6379,
    password='your-password'
)
Session(app)

@app.route('/login', methods=['POST'])
def login():
    # Store session data
    session['user_id'] = 12345
    session['username'] = 'john_doe'
    session['role'] = 'admin'
    # Auto-expires after 24 hours
    session.permanent = True
    return {'status': 'logged_in'}

# Session key: session:a1b2c3d4-e5f6-7890...
# Session value: ~256 bytes (pickled Python dict)
# Typical load: 10,000 active sessions = ~2.5MB
```

### Rate Limiting

```python
import redis
from datetime import datetime

cache = redis.Redis(host='localhost', port=6379)

def rate_limit(user_id, max_requests=100, window=3600):
    """Allow max_requests per window (seconds)"""
    key = f"ratelimit:{user_id}:{datetime.now().strftime('%Y%m%d%H')}"
    
    current = cache.get(key)
    if current and int(current) >= max_requests:
        return False  # Rate limited
    
    pipe = cache.pipeline()
    pipe.incr(key)
    pipe.expire(key, window)
    pipe.execute()
    
    return True  # Allowed

# Example: API endpoint with rate limiting
@app.route('/api/expensive-operation')
def expensive_operation():
    user_id = get_current_user_id()
    
    if not rate_limit(user_id, max_requests=100, window=3600):
        return {'error': 'Rate limit exceeded'}, 429
    
    # Process request
    return {'result': 'success'}

# Key size: ~40 bytes
# Value: 1-5 bytes (counter)
# Typical load: 10,000 users × 1 key = 400KB
```

### Real-World Performance Example

```python
# Scenario: E-commerce product catalog API
# Database query: 150ms average
# Redistill cache: 2ms average
# 
# Without cache: 1000 req/s × 150ms = 150 concurrent connections needed
# With cache (95% hit rate): 1000 req/s × 9.5ms avg = 9.5 concurrent connections
# 
# Result: 15x reduction in backend load, 16x faster response time

import time
from functools import wraps

def timed_cache(key_prefix, ttl=300):
    def decorator(f):
        @wraps(f)
        def wrapper(*args, **kwargs):
            cache_key = f"{key_prefix}:{':'.join(map(str, args))}"
            
            # Check cache
            start = time.time()
            cached = cache.get(cache_key)
            if cached:
                elapsed = (time.time() - start) * 1000
                print(f"Cache HIT: {elapsed:.2f}ms")
                return json.loads(cached)
            
            # Cache miss - query database
            result = f(*args, **kwargs)
            cache.setex(cache_key, ttl, json.dumps(result))
            elapsed = (time.time() - start) * 1000
            print(f"Cache MISS: {elapsed:.2f}ms")
            return result
        
        return wrapper
    return decorator

@timed_cache("product", ttl=600)
def get_product(product_id):
    # Simulated database query
    time.sleep(0.15)  # 150ms
    return {"id": product_id, "name": "Widget", "price": 29.99}

# First call:  Cache MISS: 152.34ms
# Second call: Cache HIT: 1.87ms  (81x faster!)
# Third call:  Cache HIT: 1.92ms
```

### Optimal Data Patterns for Redistill

**✅ Perfect For:**
- **Session tokens**: 32-256 bytes, 1M+ ops/s sustained
- **API responses**: 1-10KB JSON, 95%+ cache hit rate
- **HTML fragments**: 10-50KB, moderate churn
- **User profiles**: 500B-2KB, high read ratio (20:1)
- **Rate limit counters**: 1-8 bytes, millions of keys

**⚠️ Consider Alternatives:**
- **Large objects** (>1MB): Use object storage (S3, MinIO)
- **Binary data** (images, videos): Use CDN
- **Write-heavy** (>50% writes): Consider Redis or database
- **Complex queries**: Use database with indexes

**Memory Planning:**
```
Example workload:
- 1M sessions × 256 bytes = 256 MB
- 100K API responses × 5KB = 500 MB
- 10K HTML pages × 50KB = 500 MB
- Total: ~1.3 GB (set max_memory = 2 GB for safety)

With 2GB limit and LRU eviction:
- Redistill handles eviction automatically
- Least recently used items removed first
- Cache hit rate remains high (85-95%)
```

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
- [Performance Tuning Guide](docs/PERFORMANCE_TUNING.md) - Optimize for your workload
- [Production Guide](docs/PRODUCTION_GUIDE.md) - Deployment best practices
- [Configuration Reference](docs/CONFIG.md) - Complete configuration options
- [Features](docs/FEATURES.md) - Supported features and roadmap

## Design Philosophy

Redistill trades persistence for performance and optimizes for read-heavy workloads. Key design decisions:

**Performance Optimizations:**
- **No persistence layer** - eliminates disk I/O, AOF logging, and RDB snapshot overhead
- **Multi-threaded architecture** - leverages all CPU cores for parallel request processing
- **Lock-free reads** - uses DashMap for concurrent read access without locks
- **Zero-copy design** - uses `Bytes` to avoid string allocations
- **Batched writes** - reduces syscall overhead for pipelined workloads

**Trade-offs:**
- ✅ Excellent for read-heavy caching (2x faster GETs)
- ✅ Great for moderate concurrency (up to 300 clients)
- ⚠️ Write-heavy extreme loads (500+ clients) favor Redis's single-threaded model

**Use Cases:**
- Cache tier in front of persistent databases
- Session storage and API response caching
- Ephemeral data that can be regenerated
- High-throughput read-heavy workloads

For workloads requiring persistence, clustering, or complex data structures, use Redis. For maximum cache performance, use Redistill.

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
| Throughput (pipelined GET) | 3.6-6.8M ops/s | 2.9-3.4M ops/s |
| Throughput (production) | 1.2-2.4M ops/s | 1.0-1.6M ops/s |
| Latency (p50, pipelined) | 0.21-2.2ms | 0.35-3.2ms |
| Concurrency model | Multi-threaded | Single-threaded |
| Memory overhead | Minimal | Moderate |
| Persistence | No | Yes (AOF/RDB) |
| Replication | No | Yes |
| Clustering | No | Yes |
| Data types | String | String, List, Set, Hash, etc. |
| Best for | Read-heavy caching | General purpose |

**When to Use Redistill:**
- ✅ High-performance caching (session storage, API responses)
- ✅ Read-heavy workloads (80-95% reads)
- ✅ Ephemeral data that can be regenerated
- ✅ Maximum throughput for GET operations

**When to Use Redis:**
- ✅ Need persistence (AOF/RDB)
- ✅ Need replication and clustering
- ✅ Complex data structures (lists, sets, sorted sets)
- ✅ Write-heavy workloads at extreme concurrency (500+ clients)

Redistill is designed for specific use cases where maximum cache performance is required and persistence is not needed.
