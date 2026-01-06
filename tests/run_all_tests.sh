#!/bin/bash
# Run all production readiness tests

set -e

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
PROJECT_DIR="$SCRIPT_DIR/.."

echo "======================================"
echo "  REDISTILL PRODUCTION READINESS TESTS"
echo "======================================"
echo ""

cd "$PROJECT_DIR"

# Check if server binary exists
if [ ! -f "target/release/redistill" ]; then
    echo "Building release binary..."
    cargo build --release
fi

# Check if redis-cli is available
if ! command -v redis-cli &> /dev/null; then
    echo "redis-cli not found. Please install Redis client tools."
    echo "   On macOS: brew install redis"
    echo "   On Ubuntu: sudo apt-get install redis-tools"
    exit 1
fi

# Check if redis crate is available for Rust tests
if ! grep -q "redis" Cargo.toml 2>/dev/null; then
    echo "Adding redis dependency for tests..."
    cat >> Cargo.toml << 'EOF'

[dev-dependencies]
redis = "0.24"
EOF
fi

# Create test config with memory tracking enabled
echo "Creating test configuration..."
cat > /tmp/redistill-test.toml << 'EOF'
[server]
bind = "127.0.0.1"
port = 6379
num_shards = 256
batch_size = 16
buffer_size = 16384
buffer_pool_size = 1024
max_connections = 10000
connection_timeout = 300
connection_rate_limit = 0
health_check_port = 0

[security]
password = ""
tls_enabled = false

[performance]
tcp_nodelay = true
tcp_keepalive = 60

[memory]
max_memory = 1073741824  # 1GB - enables memory tracking for tests
eviction_policy = "allkeys-lru"
eviction_sample_size = 5

[persistence]
enabled = false
EOF

# Start server in background with test config
echo "Starting Redistill server with memory tracking enabled..."
pkill -f "target/release/redistill" || true
sleep 1

REDISTILL_CONFIG=/tmp/redistill-test.toml ./target/release/redistill > /tmp/redistill-test.log 2>&1 &
SERVER_PID=$!
sleep 2

# Check if server started
if ! kill -0 $SERVER_PID 2>/dev/null; then
    echo "Failed to start server"
    cat /tmp/redistill-test.log
    exit 1
fi

echo "✅ Server started (PID: $SERVER_PID)"
echo ""

# Function to cleanup
cleanup() {
    echo ""
    echo "Cleaning up..."
    kill $SERVER_PID 2>/dev/null || true
    wait $SERVER_PID 2>/dev/null || true
    rm -f /tmp/redistill-test.toml
}

trap cleanup EXIT

# Wait for server to be ready
echo "Waiting for server to be ready..."
for i in {1..30}; do
    if redis-cli -p 6379 PING > /dev/null 2>&1; then
        echo "✅ Server is ready"
        break
    fi
    if [ $i -eq 30 ]; then
        echo "Server not responding"
        exit 1
    fi
    sleep 1
done

echo ""
echo "======================================"
echo "  RUNNING TESTS"
echo "======================================"
echo ""

# 1. Concurrency tests
echo "1. Running concurrency tests (TTL/PTTL)..."
if cargo test --test concurrency_tests -- --ignored --nocapture 2>&1 | tee /tmp/concurrency-test.log; then
    echo "Concurrency tests passed"
else
    echo "Concurrency tests failed"
    FAILED=1
fi
echo ""

# 2. Memory tracking tests
echo "2. Running memory tracking tests..."
if cargo test --test memory_tracking_tests -- --ignored --nocapture 2>&1 | tee /tmp/memory-test.log; then
    echo "Memory tracking tests passed"
else
    echo "Memory tracking tests failed"
    FAILED=1
fi
echo ""

# 3. Edge case tests
echo "3. Running edge case tests..."
if cargo test --test edge_cases_tests -- --ignored --nocapture 2>&1 | tee /tmp/edge-test.log; then
    echo "Edge case tests passed"
else
    echo "Edge case tests failed"
    FAILED=1
fi
echo ""

# 4. Stress test
echo "4. Running stress test (expiring keys)..."
chmod +x "$SCRIPT_DIR/stress_expiring_keys.sh"
if "$SCRIPT_DIR/stress_expiring_keys.sh" 2>&1 | tee /tmp/stress-test.log; then
    echo "Stress test passed"
else
    echo "Stress test failed"
    FAILED=1
fi
echo ""

# Summary
echo "======================================"
echo "  TEST SUMMARY"
echo "======================================"

if [ -z "${FAILED:-}" ]; then
    echo "All tests passed!"
    exit 0
else
    echo "Some tests failed. Check logs:"
    echo "   - Concurrency: /tmp/concurrency-test.log"
    echo "   - Memory: /tmp/memory-test.log"
    echo "   - Edge cases: /tmp/edge-test.log"
    echo "   - Stress: /tmp/stress-test.log"
    exit 1
fi
