#!/bin/bash
# Quick S3-FIFO test - verifies basic functionality

echo "Quick S3-FIFO Test"
echo "=================="
echo ""

# Create minimal config
cat > /tmp/redistill-test.toml << TOML
[server]
port = 6379

[memory]
max_memory = 512000  # 500KB
eviction_policy = "allkeys-s3fifo"
TOML

# Start server
REDISTILL_CONFIG=../../redistill.toml ../../target/release/redistill > /dev/null 2>&1 &
PID=$!
sleep 2

# Test 1: Verify policy
POLICY=$(redis-cli INFO 2>/dev/null | grep "maxmemory_policy" | cut -d: -f2 | tr -d '\r\n ')
if [ "$POLICY" == "allkeys-s3fifo" ]; then
    echo "✅ Policy correctly set: $POLICY"
else
    echo "❌ Wrong policy: $POLICY"
fi

# Test 2: Trigger eviction
echo "Filling memory..."
for i in {1..1000}; do
    redis-cli SET "k$i" "$(printf 'x%.0s' {1..600})" > /dev/null 2>&1
done

EVICTED=$(redis-cli INFO 2>/dev/null | grep "evicted_keys" | cut -d: -f2 | tr -d '\r\n ')
echo "Evicted keys: $EVICTED"

if [ "$EVICTED" -gt "0" ]; then
    echo "✅ Eviction working"
else
    echo "⚠️  No eviction yet (may need more data)"
fi

# Cleanup
kill $PID 2>/dev/null
rm -f /tmp/redistill-test.toml

echo ""
echo "Done! For comprehensive tests, run: ./tests/scripts/test_s3fifo.sh"
