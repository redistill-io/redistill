# Redistill Quick Start

Get Redistill running in 5 minutes.

## Prerequisites

- Rust 1.70 or higher
- Git
- 1GB free RAM

## Installation
We recommend Docker for the quick start.

```bash
# Run with default settings
docker run -d --name redistill -p 6379:6379 shahidontech/redistill:latest
```

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

You can also build the project from the source.

```bash
# Clone repository
git clone https://github.com/yourusername/redistill.git
cd redistill

# Build release binary
cargo build --release

# Binary location
./target/release/redistill
```

## Start Server

### Without authentication
```bash
./target/release/redistill
```

### With authentication
```bash
export REDIS_PASSWORD=your_password
./target/release/redistill
```

The server starts on `127.0.0.1:6379` by default.

## Test Connection

```bash
# Using redis-cli
redis-cli

# Test commands
> PING
PONG

> SET mykey "Hello Redistill"
OK

> GET mykey
"Hello Redistill"

> SCAN 0
1) "0"
2) 1) "mykey"

> DEL mykey
"Hello Redistill"

> DEL mykey
(integer) 1

# Hash operations
> HSET user:1001 name "John" age "30" city "NYC"
(integer) 3

> HGET user:1001 name
"John"

> HGETALL user:1001
1) "name"
2) "John"
3) "age"
4) "30"
5) "city"
6) "NYC"
```

## With Authentication

```bash
redis-cli

> SET test "value"
ERR NOAUTH Authentication required

> AUTH your_password
OK

> SET test "value"
OK
```

## Health Check

```bash
# HTTP endpoint (if health_check_port is configured)
curl http://localhost:8080/health

# Response
{
  "status": "ok",
  "uptime_seconds": 120,
  "active_connections": 2,
  "memory_used": 1048576
}
```

## Performance Test

```bash
redis-benchmark -t set,get -n 10000000 -c 50 -P 16 -q

# Expected output:
# SET: ~2.2M requests/second
# GET: ~2.3M requests/second
```

## Application Integration

### Python
```python
import redis

r = redis.Redis(
    host='localhost',
    port=6379,
    password='your_password',  # if authentication enabled
    decode_responses=True
)

r.set('key', 'value')
print(r.get('key'))  # Output: value
```

### Node.js
```javascript
const Redis = require('ioredis');

const redis = new Redis({
    host: 'localhost',
    port: 6379,
    password: 'your_password'  // if authentication enabled
});

await redis.set('key', 'value');
const value = await redis.get('key');
console.log(value);  // Output: value
```

### Go
```go
package main

import (
    "context"
    "fmt"
    "github.com/go-redis/redis/v8"
)

func main() {
    ctx := context.Background()
    
    rdb := redis.NewClient(&redis.Options{
        Addr:     "localhost:6379",
        Password: "your_password",  // if authentication enabled
    })
    
    rdb.Set(ctx, "key", "value", 0)
    val, _ := rdb.Get(ctx, "key").Result()
    fmt.Println(val)  // Output: value
}
```

## Monitor Server

### Server statistics
```bash
redis-cli INFO
```

Output includes:
- Server uptime
- Connected clients
- Total commands processed
- Memory usage
- Database size

### Real-time monitoring
```bash
# Watch active connections and commands
watch -n 1 'redis-cli INFO | grep -E "active_connections|total_commands"'
```

## Configuration

Create `redistill.toml` for custom configuration:

```toml
[server]
bind = "127.0.0.1"
port = 6379
health_check_port = 8080

[security]
password = "your-password"

[memory]
max_memory = 2147483648  # 2GB
eviction_policy = "allkeys-lru"

[persistence]
enabled = false                   # Optional: Enable for warm restarts
snapshot_path = "redistill.rdb"   # Snapshot file path
snapshot_interval = 300           # Auto-save interval (0 = disabled)
save_on_shutdown = true           # Save on graceful shutdown
```

Restart server to apply configuration.

## Stop Server

Press `Ctrl+C` for graceful shutdown:

```
Received shutdown signal...
Final Stats:
  Total connections: 1234
  Total commands: 5678901
  Active connections: 0
  Keys in database: 1000
Redistill shut down gracefully
```

## Common Issues

### Port already in use
```bash
# Check what's using port 6379
lsof -i :6379

# Kill existing process or change port in config
```

### Connection refused
```bash
# Check if server is running
ps aux | grep redistill

# Check configuration
cat redistill.toml
```

### Authentication errors
```bash
# Verify password
echo $REDIS_PASSWORD

# Or check config file
grep password redistill.toml
```

## Next Steps

- Read [Configuration Reference](CONFIG.md) for tuning options
- See [Production Guide](PRODUCTION_GUIDE.md) for deployment
- Check [Features](FEATURES.md) for supported commands
