# Redistill Production Deployment Guide

A comprehensive guide for deploying Redistill in production environments.

## Pre-Deployment Checklist

Before deploying to production, ensure:

- [ ] Authentication password configured
- [ ] Memory limits set appropriately
- [ ] TLS enabled (if exposing externally)
- [ ] Connection limits configured
- [ ] Health check endpoint enabled
- [ ] Load testing completed
- [ ] Monitoring configured
- [ ] Backup plan for cache misses
- [ ] Rollback procedure documented

## Essential Configuration

### 1. Authentication

**Requirement**: Mandatory for production deployments

Configuration file:
```toml
[security]
password = "your-secure-password-here"
```

Environment variable (recommended for containers):
```bash
export REDIS_PASSWORD="your-secure-password-here"
./redistill
```

**Password Requirements**:
- Minimum 16 characters
- Mix of letters, numbers, symbols
- Randomly generated
- Stored securely (secrets management)

### 2. Memory Limits

**Requirement**: Prevent OOM crashes

```toml
[memory]
max_memory = 2147483648  # 2GB
eviction_policy = "allkeys-lru"
```

**Sizing Guide**:
```
max_memory = (available_RAM * 0.75)

Examples:
- 4GB server  → max_memory = 3221225472 (3GB)
- 8GB server  → max_memory = 6442450944 (6GB)
- 16GB server → max_memory = 12884901888 (12GB)
```

Leave 25% for OS and overhead.

### 3. Connection Management

**Requirement**: Protect against resource exhaustion

```toml
[server]
max_connections = 10000
connection_rate_limit = 1000
connection_timeout = 300
```

Adjust based on expected load:
- Web apps: 100-1000 connections
- API gateways: 1000-10000 connections
- High traffic: 10000+ connections

### 4. Health Monitoring

**Requirement**: Enable load balancer health checks

```toml
[server]
health_check_port = 8080
```

Health endpoint: `http://localhost:8080/health`

Response format:
```json
{
  "status": "ok",
  "uptime_seconds": 3600,
  "active_connections": 245,
  "total_connections": 5432,
  "rejected_connections": 12,
  "memory_used": 524288000,
  "max_memory": 2147483648,
  "evicted_keys": 89,
  "total_commands": 1234567
}
```

### 5. TLS/SSL Configuration

**Requirement**: Mandatory for external exposure

```toml
[security]
tls_enabled = true
tls_cert_path = "/etc/letsencrypt/live/your-domain.com/fullchain.pem"
tls_key_path = "/etc/letsencrypt/live/your-domain.com/privkey.pem"
```

Use production certificates:
- Let's Encrypt (free, automated)
- Commercial CA
- Internal PKI for private networks

**Do not** use self-signed certificates in production.

### 6. Persistence Configuration (Optional)

**Requirement**: Optional - Enable for warm restarts

```toml
[persistence]
enabled = true                    # Enable snapshot persistence
snapshot_path = "/data/redistill.rdb"  # Snapshot file path
snapshot_interval = 300           # Auto-save every 5 minutes (0 = disabled)
save_on_shutdown = true          # Save snapshot on graceful shutdown
```

**When to Enable:**
- Need warm restarts after deployments
- Can tolerate slight performance overhead
- Want to preserve data across restarts

**When to Keep Disabled:**
- Maximum performance is critical
- Data can be regenerated
- Using external persistence layer

**Note:** Persistence is disabled by default for maximum performance. When enabled, snapshots are saved in the background and don't block request handling.

## Deployment Methods

### Docker Deployment

Dockerfile:
```dockerfile
FROM rust:1.75 as builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && \
    apt-get install -y ca-certificates && \
    rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/redistill /usr/local/bin/
COPY redistill.toml /etc/redistill/

EXPOSE 6379 8080

CMD ["redistill"]
```

Docker Compose:
```yaml
version: '3.8'

services:
  redistill:
    build: .
    ports:
      - "6379:6379"
      - "8080:8080"
    environment:
      - REDIS_PASSWORD=${REDIS_PASSWORD}
    volumes:
      - ./redistill.toml:/etc/redistill/redistill.toml:ro
    restart: unless-stopped
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:8080/health"]
      interval: 10s
      timeout: 5s
      retries: 3
```

### Systemd Service

Service file (`/etc/systemd/system/redistill.service`):
```ini
[Unit]
Description=Redistill Cache Server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=redistill
Group=redistill
WorkingDirectory=/opt/redistill
EnvironmentFile=/etc/redistill/environment
ExecStart=/opt/redistill/redistill
ExecReload=/bin/kill -HUP $MAINPID
Restart=on-failure
RestartSec=5s
LimitNOFILE=65536

# Security
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/opt/redistill

[Install]
WantedBy=multi-user.target
```

Environment file (`/etc/redistill/environment`):
```bash
REDIS_PASSWORD=your_secure_password_here
REDISTILL_CONFIG=/opt/redistill/redistill.toml
```

Enable and start:
```bash
sudo systemctl enable redistill
sudo systemctl start redistill
sudo systemctl status redistill
```

### Kubernetes Deployment

Deployment manifest:
```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: redistill
  namespace: cache
spec:
  replicas: 3
  selector:
    matchLabels:
      app: redistill
  template:
    metadata:
      labels:
        app: redistill
    spec:
      containers:
      - name: redistill
        image: your-registry/redistill:v1.0.0
        ports:
        - name: redis
          containerPort: 6379
        - name: health
          containerPort: 8080
        env:
        - name: REDIS_PASSWORD
          valueFrom:
            secretKeyRef:
              name: redistill-secret
              key: password
        resources:
          requests:
            memory: "2Gi"
            cpu: "500m"
          limits:
            memory: "4Gi"
            cpu: "2000m"
        livenessProbe:
          httpGet:
            path: /health
            port: 8080
          initialDelaySeconds: 10
          periodSeconds: 10
          timeoutSeconds: 5
          failureThreshold: 3
        readinessProbe:
          httpGet:
            path: /health
            port: 8080
          initialDelaySeconds: 5
          periodSeconds: 5
          timeoutSeconds: 3
          failureThreshold: 2
---
apiVersion: v1
kind: Service
metadata:
  name: redistill
  namespace: cache
spec:
  type: ClusterIP
  ports:
  - name: redis
    port: 6379
    targetPort: 6379
  - name: health
    port: 8080
    targetPort: 8080
  selector:
    app: redistill
```

Secret manifest:
```yaml
apiVersion: v1
kind: Secret
metadata:
  name: redistill-secret
  namespace: cache
type: Opaque
stringData:
  password: your_secure_password_here
```

## Monitoring and Alerting

### Key Metrics

Monitor these metrics continuously:

1. **Memory Usage**
   - Metric: `used_memory` / `max_memory`
   - Alert: > 90%
   - Action: Review eviction policy or increase memory

2. **Eviction Rate**
   - Metric: `evicted_keys` delta
   - Alert: > 1000/minute
   - Action: Increase memory or review cache strategy

3. **Connection Rejections**
   - Metric: `rejected_connections` delta
   - Alert: > 0
   - Action: Increase limits or investigate attack

4. **Active Connections**
   - Metric: `active_connections`
   - Alert: > 80% of `max_connections`
   - Action: Scale or increase limit

5. **Command Throughput**
   - Metric: `total_commands` delta
   - Alert: Sudden drop
   - Action: Check application health

### Monitoring Tools Integration

**Prometheus** (requires exporter - not built-in):
```yaml
scrape_configs:
  - job_name: 'redistill'
    static_configs:
      - targets: ['redistill:8080']
```

**Datadog** (custom check):
```python
# Check health endpoint
response = requests.get('http://redistill:8080/health')
metrics = response.json()

# Send to Datadog
statsd.gauge('redistill.memory.used', metrics['memory_used'])
statsd.gauge('redistill.connections.active', metrics['active_connections'])
```

**CloudWatch** (for AWS):
```bash
# Push metrics from health endpoint
aws cloudwatch put-metric-data \
  --namespace Redistill \
  --metric-name ActiveConnections \
  --value $(curl -s http://localhost:8080/health | jq '.active_connections')
```

## Performance Tuning

### High Throughput Configuration

```toml
[server]
bind = "0.0.0.0"
port = 6379
num_shards = 512
batch_size = 32
buffer_size = 32768
buffer_pool_size = 2048
max_connections = 50000

[performance]
tcp_nodelay = true
tcp_keepalive = 120

[persistence]
enabled = false  # Disabled for maximum performance
```

**Expected**: 2.5M+ ops/s with pipelining

### Low Latency Configuration

```toml
[server]
bind = "0.0.0.0"
port = 6379
num_shards = 128
batch_size = 1
buffer_size = 8192

[performance]
tcp_nodelay = true
```

**Expected**: < 1ms p99 latency

### Memory-Constrained Configuration

```toml
[server]
num_shards = 128
buffer_pool_size = 512

[memory]
max_memory = 1073741824  # 1GB
eviction_policy = "allkeys-lru"

[persistence]
enabled = false  # Optional: Enable if warm restarts needed
```

## Security Hardening

### Operating System

1. **Run as dedicated user**:
```bash
sudo useradd -r -s /bin/false redistill
sudo chown -R redistill:redistill /opt/redistill
```

2. **Set file permissions**:
```bash
chmod 750 /opt/redistill
chmod 640 /opt/redistill/redistill.toml
```

3. **Configure firewall**:
```bash
# Allow only from application servers
sudo ufw allow from 10.0.1.0/24 to any port 6379
sudo ufw allow from 10.0.1.0/24 to any port 8080
```

4. **Disable unnecessary services**:
```bash
# If running on dedicated server
sudo systemctl disable --now bluetooth cups
```

### Network Security

1. **Use private networks**: Deploy in private subnet, not public internet
2. **TLS for external access**: Always enable TLS for cross-datacenter
3. **VPN for remote access**: Don't expose directly to internet
4. **Monitor failed auth attempts**: Track in logs

### Access Control

1. **Rotate passwords quarterly**
2. **Use secrets management**: HashiCorp Vault, AWS Secrets Manager
3. **Limit connection sources**: Firewall rules, security groups
4. **Audit access logs**: Review regularly

## Scaling Strategies

### Vertical Scaling

**When**: Single instance reaches capacity

**Steps**:
1. Increase RAM (most impactful)
2. Increase CPU cores (improves parallelism)
3. Tune `num_shards` to match CPU cores

**Limits**: Single machine capacity

### Horizontal Scaling

**Methods**:

1. **Client-Side Sharding**:
   - Application handles distribution
   - Consistent hashing recommended
   - No single point of failure

2. **Proxy-Based Sharding**:
   - Twemproxy: Lightweight, stable
   - Envoy: Modern, feature-rich
   - HAProxy: Battle-tested

3. **DNS Round-Robin**:
   - Simple load distribution
   - No automatic failover
   - Best for read-heavy workloads

Example client-side sharding (Python):
```python
from redis import Redis
from consistent_hash_ring import ConsistentHashRing

# Create ring with 3 Redistill instances
ring = ConsistentHashRing([
    Redis(host='redistill1'),
    Redis(host='redistill2'),
    Redis(host='redistill3')
])

# Get shard for key
shard = ring.get_node('user:12345')
shard.set('user:12345', data)
```

## Backup and Recovery

### Persistence Options

Redistill supports **optional snapshot-based persistence** for warm restarts. By default, persistence is disabled for maximum performance.

**With Persistence Disabled (Default):**
- Maximum performance (zero disk I/O overhead)
- All data lost on restart
- Use as cache, not source of truth

**With Persistence Enabled:**
- Warm restarts supported (snapshots loaded on startup)
- Background snapshots (non-blocking)
- Configurable snapshot interval
- Save on graceful shutdown
- Data loss possible between snapshots (not real-time durability)

### Data Loss Scenarios

**Without Persistence (Default):**
1. **Server crash**: All data lost
2. **Deployment**: All data lost
3. **Restart**: All data lost

**With Persistence Enabled:**
1. **Server crash**: Data since last snapshot lost
2. **Deployment**: Data since last snapshot lost (if snapshot saved before restart)
3. **Restart**: Data since last snapshot lost (if snapshot saved before restart)

### Mitigation Strategies

1. **Cache warming**: Preload critical data on startup
2. **Application-level fallback**: Query source on cache miss
3. **Gradual traffic ramp**: Don't send full load immediately
4. **Multiple instances**: Reduce impact of single instance failure

Example cache warming:
```python
# On application startup
def warm_cache():
    critical_keys = get_critical_keys()
    for key in critical_keys:
        data = fetch_from_database(key)
        redis_client.set(key, data)
```

## Troubleshooting

### High Memory Usage

**Symptoms**: Memory near `max_memory`, frequent evictions

**Diagnosis**:
```bash
redis-cli INFO | grep memory
redis-cli INFO | grep evicted
```

**Solutions**:
1. Increase `max_memory`
2. Review application caching strategy
3. Reduce TTLs for temporary data
4. Scale horizontally

### Connection Rejections

**Symptoms**: `rejected_connections` > 0

**Diagnosis**:
```bash
redis-cli INFO | grep rejected
curl http://localhost:8080/health | jq '.rejected_connections'
```

**Solutions**:
1. Increase `max_connections`
2. Increase `connection_rate_limit`
3. Check for connection leaks in application
4. Investigate potential DDoS

### Slow Performance

**Symptoms**: Increased latency, reduced throughput

**Diagnosis**:
```bash
# Check CPU usage
top -p $(pgrep redistill)

# Check connections
redis-cli INFO | grep connections

# Check commands
redis-cli INFO | grep commands
```

**Solutions**:
1. Check CPU saturation (> 90%)
2. Increase `num_shards` if CPU-bound
3. Review `batch_size` setting
4. Check network latency
5. Optimize application queries

### Memory Leaks

**Symptoms**: Memory usage grows without bound

**Diagnosis**:
```bash
# Monitor over time
watch -n 60 'redis-cli INFO | grep used_memory'
```

**Solutions**:
1. Report bug with reproduction steps
2. Restart service as workaround
3. Enable memory limit as safeguard

## Maintenance Procedures

### Rolling Updates

For zero-downtime updates:

1. Deploy new version to staging
2. Run smoke tests
3. Update one instance
4. Monitor for 15 minutes
5. If stable, update remaining instances
6. Monitor for 1 hour

### Configuration Changes

1. Test in non-production environment
2. Apply during low-traffic period
3. Monitor metrics for 30 minutes
4. Keep rollback plan ready

### Password Rotation

1. Generate new password
2. Update secrets management
3. Deploy new password to Redistill
4. Update all clients
5. Verify connectivity
6. Remove old password

## Support and Resources

**Documentation**: `docs/` folder in repository
**Issues**: GitHub Issues
**Performance**: Run benchmarks before production deployment

**Before reporting issues**:
1. Check documentation
2. Review logs
3. Verify configuration
4. Test with redis-benchmark
5. Provide reproduction steps
