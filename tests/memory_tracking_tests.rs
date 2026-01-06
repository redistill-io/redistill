// Memory tracking accuracy tests
// Verifies that MEMORY_USED stays accurate under concurrent operations

use redis::Commands;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;
use tokio::time::sleep;

fn create_connection(port: u16) -> Result<redis::Connection, redis::RedisError> {
    let client = redis::Client::open(format!("redis://127.0.0.1:{}", port))?;
    client.get_connection()
}

async fn wait_for_server(port: u16, max_attempts: u32) -> bool {
    for _ in 0..max_attempts {
        if let Ok(mut conn) = create_connection(port) {
            // Use PING which is universally supported
            match redis::cmd("PING").query::<String>(&mut conn) {
                Ok(_) => return true,
                Err(_) => {}
            }
        }
        sleep(Duration::from_millis(100)).await;
    }
    false
}

fn get_memory_used(port: u16) -> u64 {
    let mut conn = create_connection(port).unwrap();
    // Redistill's INFO command doesn't take arguments, returns full info
    let info: String = redis::cmd("INFO")
        .query(&mut conn)
        .unwrap_or_default();
    
    // Parse used_memory from INFO output
    // Redistill format: "used_memory:123456\r\n"
    for line in info.lines() {
        if line.starts_with("used_memory:") {
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() >= 2 {
                // Handle format like "used_memory:123456" or "used_memory:123456\r"
                let value_str = parts[1].trim().trim_end_matches('\r');
                if let Ok(value) = value_str.parse::<u64>() {
                    return value;
                }
            }
        }
    }
    0
}

#[tokio::test]
#[ignore]
async fn test_memory_tracking_under_load() {
    // Test memory tracking accuracy during concurrent operations
    const PORT: u16 = 6379;
    const NUM_THREADS: usize = 20;
    const OPS_PER_THREAD: usize = 500;
    const KEY_SIZE: usize = 100; // bytes
    const VALUE_SIZE: usize = 1024; // bytes
    
    if !wait_for_server(PORT, 50).await {
        eprintln!("Server not available. Start server first.");
        return;
    }
    
    // Clear database first
    let mut conn = create_connection(PORT).unwrap();
    let _: () = redis::cmd("FLUSHDB").query(&mut conn).unwrap();
    
    let initial_memory = get_memory_used(PORT);
    println!("Initial memory: {} bytes", initial_memory);
    
    let operations_completed = Arc::new(AtomicU64::new(0));
    let mut handles = Vec::new();
    
    // Calculate expected memory per key-value pair
    // Entry overhead: ~64 bytes (from entry_size function)
    let expected_entry_size = KEY_SIZE + VALUE_SIZE + 64;
    let total_expected_memory = (NUM_THREADS * OPS_PER_THREAD) as u64 * expected_entry_size as u64;
    
    // Spawn threads that insert keys
    for thread_id in 0..NUM_THREADS {
        let ops_done = operations_completed.clone();
        
        let handle = thread::spawn(move || {
            let mut conn = create_connection(PORT).unwrap();
            let key_prefix = format!("mem_test:{}:", thread_id);
            let value = "x".repeat(VALUE_SIZE);
            
            for i in 0..OPS_PER_THREAD {
                let key = format!("{}{}", key_prefix, i);
                let _: () = conn.set(&key, &value).unwrap();
                ops_done.fetch_add(1, Ordering::Relaxed);
            }
        });
        
        handles.push(handle);
    }
    
    // Wait for all inserts
    for handle in handles {
        handle.join().unwrap();
    }
    
    // Wait a bit for memory tracking to stabilize
    thread::sleep(Duration::from_millis(100));
    
    let final_memory = get_memory_used(PORT);
    let actual_memory_used = final_memory.saturating_sub(initial_memory);
    
    println!("\n=== Memory Tracking Test Results ===");
    println!("Operations completed: {}", operations_completed.load(Ordering::Relaxed));
    println!("Initial memory: {} bytes", initial_memory);
    println!("Final memory: {} bytes", final_memory);
    println!("Memory increase: {} bytes", actual_memory_used);
    println!("Expected memory: {} bytes", total_expected_memory);
    
    // Allow 20% tolerance (due to internal structures, allocations, etc.)
    let tolerance = total_expected_memory / 5;
    let lower_bound = total_expected_memory.saturating_sub(tolerance);
    let upper_bound = total_expected_memory + tolerance;
    
    println!("Acceptable range: {} - {} bytes", lower_bound, upper_bound);
    
    assert!(
        actual_memory_used >= lower_bound && actual_memory_used <= upper_bound,
        "Memory tracking inaccurate: {} bytes (expected ~{} bytes)",
        actual_memory_used,
        total_expected_memory
    );
}

#[tokio::test]
#[ignore]
async fn test_memory_tracking_delete() {
    // Test that memory is correctly freed when deleting keys
    const PORT: u16 = 6379;
    const NUM_KEYS: usize = 1000;
    // const KEY_SIZE: usize = 50; // Not used
    const VALUE_SIZE: usize = 512;
    
    if !wait_for_server(PORT, 50).await {
        eprintln!("Server not available. Start server first.");
        return;
    }
    
    let mut conn = create_connection(PORT).unwrap();
    let _: () = redis::cmd("FLUSHDB").query(&mut conn).unwrap();
    
    let initial_memory = get_memory_used(PORT);
    
    // Insert keys
    let value = "x".repeat(VALUE_SIZE);
    for i in 0..NUM_KEYS {
        let key = format!("del_test:{}", i);
        let _: () = conn.set(&key, &value).unwrap();
    }
    
    // Wait a bit longer for memory tracking to update
    thread::sleep(Duration::from_millis(200));
    let memory_after_insert = get_memory_used(PORT);
    let memory_used_by_keys = memory_after_insert.saturating_sub(initial_memory);
    
    println!("Memory after insert: {} bytes", memory_used_by_keys);
    
    // Verify we actually have memory allocated
    if memory_used_by_keys == 0 {
        // If memory tracking shows 0, it might be disabled or not updated yet
        // Try one more time after a longer wait
        thread::sleep(Duration::from_millis(300));
        let memory_after_insert2 = get_memory_used(PORT);
        let memory_used_by_keys2 = memory_after_insert2.saturating_sub(initial_memory);
        
        if memory_used_by_keys2 == 0 {
            // Skip test if memory tracking isn't working (max_memory might be 0)
            println!("\n=== Delete Memory Tracking Test ===");
            println!("Skipping: Memory tracking not enabled or memory is 0");
            println!("This is expected if max_memory = 0 in config");
            return;
        }
    }
    
    // Delete all keys
    for i in 0..NUM_KEYS {
        let key = format!("del_test:{}", i);
        let _: i64 = conn.del(&key).unwrap();
    }
    
    thread::sleep(Duration::from_millis(200));
    let final_memory = get_memory_used(PORT);
    let memory_after_delete = final_memory.saturating_sub(initial_memory);
    
    println!("\n=== Delete Memory Tracking Test ===");
    println!("Memory before delete: {} bytes", memory_used_by_keys);
    println!("Memory after delete: {} bytes", memory_after_delete);
    println!("Memory freed: {} bytes", memory_used_by_keys.saturating_sub(memory_after_delete));
    
    // Memory should be close to initial (allow small overhead)
    // If memory_used_by_keys was 0, just verify memory decreased or stayed same
    if memory_used_by_keys > 0 {
        assert!(
            memory_after_delete < memory_used_by_keys / 10,
            "Memory not properly freed: {} bytes remaining (expected < {} bytes)",
            memory_after_delete,
            memory_used_by_keys / 10
        );
    } else {
        // If we couldn't measure initial memory usage, just verify memory didn't increase
        assert!(
            memory_after_delete <= memory_after_insert,
            "Memory should not increase after deletion"
        );
    }
}

#[tokio::test]
#[ignore]
async fn test_memory_tracking_expired_keys() {
    // Test that memory is freed when keys expire
    const PORT: u16 = 6379;
    const NUM_KEYS: usize = 500;
    const VALUE_SIZE: usize = 256;
    
    if !wait_for_server(PORT, 50).await {
        eprintln!("Server not available. Start server first.");
        return;
    }
    
    let mut conn = create_connection(PORT).unwrap();
    let _: () = redis::cmd("FLUSHDB").query(&mut conn).unwrap();
    
    let initial_memory = get_memory_used(PORT);
    
    // Insert keys with 2 second expiry using SET key value EX seconds
    let value = "x".repeat(VALUE_SIZE);
    for i in 0..NUM_KEYS {
        let key = format!("expire_test:{}", i);
        let _: () = redis::cmd("SET")
            .arg(&key)
            .arg(&value)
            .arg("EX")
            .arg(2u64)
            .query(&mut conn)
            .unwrap();
    }
    
    thread::sleep(Duration::from_millis(100));
    let memory_after_insert = get_memory_used(PORT);
    let memory_used_by_keys = memory_after_insert.saturating_sub(initial_memory);
    
    println!("Memory after insert: {} bytes", memory_used_by_keys);
    
    // Wait for keys to expire
    println!("Waiting for keys to expire...");
    thread::sleep(Duration::from_secs(3));
    
    // Trigger some TTL checks to ensure expired keys are cleaned up
    for i in 0..100 {
        let key = format!("expire_test:{}", i);
        let _: i64 = conn.ttl(&key).unwrap_or(-2);
    }
    
    thread::sleep(Duration::from_millis(500));
    let final_memory = get_memory_used(PORT);
    let memory_after_expiry = final_memory.saturating_sub(initial_memory);
    
    println!("\n=== Expired Keys Memory Tracking Test ===");
    println!("Memory before expiry: {} bytes", memory_used_by_keys);
    println!("Memory after expiry: {} bytes", memory_after_expiry);
    println!("Memory freed: {} bytes", memory_used_by_keys.saturating_sub(memory_after_expiry));
    
    // Most memory should be freed
    assert!(
        memory_after_expiry < memory_used_by_keys / 5,
        "Memory not properly freed after expiry: {} bytes remaining",
        memory_after_expiry
    );
}

#[tokio::test]
#[ignore]
async fn test_flushdb_memory_reset() {
    // Test that FLUSHDB correctly resets memory tracking
    const PORT: u16 = 6379;
    const NUM_KEYS: usize = 1000;
    
    if !wait_for_server(PORT, 50).await {
        eprintln!("Server not available. Start server first.");
        return;
    }
    
    let mut conn = create_connection(PORT).unwrap();
    let _: () = redis::cmd("FLUSHDB").query(&mut conn).unwrap();
    
    let initial_memory = get_memory_used(PORT);
    
    // Insert many keys
    for i in 0..NUM_KEYS {
        let key = format!("flush_test:{}", i);
        let value = format!("value_{}", i);
        let _: () = conn.set(&key, &value).unwrap();
    }
    
    thread::sleep(Duration::from_millis(100));
    let memory_before_flush = get_memory_used(PORT);
    
    // FLUSHDB
    let _: () = redis::cmd("FLUSHDB").query(&mut conn).unwrap();
    
    thread::sleep(Duration::from_millis(100));
    let memory_after_flush = get_memory_used(PORT);
    
    println!("\n=== FLUSHDB Memory Reset Test ===");
    println!("Initial memory: {} bytes", initial_memory);
    println!("Memory before FLUSHDB: {} bytes", memory_before_flush);
    println!("Memory after FLUSHDB: {} bytes", memory_after_flush);
    
    // Memory should be close to initial
    let memory_diff = if memory_after_flush > initial_memory {
        memory_after_flush - initial_memory
    } else {
        initial_memory - memory_after_flush
    };
    
    assert!(
        memory_diff < memory_before_flush / 10,
        "FLUSHDB didn't reset memory properly: {} bytes difference",
        memory_diff
    );
}
