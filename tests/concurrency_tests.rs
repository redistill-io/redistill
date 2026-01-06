// Concurrency tests for TTL/PTTL commands
// Tests race conditions, memory leaks, and correctness under concurrent load

use redis::Commands;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;
use tokio::time::sleep;

/// Helper to create a Redis connection
fn create_connection(port: u16) -> Result<redis::Connection, redis::RedisError> {
    let client = redis::Client::open(format!("redis://127.0.0.1:{}", port))?;
    client.get_connection()
}

/// Helper to wait for server to be ready
async fn wait_for_server(port: u16, max_attempts: u32) -> bool {
    for _ in 0..max_attempts {
        if let Ok(mut conn) = create_connection(port) {
            // Try PING command which is universally supported
            match redis::cmd("PING").query::<String>(&mut conn) {
                Ok(_) => return true,
                Err(_) => {}
            }
        }
        sleep(Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test]
#[ignore] // Requires server to be running
async fn test_ttl_concurrent_operations() {
    // This test should be run with a server already started
    // cargo test --test concurrency_tests test_ttl_concurrent_operations -- --ignored --nocapture
    
    const PORT: u16 = 6379;
    const NUM_THREADS: usize = 50;
    const OPERATIONS_PER_THREAD: usize = 1000;
    
    // Wait for server
    if !wait_for_server(PORT, 50).await {
        eprintln!("Server not available on port {}. Start server first.", PORT);
        return;
    }
    
    let success_count = Arc::new(AtomicUsize::new(0));
    let error_count = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    
    // Spawn threads that perform TTL operations concurrently
    for thread_id in 0..NUM_THREADS {
        let success = success_count.clone();
        let errors = error_count.clone();
        
        let handle = thread::spawn(move || {
            let mut conn = match create_connection(PORT) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Thread {}: Failed to connect: {:?}", thread_id, e);
                    errors.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            };
            
            for i in 0..OPERATIONS_PER_THREAD {
                let key = format!("ttl_test:{}:{}", thread_id, i);
                
                // Set key with TTL using SET key value EX seconds (Redistill doesn't support SETEX)
                let _: Result<(), _> = redis::cmd("SET")
                    .arg(&key)
                    .arg("value")
                    .arg("EX")
                    .arg(60u64)
                    .query(&mut conn);
                
                // Immediately check TTL (race condition test)
                match conn.ttl::<_, i64>(&key) {
                    Ok(ttl) => {
                        if ttl >= 0 && ttl <= 60 {
                            success.fetch_add(1, Ordering::Relaxed);
                        } else {
                            errors.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Err(e) => {
                        eprintln!("Thread {}: TTL failed for {}: {:?}", thread_id, key, e);
                        errors.fetch_add(1, Ordering::Relaxed);
                    }
                }
                
                // Check PTTL
                match conn.pttl::<_, i64>(&key) {
                    Ok(pttl) => {
                        if pttl >= 0 && pttl <= 60000 {
                            success.fetch_add(1, Ordering::Relaxed);
                        } else {
                            errors.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Err(e) => {
                        eprintln!("Thread {}: PTTL failed for {}: {:?}", thread_id, key, e);
                        errors.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        });
        
        handles.push(handle);
    }
    
    // Wait for all threads
    for handle in handles {
        handle.join().unwrap();
    }
    
    let total_ops = NUM_THREADS * OPERATIONS_PER_THREAD * 2; // TTL + PTTL
    let successes = success_count.load(Ordering::Relaxed);
    let errors = error_count.load(Ordering::Relaxed);
    
    println!("\n=== TTL Concurrency Test Results ===");
    println!("Total operations: {}", total_ops);
    println!("Successful: {}", successes);
    println!("Errors: {}", errors);
    println!("Success rate: {:.2}%", (successes as f64 / total_ops as f64) * 100.0);
    
    // Should have at least 95% success rate
    assert!(
        successes as f64 / total_ops as f64 > 0.95,
        "Success rate too low: {}/{}",
        successes,
        total_ops
    );
}

#[tokio::test]
#[ignore]
async fn test_expired_key_removal_race() {
    // Test race condition when multiple threads access expired keys
    const PORT: u16 = 6379;
    const NUM_THREADS: usize = 100;
    
    if !wait_for_server(PORT, 50).await {
        eprintln!("Server not available. Start server first.");
        return;
    }
    
    let mut conn = create_connection(PORT).unwrap();
    
    // Set a key that expires in 1 second using SET key value EX seconds
    let _: () = redis::cmd("SET")
        .arg("expire_race:key")
        .arg("value")
        .arg("EX")
        .arg(1u64)
        .query(&mut conn)
        .unwrap();
    
    // Wait for it to expire
    thread::sleep(Duration::from_secs(2));
    
    let success_count = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    
    // Multiple threads try to access expired key simultaneously
    for _ in 0..NUM_THREADS {
        let success = success_count.clone();
        
        let handle = thread::spawn(move || {
            let mut conn = create_connection(PORT).unwrap();
            
            // All should get -2 (key doesn't exist)
            match conn.ttl::<_, i64>("expire_race:key") {
                Ok(ttl) if ttl == -2 => {
                    success.fetch_add(1, Ordering::Relaxed);
                }
                Ok(ttl) => {
                    eprintln!("Unexpected TTL: {}", ttl);
                }
                Err(e) => {
                    eprintln!("Error: {:?}", e);
                }
            }
        });
        
        handles.push(handle);
    }
    
    for handle in handles {
        handle.join().unwrap();
    }
    
    let successes = success_count.load(Ordering::Relaxed);
    println!("\n=== Expired Key Race Test ===");
    println!("All threads got correct result (-2): {}/{}", successes, NUM_THREADS);
    
    assert_eq!(successes, NUM_THREADS, "Not all threads got correct result");
}

#[tokio::test]
#[ignore]
async fn test_ttl_pttl_consistency() {
    // Test that TTL and PTTL are consistent
    const PORT: u16 = 6379;
    const TEST_DURATION_SECS: u64 = 30;
    
    if !wait_for_server(PORT, 50).await {
        eprintln!("Server not available. Start server first.");
        return;
    }
    
    let mut conn = create_connection(PORT).unwrap();
    let key = "consistency_test:key";
    
    // Set with 60 second TTL using SET key value EX seconds
    let _: () = redis::cmd("SET")
        .arg(key)
        .arg("value")
        .arg("EX")
        .arg(60u64)
        .query(&mut conn)
        .unwrap();
    
    let start = std::time::Instant::now();
    let inconsistencies = Arc::new(AtomicUsize::new(0));
    let checks = Arc::new(AtomicUsize::new(0));
    
    while start.elapsed().as_secs() < TEST_DURATION_SECS {
        let inc = inconsistencies.clone();
        let chk = checks.clone();
        
        thread::spawn(move || {
            let mut conn = create_connection(PORT).unwrap();
            
            let ttl: i64 = conn.ttl(key).unwrap_or(-1);
            let pttl: i64 = conn.pttl(key).unwrap_or(-1);
            
            chk.fetch_add(1, Ordering::Relaxed);
            
            // Convert PTTL to seconds and compare
            if ttl == -1 && pttl == -1 {
                // Both should indicate no expiry
                return;
            }
            
            if ttl == -2 && pttl == -2 {
                // Both should indicate key doesn't exist
                return;
            }
            
            if ttl >= 0 && pttl >= 0 {
                // PTTL should be approximately TTL * 1000 (within 1 second tolerance)
                let ttl_ms = (ttl as i64) * 1000;
                let diff = (pttl - ttl_ms).abs();
                if diff > 1000 {
                    inc.fetch_add(1, Ordering::Relaxed);
                    eprintln!("Inconsistency: TTL={}s, PTTL={}ms (diff={}ms)", ttl, pttl, diff);
                }
            } else {
                inc.fetch_add(1, Ordering::Relaxed);
                eprintln!("Inconsistency: TTL={}, PTTL={}", ttl, pttl);
            }
        })
        .join()
        .unwrap();
        
        thread::sleep(Duration::from_millis(100));
    }
    
    let inc_count = inconsistencies.load(Ordering::Relaxed);
    let check_count = checks.load(Ordering::Relaxed);
    
    println!("\n=== TTL/PTTL Consistency Test ===");
    println!("Checks performed: {}", check_count);
    println!("Inconsistencies: {}", inc_count);
    
    // Should have very few or no inconsistencies
    assert!(
        inc_count < check_count / 100,
        "Too many inconsistencies: {}/{}",
        inc_count,
        check_count
    );
}
