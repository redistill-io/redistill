#!/bin/bash
# Stress test with many expiring keys
# Tests memory cleanup, expiration handling, and performance under load

set -e

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
PROJECT_DIR="$SCRIPT_DIR/.."

echo "======================================"
echo "  EXPIRING KEYS STRESS TEST"
echo "======================================"
echo ""

# Check if server is running
if ! redis-cli -p 6379 PING > /dev/null 2>&1; then
    echo "❌ Server not running on port 6379"
    echo "   Please start the server first:"
    echo "   ./target/release/redistill"
    exit 1
fi

echo "✅ Server is running"
echo ""

# Configuration
PORT=6379
NUM_KEYS=100000
KEY_PREFIX="stress:expire:"
EXPIRY_SECONDS=5

# Clear database
echo "Clearing database..."
redis-cli -p $PORT FLUSHDB > /dev/null

# Get initial memory
echo "Getting initial memory usage..."
INITIAL_MEMORY=$(redis-cli -p $PORT INFO | grep "^used_memory:" | cut -d: -f2 | tr -d '\r' | tr -d ' ')
echo "Initial memory: $INITIAL_MEMORY bytes"
echo ""

# Insert keys with expiry
echo "Inserting $NUM_KEYS keys with ${EXPIRY_SECONDS}s expiry..."
START_TIME=$(date +%s)

# Use pipelining to reduce connection overhead
# Generate commands in RESP format and pipe to redis-cli
{
    for ((i=1; i<=NUM_KEYS; i++)); do
        key="${KEY_PREFIX}${i}"
        val="value_${i}"
        # RESP format: *<nargs>\r\n$<len>\r\n<arg>\r\n...
        # SET key value EX seconds = *4\r\n$3\r\nSET\r\n$<keylen>\r\n<key>\r\n$<vallen>\r\n<val>\r\n$2\r\nEX\r\n$1\r\n5\r\n
        keylen=${#key}
        vallen=${#val}
        explen=${#EXPIRY_SECONDS}
        printf "*4\r\n\$3\r\nSET\r\n\$%d\r\n%s\r\n\$%d\r\n%s\r\n\$2\r\nEX\r\n\$%d\r\n%s\r\n" \
            $keylen "$key" $vallen "$val" $explen "$EXPIRY_SECONDS"
        
        # Progress indicator every 10000 keys
        if [ $((i % 10000)) -eq 0 ]; then
            echo "  Inserted $i keys..." >&2
        fi
    done
} | redis-cli -p $PORT --pipe > /dev/null 2>&1

INSERT_TIME=$(date +%s)
INSERT_DURATION=$((INSERT_TIME - START_TIME))
echo "Insertion completed in ${INSERT_DURATION}s"
echo ""

# Check memory after insertion
echo "Checking memory after insertion..."
MEMORY_AFTER_INSERT=$(redis-cli -p $PORT INFO | grep "^used_memory:" | cut -d: -f2 | tr -d '\r' | tr -d ' ')
MEMORY_USED=$((MEMORY_AFTER_INSERT - INITIAL_MEMORY))
echo "Memory after insert: $MEMORY_AFTER_INSERT bytes"
echo "Memory used by keys: $MEMORY_USED bytes"
echo ""

# Wait for keys to expire
echo "Waiting for keys to expire (${EXPIRY_SECONDS} seconds)..."
sleep $((EXPIRY_SECONDS + 2))

# Trigger cleanup by checking some keys
echo "Triggering cleanup by checking random keys..."
for i in {1..1000}; do
    RANDOM_KEY=$((RANDOM % NUM_KEYS + 1))
    redis-cli -p $PORT TTL "${KEY_PREFIX}${RANDOM_KEY}" > /dev/null 2>&1
done

# Wait a bit for cleanup to complete
sleep 1

# Check memory after expiry
echo "Checking memory after expiry..."
MEMORY_AFTER_EXPIRY=$(redis-cli -p $PORT INFO | grep "^used_memory:" | cut -d: -f2 | tr -d '\r' | tr -d ' ')
MEMORY_REMAINING=$((MEMORY_AFTER_EXPIRY - INITIAL_MEMORY))
MEMORY_FREED=$((MEMORY_USED - MEMORY_REMAINING))

echo ""
echo "======================================"
echo "  STRESS TEST RESULTS"
echo "======================================"
echo "Keys inserted: $NUM_KEYS"
echo "Expiry time: ${EXPIRY_SECONDS}s"
echo "Insertion duration: ${INSERT_DURATION}s"
echo ""
echo "Memory usage:"
echo "  Initial: $INITIAL_MEMORY bytes"
echo "  After insert: $MEMORY_AFTER_INSERT bytes"
echo "  After expiry: $MEMORY_AFTER_EXPIRY bytes"
echo "  Used by keys: $MEMORY_USED bytes"
echo "  Remaining: $MEMORY_REMAINING bytes"
echo "  Freed: $MEMORY_FREED bytes"
echo ""

# Check if keys are actually expired
echo "Verifying keys are expired..."
EXPIRED_COUNT=0
NOT_EXPIRED_COUNT=0

for i in {1..100}; do
    RANDOM_KEY=$((RANDOM % NUM_KEYS + 1))
    TTL=$(redis-cli -p $PORT TTL "${KEY_PREFIX}${RANDOM_KEY}" 2>/dev/null || echo "-2")
    if [ "$TTL" == "-2" ]; then
        EXPIRED_COUNT=$((EXPIRED_COUNT + 1))
    else
        NOT_EXPIRED_COUNT=$((NOT_EXPIRED_COUNT + 1))
    fi
done

echo "Sample check (100 random keys):"
echo "  Expired: $EXPIRED_COUNT"
echo "  Not expired: $NOT_EXPIRED_COUNT"
echo ""

# Memory cleanup check
if [ $MEMORY_USED -gt 0 ]; then
    CLEANUP_PERCENT=$((MEMORY_FREED * 100 / MEMORY_USED))
    if [ $CLEANUP_PERCENT -gt 80 ]; then
        echo "✅ Memory cleanup successful: ${CLEANUP_PERCENT}% freed"
    else
        echo "⚠️  Memory cleanup incomplete: only ${CLEANUP_PERCENT}% freed"
        echo "   This may indicate a memory leak in expiration handling"
    fi
else
    echo "⚠️  Could not calculate cleanup percentage (memory tracking may not be enabled)"
fi

# Key expiration check
if [ $EXPIRED_COUNT -gt 90 ]; then
    echo "✅ Keys properly expired: ${EXPIRED_COUNT}%"
else
    echo "⚠️  Some keys not expired: only ${EXPIRED_COUNT}% expired"
fi

echo ""
echo "Test completed!"
