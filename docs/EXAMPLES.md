# Practical Examples

This guide provides real-world code examples and patterns for using Redistill in production applications.

## Table of Contents

- [Data Sizes and Basic Operations](#data-sizes-and-basic-operations)
- [HTTP Response Caching (Python + Flask)](#http-response-caching-python--flask)
- [JSON API Response Caching (Node.js + Express)](#json-api-response-caching-nodejs--express)
- [Session Storage (Redis-compatible)](#session-storage-redis-compatible)
- [Rate Limiting](#rate-limiting)
- [Real-World Performance Example](#real-world-performance-example)
- [Optimal Data Patterns for Redistill](#optimal-data-patterns-for-redistill)

## Data Sizes and Basic Operations

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

## HTTP Response Caching (Python + Flask)

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

## JSON API Response Caching (Node.js + Express)

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

## Session Storage (Redis-compatible)

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

## Rate Limiting

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

## Real-World Performance Example

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

## Optimal Data Patterns for Redistill

### ✅ Perfect For

- **Session tokens**: 32-256 bytes, 1M+ ops/s sustained
- **API responses**: 1-10KB JSON, 95%+ cache hit rate
- **HTML fragments**: 10-50KB, moderate churn
- **User profiles**: 500B-2KB, high read ratio (20:1)
- **Rate limit counters**: 1-8 bytes, millions of keys

### ⚠️ Consider Alternatives

- **Large objects** (>1MB): Use object storage (S3, MinIO)
- **Binary data** (images, videos): Use CDN
- **Write-heavy** (>50% writes): Consider Redis or database
- **Complex queries**: Use database with indexes

### Memory Planning

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

## Best Practices

1. **Set appropriate TTLs** - Balance freshness vs cache hit rate
2. **Use key namespacing** - Prefix keys by type (e.g., `api:`, `session:`)
3. **Monitor cache hit rates** - Aim for 85%+ hit rate
4. **Plan memory capacity** - Calculate expected data size + 30% buffer
5. **Handle cache misses gracefully** - Always have a fallback to source data
6. **Use connection pooling** - Reuse connections for better performance
7. **Implement circuit breakers** - Degrade gracefully if cache is unavailable

## See Also

- [Configuration Reference](CONFIG.md) - Configuration options
- [Performance Tuning Guide](PERFORMANCE_TUNING.md) - Optimization tips
- [Production Guide](PRODUCTION_GUIDE.md) - Deployment best practices

