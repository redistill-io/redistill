#!/bin/bash
# Compare S3-FIFO vs LRU eviction algorithms
# Runs same workload with both policies and compares metrics

set -e

GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

echo "=========================================="
echo "  S3-FIFO vs LRU EVICTION COMPARISON"
echo "=========================================="
echo ""

# Cleanup function
cleanup() {
    pkill -f "redistill.*test.*toml" 2>/dev/null || true
    sleep 1
    rm -f /tmp/redistill-lru-test.toml /tmp/redistill-s3fifo-test.toml
}

trap cleanup EXIT

# Build if needed
if [ ! -f ../../target/release/redistill ]; then
    echo "Building redistill..."
    cargo build --release 2>/dev/null
fi

# Test configuration
MEMORY_LIMIT=1048576  # 1MB
PORT_LRU=6379
PORT_S3FIFO=6380

# Create LRU config
cat > /tmp/redistill-lru-test.toml << EOF
[server]
port = $PORT_LRU

[memory]
max_memory = $MEMORY_LIMIT
eviction_policy = "allkeys-lru"
eviction_sample_size = 5
EOF

# Create S3-FIFO config
cat > /tmp/redistill-s3fifo-test.toml << EOF
[server]
port = $PORT_S3FIFO

[memory]
max_memory = $MEMORY_LIMIT
eviction_policy = "allkeys-s3fifo"
eviction_sample_size = 5
EOF

echo "Configuration:"
echo "  Memory limit: $(($MEMORY_LIMIT / 1024))KB"
echo "  Test workload: Mixed (one-hit wonders + popular items)"
echo ""

# ==================== Test 1: One-Hit Wonder Workload ====================
echo "=========================================="
echo "Test 1: One-Hit Wonder Workload"
echo "=========================================="
echo "This workload has many keys accessed only once (S3-FIFO should excel)"
echo ""

# Start LRU server
echo -e "${BLUE}Starting LRU server...${NC}"
REDISTILL_CONFIG=/tmp/redistill-lru-test.toml ../../target/release/redistill > /dev/null 2>&1 &
LRU_PID=$!
sleep 2

# Start S3-FIFO server
echo -e "${BLUE}Starting S3-FIFO server...${NC}"
REDISTILL_CONFIG=/tmp/redistill-s3fifo-test.toml ../../target/release/redistill > /dev/null 2>&1 &
S3FIFO_PID=$!
sleep 2

# Workload 1: Insert many one-hit wonders (accessed only once)
echo "Phase 1: Inserting 800 one-hit wonder keys (accessed once)..."
for i in {1..800}; do
    redis-cli -p $PORT_LRU SET "ohw$i" "$(printf 'x%.0s' {1..800})" > /dev/null 2>&1
    redis-cli -p $PORT_LRU GET "ohw$i" > /dev/null 2>&1  # Access once
    
    redis-cli -p $PORT_S3FIFO SET "ohw$i" "$(printf 'x%.0s' {1..800})" > /dev/null 2>&1
    redis-cli -p $PORT_S3FIFO GET "ohw$i" > /dev/null 2>&1  # Access once
done

# Phase 2: Insert popular keys (accessed multiple times)
echo "Phase 2: Inserting 200 popular keys (accessed 5 times)..."
for i in {1..200}; do
    # LRU
    redis-cli -p $PORT_LRU SET "pop$i" "$(printf 'x%.0s' {1..800})" > /dev/null 2>&1
    for j in {1..5}; do
        redis-cli -p $PORT_LRU GET "pop$i" > /dev/null 2>&1
        sleep 0.01
    done
    
    # S3-FIFO
    redis-cli -p $PORT_S3FIFO SET "pop$i" "$(printf 'x%.0s' {1..800})" > /dev/null 2>&1
    for j in {1..5}; do
        redis-cli -p $PORT_S3FIFO GET "pop$i" > /dev/null 2>&1
        sleep 0.01
    done
done

sleep 1

# Phase 3: Trigger eviction by filling memory
echo "Phase 3: Filling memory to trigger eviction..."
for i in {1..400}; do
    redis-cli -p $PORT_LRU SET "fill$i" "$(printf 'x%.0s' {1..1000})" > /dev/null 2>&1
    redis-cli -p $PORT_S3FIFO SET "fill$i" "$(printf 'x%.0s' {1..1000})" > /dev/null 2>&1
done

sleep 1

# Phase 4: Check hit rates (re-access keys)
echo "Phase 4: Checking hit rates (re-accessing keys)..."

LRU_HITS=0
LRU_MISSES=0
for i in {1..100}; do
    if redis-cli -p $PORT_LRU GET "pop$i" > /dev/null 2>&1 && [ "$(redis-cli -p $PORT_LRU GET "pop$i" 2>/dev/null)" != "$(printf '')" ]; then
        LRU_HITS=$((LRU_HITS + 1))
    else
        LRU_MISSES=$((LRU_MISSES + 1))
    fi
done

S3FIFO_HITS=0
S3FIFO_MISSES=0
for i in {1..100}; do
    if redis-cli -p $PORT_S3FIFO GET "pop$i" > /dev/null 2>&1 && [ "$(redis-cli -p $PORT_S3FIFO GET "pop$i" 2>/dev/null)" != "$(printf '')" ]; then
        S3FIFO_HITS=$((S3FIFO_HITS + 1))
    else
        S3FIFO_MISSES=$((S3FIFO_MISSES + 1))
    fi
done

# Get statistics
LRU_EVICTED=$(redis-cli -p $PORT_LRU INFO 2>/dev/null | grep "evicted_keys" | cut -d: -f2 | tr -d '\r\n ' || echo "0")
LRU_MEMORY=$(redis-cli -p $PORT_LRU INFO 2>/dev/null | grep "used_memory:" | cut -d: -f2 | tr -d '\r\n ' || echo "0")
LRU_KEYS=$(redis-cli -p $PORT_LRU DBSIZE 2>/dev/null | tr -d '\r\n ' || echo "0")

S3FIFO_EVICTED=$(redis-cli -p $PORT_S3FIFO INFO 2>/dev/null | grep "evicted_keys" | cut -d: -f2 | tr -d '\r\n ' || echo "0")
S3FIFO_MEMORY=$(redis-cli -p $PORT_S3FIFO INFO 2>/dev/null | grep "used_memory:" | cut -d: -f2 | tr -d '\r\n ' || echo "0")
S3FIFO_KEYS=$(redis-cli -p $PORT_S3FIFO DBSIZE 2>/dev/null | tr -d '\r\n ' || echo "0")

# Calculate hit rates
LRU_TOTAL=$((LRU_HITS + LRU_MISSES))
S3FIFO_TOTAL=$((S3FIFO_HITS + S3FIFO_MISSES))

if [ $LRU_TOTAL -gt 0 ]; then
    LRU_HIT_RATE=$(awk "BEGIN {printf \"%.2f\", ($LRU_HITS / $LRU_TOTAL) * 100}")
else
    LRU_HIT_RATE="0.00"
fi

if [ $S3FIFO_TOTAL -gt 0 ]; then
    S3FIFO_HIT_RATE=$(awk "BEGIN {printf \"%.2f\", ($S3FIFO_HITS / $S3FIFO_TOTAL) * 100}")
else
    S3FIFO_HIT_RATE="0.00"
fi

# Calculate improvement
if [ "$LRU_HIT_RATE" != "0.00" ] && [ "$S3FIFO_HIT_RATE" != "0.00" ]; then
    IMPROVEMENT=$(awk "BEGIN {printf \"%.2f\", (($S3FIFO_HIT_RATE - $LRU_HIT_RATE) / $LRU_HIT_RATE) * 100}")
else
    IMPROVEMENT="0.00"
fi

echo ""
echo -e "${BLUE}Results - One-Hit Wonder Workload:${NC}"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
printf "%-20s | %15s | %15s | %15s\n" "Metric" "LRU" "S3-FIFO" "Improvement"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
printf "%-20s | %15s | %15s | %15s\n" "Hit Rate (%)" "$LRU_HIT_RATE%" "$S3FIFO_HIT_RATE%" "$IMPROVEMENT%"
printf "%-20s | %15s | %15s | %15s\n" "Hits" "$LRU_HITS" "$S3FIFO_HITS" "$(($S3FIFO_HITS - $LRU_HITS))"
printf "%-20s | %15s | %15s | %15s\n" "Misses" "$LRU_MISSES" "$S3FIFO_MISSES" "$(($S3FIFO_MISSES - $LRU_MISSES))"
printf "%-20s | %15s | %15s | %15s\n" "Evicted Keys" "$LRU_EVICTED" "$S3FIFO_EVICTED" "$(($S3FIFO_EVICTED - $LRU_EVICTED))"
printf "%-20s | %15s | %15s | %15s\n" "Current Keys" "$LRU_KEYS" "$S3FIFO_KEYS" "$(($S3FIFO_KEYS - $LRU_KEYS))"
printf "%-20s | %15s | %15s | %15s\n" "Memory Used" "$(awk "BEGIN {printf \"%.2f\", $LRU_MEMORY/1024}")KB" "$(awk "BEGIN {printf \"%.2f\", $S3FIFO_MEMORY/1024}")KB" "-"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# Analysis
if (( $(echo "$S3FIFO_HIT_RATE > $LRU_HIT_RATE" | bc -l 2>/dev/null || echo "0") )); then
    echo -e "${GREEN}✓ S3-FIFO has better hit rate (+$IMPROVEMENT%)${NC}"
    echo "  This shows S3-FIFO is more effective at keeping popular items"
elif (( $(echo "$S3FIFO_HIT_RATE == $LRU_HIT_RATE" | bc -l 2>/dev/null || echo "0") )); then
    echo -e "${YELLOW}⚠ Hit rates are similar${NC}"
else
    echo -e "${YELLOW}⚠ LRU has better hit rate in this test${NC}"
    echo "  Note: Results vary by workload - S3-FIFO excels with many one-hit wonders"
fi

echo ""

# Stop servers
kill $LRU_PID $S3FIFO_PID 2>/dev/null || true
sleep 2

# ==================== Test 2: Mixed Workload ====================
echo "=========================================="
echo "Test 2: Mixed Workload"
echo "=========================================="
echo "Balanced workload with mixed access patterns"
echo ""

# Restart servers
REDISTILL_CONFIG=/tmp/redistill-lru-test.toml ../../target/release/redistill > /dev/null 2>&1 &
LRU_PID=$!
sleep 2

REDISTILL_CONFIG=/tmp/redistill-s3fifo-test.toml ../../target/release/redistill > /dev/null 2>&1 &
S3FIFO_PID=$!
sleep 2

# Mixed workload
echo "Inserting mixed workload (50% one-hit, 50% popular)..."
for i in {1..500}; do
    # One-hit wonders
    redis-cli -p $PORT_LRU SET "mix_ohw$i" "$(printf 'x%.0s' {1..800})" > /dev/null 2>&1
    redis-cli -p $PORT_LRU GET "mix_ohw$i" > /dev/null 2>&1
    
    redis-cli -p $PORT_S3FIFO SET "mix_ohw$i" "$(printf 'x%.0s' {1..800})" > /dev/null 2>&1
    redis-cli -p $PORT_S3FIFO GET "mix_ohw$i" > /dev/null 2>&1
    
    # Popular items (accessed 3 times)
    redis-cli -p $PORT_LRU SET "mix_pop$i" "$(printf 'x%.0s' {1..800})" > /dev/null 2>&1
    for j in {1..3}; do
        redis-cli -p $PORT_LRU GET "mix_pop$i" > /dev/null 2>&1
    done
    
    redis-cli -p $PORT_S3FIFO SET "mix_pop$i" "$(printf 'x%.0s' {1..800})" > /dev/null 2>&1
    for j in {1..3}; do
        redis-cli -p $PORT_S3FIFO GET "mix_pop$i" > /dev/null 2>&1
    done
done

# Trigger eviction
for i in {1..300}; do
    redis-cli -p $PORT_LRU SET "mix_fill$i" "$(printf 'x%.0s' {1..1200})" > /dev/null 2>&1
    redis-cli -p $PORT_S3FIFO SET "mix_fill$i" "$(printf 'x%.0s' {1..1200})" > /dev/null 2>&1
done

sleep 1

# Check popular items
LRU_HITS=0
LRU_MISSES=0
for i in {1..50}; do
    if redis-cli -p $PORT_LRU GET "mix_pop$i" > /dev/null 2>&1 && [ "$(redis-cli -p $PORT_LRU GET "mix_pop$i" 2>/dev/null)" != "$(printf '')" ]; then
        LRU_HITS=$((LRU_HITS + 1))
    else
        LRU_MISSES=$((LRU_MISSES + 1))
    fi
done

S3FIFO_HITS=0
S3FIFO_MISSES=0
for i in {1..50}; do
    if redis-cli -p $PORT_S3FIFO GET "mix_pop$i" > /dev/null 2>&1 && [ "$(redis-cli -p $PORT_S3FIFO GET "mix_pop$i" 2>/dev/null)" != "$(printf '')" ]; then
        S3FIFO_HITS=$((S3FIFO_HITS + 1))
    else
        S3FIFO_MISSES=$((S3FIFO_MISSES + 1))
    fi
done

LRU_EVICTED=$(redis-cli -p $PORT_LRU INFO 2>/dev/null | grep "evicted_keys" | cut -d: -f2 | tr -d '\r\n ' || echo "0")
S3FIFO_EVICTED=$(redis-cli -p $PORT_S3FIFO INFO 2>/dev/null | grep "evicted_keys" | cut -d: -f2 | tr -d '\r\n ' || echo "0")

LRU_TOTAL=$((LRU_HITS + LRU_MISSES))
S3FIFO_TOTAL=$((S3FIFO_HITS + S3FIFO_MISSES))

if [ $LRU_TOTAL -gt 0 ]; then
    LRU_HIT_RATE=$(awk "BEGIN {printf \"%.2f\", ($LRU_HITS / $LRU_TOTAL) * 100}")
else
    LRU_HIT_RATE="0.00"
fi

if [ $S3FIFO_TOTAL -gt 0 ]; then
    S3FIFO_HIT_RATE=$(awk "BEGIN {printf \"%.2f\", ($S3FIFO_HITS / $S3FIFO_TOTAL) * 100}")
else
    S3FIFO_HIT_RATE="0.00"
fi

if [ "$LRU_HIT_RATE" != "0.00" ] && [ "$S3FIFO_HIT_RATE" != "0.00" ]; then
    IMPROVEMENT=$(awk "BEGIN {printf \"%.2f\", (($S3FIFO_HIT_RATE - $LRU_HIT_RATE) / $LRU_HIT_RATE) * 100}")
else
    IMPROVEMENT="0.00"
fi

echo -e "${BLUE}Results - Mixed Workload:${NC}"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
printf "%-20s | %15s | %15s | %15s\n" "Metric" "LRU" "S3-FIFO" "Difference"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
printf "%-20s | %15s | %15s | %15s\n" "Hit Rate (%)" "$LRU_HIT_RATE%" "$S3FIFO_HIT_RATE%" "$IMPROVEMENT%"
printf "%-20s | %15s | %15s | %15s\n" "Popular Items" "$LRU_HITS/50" "$S3FIFO_HITS/50" "$(($S3FIFO_HITS - $LRU_HITS))"
printf "%-20s | %15s | %15s | %15s\n" "Evicted Keys" "$LRU_EVICTED" "$S3FIFO_EVICTED" "$(($S3FIFO_EVICTED - $LRU_EVICTED))"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# Final summary
echo "=========================================="
echo "Summary"
echo "=========================================="
echo ""
echo "Key Takeaways:"
echo "  • S3-FIFO excels when there are many one-hit wonders"
echo "  • S3-FIFO promotes popular items (accessed 2+ times) to Main queue"
echo "  • LRU may perform better with stable, recency-based access patterns"
echo "  • Choose S3-FIFO for workloads with many temporary/cacheable items"
echo "  • Choose LRU for workloads with stable access patterns"
echo ""
echo -e "${GREEN}✓ Comparison complete${NC}"
echo ""
# Cleanup is handled by trap
