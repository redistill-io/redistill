#!/bin/bash
# Local Docker testing script for Redistill

set -e

echo "🐳 Redistill Docker Testing Script"
echo "===================================="
echo ""

# Colors for output
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m' # No Color

# Check if Docker is running
if ! docker info > /dev/null 2>&1; then
    echo -e "${RED}❌ Docker is not running. Please start Docker and try again.${NC}"
    exit 1
fi

echo -e "${GREEN}✓ Docker is running${NC}"

# Build the image
echo ""
echo "📦 Building Docker image..."
docker build -t redistill:test .

if [ $? -ne 0 ]; then
    echo -e "${RED}❌ Build failed${NC}"
    exit 1
fi

echo -e "${GREEN}✓ Image built successfully${NC}"

# Stop any existing container
echo ""
echo "🛑 Stopping any existing containers..."
docker stop redistill-test 2>/dev/null || true
docker rm redistill-test 2>/dev/null || true

# Run the container
echo ""
echo "🚀 Starting container..."
docker run -d \
    --name redistill-test \
    -p 6379:6379 \
    -p 8080:8080 \
    redistill:test

if [ $? -ne 0 ]; then
    echo -e "${RED}❌ Failed to start container${NC}"
    exit 1
fi

echo -e "${GREEN}✓ Container started${NC}"

# Wait for container to be ready
echo ""
echo "⏳ Waiting for Redistill to be ready..."
sleep 5

# Check if container is running
if ! docker ps | grep -q redistill-test; then
    echo -e "${RED}❌ Container is not running${NC}"
    echo "Container logs:"
    docker logs redistill-test
    exit 1
fi

# Test health check endpoint
echo ""
echo "🏥 Testing health check endpoint..."
HEALTH_RESPONSE=$(curl -s http://localhost:8080/health || echo "FAILED")

if [ "$HEALTH_RESPONSE" = "FAILED" ]; then
    echo -e "${YELLOW}⚠ Health check endpoint not responding (this might be OK if health_check_port is disabled)${NC}"
else
    echo -e "${GREEN}✓ Health check response:${NC}"
    echo "$HEALTH_RESPONSE" | head -5
fi

# Test Redis protocol with redis-cli if available
echo ""
if command -v redis-cli &> /dev/null; then
    echo "🔌 Testing Redis protocol with redis-cli..."
    
    # Test PING
    PING_RESPONSE=$(redis-cli -p 6379 ping 2>/dev/null || echo "FAILED")
    if [ "$PING_RESPONSE" = "PONG" ]; then
        echo -e "${GREEN}✓ PING command: ${PING_RESPONSE}${NC}"
    else
        echo -e "${YELLOW}⚠ PING test failed or redis-cli not configured correctly${NC}"
    fi
    
    # Test SET/GET
    SET_RESPONSE=$(redis-cli -p 6379 set test:docker "hello-world" 2>/dev/null || echo "FAILED")
    if [ "$SET_RESPONSE" = "OK" ]; then
        echo -e "${GREEN}✓ SET command: ${SET_RESPONSE}${NC}"
        
        GET_RESPONSE=$(redis-cli -p 6379 get test:docker 2>/dev/null || echo "FAILED")
        if [ "$GET_RESPONSE" = "\"hello-world\"" ] || [ "$GET_RESPONSE" = "hello-world" ]; then
            echo -e "${GREEN}✓ GET command: ${GET_RESPONSE}${NC}"
        else
            echo -e "${YELLOW}⚠ GET test failed: ${GET_RESPONSE}${NC}"
        fi
    else
        echo -e "${YELLOW}⚠ SET test failed: ${SET_RESPONSE}${NC}"
    fi
else
    echo -e "${YELLOW}⚠ redis-cli not found. Install it to test Redis protocol.${NC}"
    echo "   You can test manually with: redis-cli -p 6379 ping"
fi

# Show container info
echo ""
echo "📊 Container Information:"
echo "=========================="
docker ps --filter name=redistill-test --format "table {{.Names}}\t{{.Status}}\t{{.Ports}}"

echo ""
echo "📋 Container Logs (last 10 lines):"
echo "===================================="
docker logs --tail 10 redistill-test

echo ""
echo -e "${GREEN}✅ Testing complete!${NC}"
echo ""
echo "To interact with the container:"
echo "  - Redis CLI: redis-cli -p 6379"
echo "  - Health check: curl http://localhost:8080/health"
echo "  - View logs: docker logs -f redistill-test"
echo "  - Stop container: docker stop redistill-test"
echo "  - Remove container: docker rm redistill-test"

