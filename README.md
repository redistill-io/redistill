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

### Intel Performance (AWS c7i-flex.8xlarge - 32 cores)

Benchmark with `batch_size=256`, `buffer_pool_size=1024`:

| Workload | Redistill | Redis | Improvement |
|----------|-----------|-------|-------------|
| Interactive (-c 1, -P 1) | 20K ops/s | 20K ops/s | Similar |
| Production SET (-c 50, -P 16) | 2.34M ops/s | 1.57M ops/s | **+49%** |
| Production GET (-c 50, -P 16) | 2.38M ops/s | 1.95M ops/s | **+21%** |
| High Concurrency GET (-c 300, -P 32) | 3.16M ops/s | 2.70M ops/s | **+17%** |
| Extreme GET (-c 100, -P 64) | 5.99M ops/s | 3.10M ops/s | **+93%** |
| Ultra GET (-c 500, -P 128) | 5.87M ops/s | 3.40M ops/s | **+72%** |

### AMD Performance (AWS c7a.8xlarge - 32 cores)

Benchmark with `batch_size=256`, `buffer_pool_size=2048`:

| Workload | Redistill | Redis | Improvement |
|----------|-----------|-------|-------------|
| Interactive (-c 1, -P 1) | 42K ops/s | 42K ops/s | Similar |
| Production SET (-c 50, -P 16) | 1.17M ops/s | 1.08M ops/s | **+9%** |
| Production GET (-c 50, -P 16) | 1.15M ops/s | 0.98M ops/s | **+17%** |
| High Concurrency SET (-c 300, -P 32) | 2.24M ops/s | 1.80M ops/s | **+24%** |
| High Concurrency GET (-c 300, -P 32) | 2.21M ops/s | 2.09M ops/s | **+6%** |
| Extreme GET (-c 100, -P 64) | 3.62M ops/s | 2.94M ops/s | **+23%** |
| Ultra GET (-c 500, -P 128) | 5.93M ops/s | 3.42M ops/s | **+73%** |

### Key Takeaways

✅ **Cache Workloads (Read-Heavy):**
- GET operations: **+23% to +93% faster** than Redis
- Perfect for caching scenarios (session storage, API responses)

✅ **Production Workloads:**
- Moderate concurrency (50-300 clients): **+9% to +49% faster**
- Excellent balance of throughput and latency

⚠️ **Write-Heavy Extreme Loads:**
- Very high concurrency (500 clients, 128 pipeline): -14% to -24% slower for SETs
- Multi-threaded synchronization overhead at extreme write concurrency

**Recommendation:** Redistill excels as a **high-performance cache** where reads dominate (typical 80-95% read ratio).

## Recommended Configuration

For optimal performance across most workloads:

```toml
[server]
num_shards = 256         # Maximize parallel access across cores
batch_size = 256         # Optimal for high pipeline depths (P > 64)
buffer_size = 16384      # 16KB per buffer (standard)
buffer_pool_size = 2048  # Better tail latency than 1024

[performance]
tcp_nodelay = true       # Lower latency (disable Nagle's algorithm)
```

### Configuration Impact

**Batch Size (tested on c7i-flex.8xlarge):**

| Workload | batch=16 | batch=256 | Improvement |
|----------|----------|-----------|-------------|
| Extreme SET (-P 64) | 2.26M ops/s | **2.53M ops/s** | **+12%** |
| Extreme GET (-P 64) | 5.99M ops/s | **6.80M ops/s** | **+14%** |
| Ultra SET (-P 128) | 2.18M ops/s | **2.52M ops/s** | **+16%** |
| Ultra GET (-P 128) | 5.87M ops/s | **6.83M ops/s** | **+16%** |

**Buffer Pool Size (tested on c7a.8xlarge):**

| Config | Ultra SET (p99) | Ultra GET | Notes |
|--------|-----------------|-----------|-------|
| pool=1024 | 145ms | 5.81M ops/s | Higher tail latency |
| pool=2048 | **106ms** | 5.93M ops/s | **27% better latency** |

**Key Tuning Principles:**
- `batch_size`: Match your pipeline depth for fewer syscalls
- `buffer_pool_size`: 2048 provides better tail latency with minimal overhead
- `num_shards`: Use CPU core count × 8 (256 for 32 cores)

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
