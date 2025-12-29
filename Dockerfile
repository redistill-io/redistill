# Multi-stage Dockerfile for Redistill
# Build stage
# Using latest stable Rust (edition 2024 requires Rust 1.81+)
FROM rust:alpine AS builder

# Install build dependencies
# Note: Alpine already uses musl, so we only need musl-dev for headers
# gcc and musl-dev are needed for jemalloc-sys to compile
RUN apk add --no-cache \
    musl-dev \
    gcc \
    make

# Set working directory
WORKDIR /build

# Copy manifest files
COPY Cargo.toml Cargo.lock ./

# Copy source code (needed for dependency resolution with lib.rs)
COPY src ./src

# Build the application
# For local testing: build for native architecture (Alpine is musl-based)
# For CI/CD: GitHub Actions will handle multi-arch builds with proper cross-compilation
# Detect architecture and use appropriate musl target
RUN ARCH=$(uname -m) && \
    case $ARCH in \
        x86_64) TARGET=x86_64-unknown-linux-musl ;; \
        aarch64) TARGET=aarch64-unknown-linux-musl ;; \
        *) echo "Unsupported architecture: $ARCH" && exit 1 ;; \
    esac && \
    rustup target add $TARGET && \
    cargo build --release --target $TARGET && \
    strip target/$TARGET/release/redistill && \
    echo $TARGET > /build/.target_arch

# Runtime stage
FROM alpine:latest

# Install ca-certificates for TLS support (if needed)
RUN apk add --no-cache ca-certificates tzdata

# Create non-root user
RUN addgroup -g 1000 redistill && \
    adduser -D -u 1000 -G redistill redistill

# Create app directory
WORKDIR /app

# Copy binary from builder (use wildcard to handle any architecture)
COPY --from=builder /build/.target_arch /tmp/.target_arch
COPY --from=builder /build/target/*/release/redistill /app/redistill

# Create config directory (optional, for volume mounts)
RUN mkdir -p /app/config && \
    chown -R redistill:redistill /app

# Switch to non-root user
USER redistill

# Expose ports
# 6379: Redis protocol port
# 8080: Health check HTTP port
EXPOSE 6379 8080

# Health check
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD wget --quiet --tries=1 --spider http://localhost:8080/health || exit 1

# ============================================================================
# Default Configuration - Optimized for Production Performance
# Based on extensive benchmarking (6.87M GET ops/s, 2.74M SET ops/s)
# ============================================================================

# Network Configuration
ENV REDIS_BIND=0.0.0.0
ENV REDIS_PORT=6379
ENV REDIS_HEALTH_CHECK_PORT=8080

# Performance Tuning (Optimal settings from benchmarking)
# num_shards: 2048 provides best balance (use 4096 for GET-heavy workloads)
ENV REDIS_NUM_SHARDS=2048
# batch_size: 256 matches pipeline depth for deep pipelines (optimal for P > 64)
ENV REDIS_BATCH_SIZE=256
# buffer_size: 16KB per buffer
ENV REDIS_BUFFER_SIZE=16384
# buffer_pool_size: 2048 buffers for optimal tail latency
ENV REDIS_BUFFER_POOL_SIZE=2048

# Connection Management
ENV REDIS_MAX_CONNECTIONS=10000

# Memory Management
# max_memory: 0 = unlimited (set to bytes if you want a limit, e.g., 2147483648 for 2GB)
ENV REDIS_MAX_MEMORY=0
# eviction_policy: allkeys-lru (options: allkeys-lru, allkeys-random, noeviction)
ENV REDIS_EVICTION_POLICY=allkeys-lru

# TCP Performance Settings
ENV REDIS_TCP_NODELAY=true
ENV REDIS_TCP_KEEPALIVE=60

# Security (set via docker run -e REDIS_PASSWORD=your-password)
# ENV REDIS_PASSWORD=

# Run the application
ENTRYPOINT ["/app/redistill"]
