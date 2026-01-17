# Redistill Tests

This directory contains all testing infrastructure for Redistill.

## Structure

```
tests/
├── unit_tests.rs   # Unit tests (core functionality tests)
├── integration/    # Integration tests (full server tests)
├── unit/           # Reserved for future organized unit test modules
├── certs/          # Test TLS certificates (self-signed)
├── scripts/        # Testing utility scripts
└── benchmarks/     # Performance benchmarks
```

## Running Tests

### All Tests
```bash
cargo test
```

### Unit Tests Only
```bash
cargo test --test unit_tests
```

### Integration Tests Only
```bash
cargo test --test integration
```

### Benchmarks
```bash
cd tests/benchmarks
./run_benchmarks.sh
```

## Test Organization

### Unit Tests (`tests/unit_tests.rs`)
- Tests for core data structures (ShardedStore, Entry, etc.)
- Helper function tests (byte comparison, formatting, etc.)
- Configuration tests
- Policy tests (eviction, etc.)

All unit tests are in `tests/unit_tests.rs` to keep them separate from `main.rs` while still testing internal implementation through the library interface.

### Integration Tests (`tests/integration/`)
- Full server functionality tests
- RESP protocol tests
- TLS/SSL connection tests
- Multi-client scenario tests

### S3-FIFO Tests (`tests/scripts/test_s3fifo.sh`)
- S3-FIFO eviction algorithm tests
- Small queue behavior (one-hit wonders)
- Main queue promotion (popular items)
- Ghost queue functionality

## Test Certificates

The `certs/` directory contains self-signed certificates for testing TLS:
- **DO NOT use these in production**
- Regenerate with: `./scripts/generate_test_certs.sh`
- Valid for 365 days

## Adding Tests

### Unit Test
Add to `tests/unit_tests.rs`:
```rust
#[test]
fn test_new_feature() {
    let store = create_test_store();
    // Your test here
}
```

### Integration Test
Create file in `tests/integration/`:
```rust
#[tokio::test]
async fn test_redis_protocol() {
    // Test full server functionality
}
```

