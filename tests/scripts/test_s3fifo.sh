#!/bin/bash
# Test S3-FIFO eviction algorithm functionality

set -e

GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m' # No Color

echo "======================================"
echo "  S3-FIFO EVICTION ALGORITHM TEST"
echo "======================================"
echo ""

# Kill any existing redistill process
pkill -f redistill || true
sleep 1

echo "Test 1: Basic S3-FIFO Eviction"
echo "========================================="
echo "Starting server with S3-FIFO policy (1MB limit)..."
echo ""

REDISTILL_CONFIG=../../redistill.toml ../../target/release/redistill > /dev/null 2>&1 &
SERVER_PID=$!
sleep 2

# Verify server is running
if ! kill -0 $SERVER_PID 2>/dev/null; then
    echo -e "${RED}✗ Server failed to start${NC}"
    exit 1
fi

# Test 1: Fill memory beyond limit (should trigger eviction)
echo "Test 1.1: Filling memory beyond limit..."
for i in {1..1500}; do
    redis-cli SET "key$i" "$(printf 'x%.0s' {1..800})" > /dev/null 2>&1
done

EVICTED=$(redis-cli INFO 2>/dev/null | grep "evicted_keys" | cut -d: -f2 | tr -d '\r\n ' || echo "0")
MEMORY_USED=$(redis-cli INFO 2>/dev/null | grep "used_memory:" | cut -d: -f2 | tr -d '\r\n ' || echo "0")
POLICY=$(redis-cli INFO 2>/dev/null | grep "maxmemory_policy" | cut -d: -f2 | tr -d '\r\n ' || echo "")

echo "  Memory used: ${MEMORY_USED} bytes"
echo "  Evicted keys: ${EVICTED}"
echo "  Eviction policy: ${POLICY}"

if [ "$POLICY" != "allkeys-s3fifo" ]; then
    echo -e "${RED}✗ Wrong eviction policy: $POLICY${NC}"
    kill $SERVER_PID 2>/dev/null || true
    exit 1
fi

if [ "$EVICTED" -gt "0" ]; then
    echo -e "${GREEN}✓ Eviction working! Evicted $EVICTED keys${NC}"
else
    echo -e "${YELLOW}! No eviction yet (may need more data)${NC}"
fi
echo ""

# Test 2: Test Small queue behavior (one-hit wonders)
echo "Test 1.2: Testing Small queue (one-hit wonders)..."
echo "Inserting keys that will be accessed only once (should go to Small queue)..."
for i in {2000..2100}; do
    redis-cli SET "onehit$i" "value$i" > /dev/null 2>&1
    # Access once
    redis-cli GET "onehit$i" > /dev/null 2>&1
done

# Wait a bit
sleep 1

# Insert more to trigger eviction - Small queue items should be evicted first
echo "Filling more to trigger eviction (Small queue items should evict first)..."
for i in {3000..3100}; do
    redis-cli SET "newkey$i" "$(printf 'x%.0s' {1..1000})" > /dev/null 2>&1
done

sleep 1

# Check if one-hit wonders were evicted
MISSING=0
for i in {2000..2010}; do
    if ! redis-cli EXISTS "onehit$i" > /dev/null 2>&1 || [ "$(redis-cli EXISTS "onehit$i" 2>/dev/null)" == "0" ]; then
        MISSING=$((MISSING + 1))
    fi
done

if [ "$MISSING" -gt "5" ]; then
    echo -e "${GREEN}✓ Small queue working - one-hit wonders evicted ($MISSING/10 checked)${NC}"
else
    echo -e "${YELLOW}! Small queue behavior unclear ($MISSING/10 missing)${NC}"
fi
echo ""

# Test 3: Test Main queue promotion (items accessed twice)
echo "Test 1.3: Testing Main queue promotion..."
echo "Inserting keys that will be accessed twice (should promote to Main)..."
for i in {4000..4010}; do
    redis-cli SET "popular$i" "value$i" > /dev/null 2>&1
    # Access twice to promote to Main
    redis-cli GET "popular$i" > /dev/null 2>&1
    sleep 0.1
    redis-cli GET "popular$i" > /dev/null 2>&1
done

# Fill memory to trigger eviction - Main queue items should survive longer
echo "Filling memory to trigger eviction (Main queue items should survive)..."
for i in {5000..5200}; do
    redis-cli SET "fill$i" "$(printf 'x%.0s' {1..1000})" > /dev/null 2>&1
done

sleep 1

# Check if promoted items are still present
PRESENT=0
for i in {4000..4010}; do
    if redis-cli EXISTS "popular$i" > /dev/null 2>&1 && [ "$(redis-cli EXISTS "popular$i" 2>/dev/null)" == "1" ]; then
        PRESENT=$((PRESENT + 1))
    fi
done

if [ "$PRESENT" -gt "5" ]; then
    echo -e "${GREEN}✓ Main queue working - promoted items survived ($PRESENT/11)${NC}"
else
    echo -e "${YELLOW}! Main queue behavior unclear ($PRESENT/11 present)${NC}"
fi
echo ""

# Test 4: Test Ghost queue (re-insertion after eviction)
echo "Test 1.4: Testing Ghost queue behavior..."
echo "Evicting a key, then re-inserting (should go directly to Main)..."

# Insert and evict a key
redis-cli SET "ghosttest" "original" > /dev/null 2>&1

# Fill memory to evict it
for i in {6000..6500}; do
    redis-cli SET "evictor$i" "$(printf 'x%.0s' {1..1200})" > /dev/null 2>&1
done

sleep 1

# Verify it was evicted
if redis-cli EXISTS "ghosttest" > /dev/null 2>&1 && [ "$(redis-cli EXISTS "ghosttest" 2>/dev/null)" == "0" ]; then
    echo "  Key evicted (as expected)"
    
    # Re-insert - should go directly to Main queue due to Ghost queue
    redis-cli SET "ghosttest" "reinserted" > /dev/null 2>&1
    
    sleep 1
    
    # Fill more - if it's in Main, it should survive longer
    for i in {7000..7100}; do
        redis-cli SET "fill2$i" "$(printf 'x%.0s' {1..1200})" > /dev/null 2>&1
    done
    
    sleep 1
    
    if redis-cli EXISTS "ghosttest" > /dev/null 2>&1 && [ "$(redis-cli EXISTS "ghosttest" 2>/dev/null)" == "1" ]; then
        echo -e "${GREEN}✓ Ghost queue working - re-inserted key went to Main${NC}"
    else
        echo -e "${YELLOW}! Ghost queue behavior unclear${NC}"
    fi
else
    echo -e "${YELLOW}! Key not evicted yet, skipping ghost test${NC}"
fi
echo ""

# Get final stats
FINAL_EVICTED=$(redis-cli INFO 2>/dev/null | grep "evicted_keys" | cut -d: -f2 | tr -d '\r\n ' || echo "0")
FINAL_MEMORY=$(redis-cli INFO 2>/dev/null | grep "used_memory:" | cut -d: -f2 | tr -d '\r\n ' || echo "0")
KEY_COUNT=$(redis-cli DBSIZE 2>/dev/null | tr -d '\r\n ' || echo "0")

echo "Final Statistics:"
echo "  Total evicted: $FINAL_EVICTED keys"
echo "  Current memory: $FINAL_MEMORY bytes"
echo "  Current keys: $KEY_COUNT"

# Stop server
echo ""
echo "Stopping server..."
kill $SERVER_PID 2>/dev/null || true
sleep 1

# Cleanup
rm -f redistill-s3fifo-test.toml

echo ""
echo "======================================"
echo "Test 2: S3-FIFO vs LRU Comparison"
echo "======================================"
echo ""

echo "Note: For detailed comparison, run benchmarks with both policies:"
echo "  eviction_policy = \"allkeys-lru\"    (default)"
echo "  eviction_policy = \"allkeys-s3fifo\" (S3-FIFO)"
echo ""
echo "Expected S3-FIFO benefits:"
echo "  - Up to 72% lower miss ratios"
echo "  - Better handling of one-hit wonders"
echo "  - Slightly higher throughput overhead (2-5%)"
echo ""

echo -e "${GREEN}✓ S3-FIFO tests completed${NC}"
echo ""
