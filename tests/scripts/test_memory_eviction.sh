#!/bin/bash
# Test memory eviction functionality

set -e

echo "======================================"
echo "  MEMORY EVICTION TEST"
echo "======================================"
echo ""

# Kill any existing redistill process
pkill -f redistill || true
sleep 1

# Create config with 1MB memory limit
cat > redistill-memory-test.toml << EOF
[server]
port = 6379

[memory]
max_memory = 1048576  # 1MB
eviction_policy = "allkeys-lru"
eviction_sample_size = 5
EOF

echo "Test 1: Fill memory and verify eviction"
echo "========================================="
echo "Starting server with 1MB memory limit..."

REDISTILL_CONFIG=redistill-memory-test.toml ../../target/release/redistill &
SERVER_PID=$!
sleep 2

# Fill with 2MB of data (should trigger eviction)
echo "Filling with 2000 keys (each ~1KB)..."
for i in {1..2000}; do
    redis-cli SET "key$i" "$(printf 'x%.0s' {1..1000})" > /dev/null
done

echo "Checking memory stats..."
redis-cli INFO | grep -E "used_memory|evicted_keys"

EVICTED=$(redis-cli INFO | grep "evicted_keys" | cut -d: -f2 | tr -d '\r')
echo ""
if [ "$EVICTED" -gt "0" ]; then
    echo "✅ Eviction working! Evicted $EVICTED keys"
else
    echo "❌ No eviction occurred (expected some evictions)"
fi

# Stop server
kill $SERVER_PID
sleep 1

echo ""
echo "Test 2: No-eviction policy"
echo "======================================"

cat > redistill-memory-test.toml << EOF
[server]
port = 6379

[memory]
max_memory = 102400  # 100KB (very small)
eviction_policy = "noeviction"
EOF

echo "Starting server with noeviction policy..."
REDISTILL_CONFIG=redistill-memory-test.toml ../../target/release/redistill &
SERVER_PID=$!
sleep 2

# Try to fill beyond limit
echo "Trying to add data beyond limit..."
for i in {1..50}; do
    result=$(redis-cli SET "key$i" "$(printf 'x%.0s' {1..1000})" 2>&1 || echo "ERROR")
    if [[ "$result" == *"OOM"* ]]; then
        echo "✅ Got OOM error as expected (iteration $i)"
        break
    fi
done

# Stop server
kill $SERVER_PID
sleep 1

# Cleanup
rm -f redistill-memory-test.toml

echo ""
echo "======================================"
echo "  MEMORY TESTS COMPLETE"
echo "======================================"

