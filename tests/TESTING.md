# Testing Guide for Redistill

This document describes how to test Redistill.

## Quick Test

```bash
# Start the server
./target/release/redistill

# In another terminal, test basic commands
redis-cli PING
redis-cli SET hello world
redis-cli GET hello
```

## Test Suites

### 1. TLS Functionality Test

Tests both plain TCP and TLS connections:

```bash
./tests/scripts/test_tls.sh
```

This will:
- Start server in plain TCP mode
- Test basic commands
- Start server in TLS mode  
- Test TLS connections
- Verify plain TCP clients cannot connect to TLS port

### 2. Performance Benchmarks

Run comprehensive performance benchmarks:

```bash
./tests/benchmarks/run_benchmarks.sh
```

Tests various scenarios:
- Without pipelining (-P 1, interactive mode)
- With pipelining (-P 16, typical production)
- High concurrency (-c 300, -P 32)
- Extreme pipelining (-c 50, -P 64)

### 3. Unit Tests (Coming Soon)

```bash
cargo test
```

### 4. Integration Tests (Coming Soon)

```bash
cargo test --test integration
```

## Manual Testing Scenarios

### Basic Operations

```bash
# Connect
redis-cli -p 6379

# Basic commands
SET key1 value1
GET key1
DEL key1
PING
INFO
```

### TTL Testing

```bash
# Set key with 5 second expiry
redis-cli SET temp "expires soon" EX 5

# Check it exists
redis-cli GET temp

# Wait 6 seconds
sleep 6

# Should be gone
redis-cli GET temp
```

### Authentication Testing

```toml
# In redistill.toml
[security]
password = "test123"
```

```bash
# Restart server
./target/release/redistill

# Try without auth (should fail)
redis-cli PING

# Auth first
redis-cli AUTH test123

# Now commands work
redis-cli PING
```

### TLS Testing

```toml
# In redistill.toml
[security]
tls_enabled = true
tls_cert_path = "tests/certs/server-cert.pem"
tls_key_path = "tests/certs/server-key.pem"
```

```bash
# Restart server
./target/release/redistill

# Plain connection should fail
redis-cli PING  # Hangs or errors

# TLS connection works
redis-cli --tls --insecure PING
```

### High Load Testing

```bash
# Multiple concurrent benchmarks
for i in {1..5}; do
    redis-benchmark -p 6379 -t set,get -n 100000 -c 50 -P 16 -q &
done
wait
```

### Memory Testing

```bash
# Fill with data
redis-benchmark -p 6379 -t set -n 1000000 -d 1024 -P 16

# Check INFO output
redis-cli INFO
```

## Test Certificates

Self-signed certificates are provided in `tests/certs/` for TLS testing.

**Generate new ones:**

```bash
./tests/scripts/generate_test_certs.sh
```

**Never use these in production!** Get real certificates from:
- Let's Encrypt (free)
- Your Certificate Authority
- Cloud provider (AWS, GCP, Azure)

## Continuous Integration (Future)

When CI is set up, tests will run automatically on:
- Every pull request
- Every commit to main
- Nightly builds

## Performance Regression Testing

Compare against previous runs:

```bash
# Save current results
./tests/benchmarks/run_benchmarks.sh > results-$(date +%Y%m%d).txt

# Compare after changes
diff results-old.txt results-new.txt
```

## Stress Testing

```bash
# Long-running stress test (1 hour)
redis-benchmark -p 6379 -t set,get -n 100000000 -c 200 -P 32 -q

# Monitor during test
watch -n 1 'redis-cli INFO | grep -E "total_commands|active_connections"'
```

## Testing Checklist

Before releasing a new version:

- [ ] All unit tests pass
- [ ] All integration tests pass
- [ ] TLS test passes
- [ ] Performance benchmarks show no regression
- [ ] Manual smoke tests complete
- [ ] Authentication works correctly
- [ ] Graceful shutdown works
- [ ] INFO command returns valid data
- [ ] Memory usage is reasonable
- [ ] No data corruption under load
- [ ] TLS handshake succeeds
- [ ] Configuration file loads correctly

## Reporting Issues

When reporting a bug, include:

1. Redistill version: `./target/release/redistill --version`
2. OS and version: `uname -a`
3. Rust version: `rustc --version`
4. Configuration file (redact passwords)
5. Steps to reproduce
6. Expected vs actual behavior
7. Relevant log output

## Contributing Tests

When adding new features:

1. Add unit tests for the feature
2. Add integration test if needed
3. Update this document
4. Ensure all tests pass
5. Add to testing checklist if appropriate

See `tests/README.md` for test structure.

