# Quick Start: Running Production Tests

## Prerequisites (One-time Setup)

```bash
# 1. Build the server
cargo build --release

# 2. Install Redis client (if not already installed)
# macOS:
brew install redis

# Ubuntu/Debian:
sudo apt-get install redis-tools
```

## Run All Tests (Recommended)

```bash
./tests/run_all_tests.sh
```

This will:
-  Start the server automatically
-  Run all test suites
-  Show results
-  Clean up automatically

**Time:** ~5-10 minutes

## Run Tests Manually

### Step 1: Start Server

```bash
# Terminal 1
./target/release/redistill
```

### Step 2: Run Test Suites

```bash
# Terminal 2

# Concurrency tests
cargo test --test concurrency_tests -- --ignored --nocapture

# Memory tracking tests
cargo test --test memory_tracking_tests -- --ignored --nocapture

# Edge case tests
cargo test --test edge_cases_tests -- --ignored --nocapture

# Stress test
./tests/stress_expiring_keys.sh
```

## Quick Validation

Run this quick test to verify everything works:

```bash
# Start server
./target/release/redistill &

# Wait a moment
sleep 2

# Quick smoke test
redis-cli PING
redis-cli SET test "value" EX 5
redis-cli TTL test
redis-cli PTTL test

# Should see:
# PONG
# OK
# (integer) 5
# (integer) 5000

# Clean up
pkill -f redistill
```

## What Gets Tested

 **Concurrency**: 50 threads × 1000 ops = 50,000 concurrent TTL/PTTL operations  
 **Memory Tracking**: 20 threads × 500 ops = 10,000 keys, memory accuracy  
 **Edge Cases**: Very large expiry times, overflow protection  
 **Stress Test**: 100,000 keys with expiry, memory cleanup  

## Expected Results

All tests should pass. If any fail:
1. Check server logs
2. Ensure server has enough memory
3. See `tests/PRODUCTION_TESTS.md` for detailed troubleshooting
