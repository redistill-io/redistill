// Redistill - High-performance Redis-compatible key-value store
//
// Module structure:
// - config.rs: Configuration loading and structs
// - store.rs: ShardedStore, Entry, buffer pool, eviction
// - protocol.rs: RESP parser and writer
// - server.rs: Connection handling, TLS, health check
// - persistence.rs: Snapshot save/load

// Global allocator - jemalloc for performance
#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: jemallocator::Jemalloc = jemallocator::Jemalloc;

mod config;
mod persistence;
mod protocol;
mod server;
mod store;

use bytes::Bytes;
use once_cell::sync::Lazy;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use tokio::signal;
use tokio_rustls::TlsAcceptor;

// Re-export from modules for internal use
use config::{CONFIG, EvictionPolicy, format_bytes};
use persistence::{
    LAST_SAVE_TIME, SAVE_IN_PROGRESS, load_snapshot, save_snapshot_sync, snapshot_task,
};
use protocol::{RespParser, RespWriter, eq_ignore_case_3, eq_ignore_case_6, parse_i64, parse_u64};
use server::{
    ConnectionState, MaybeStream, check_rate_limit, load_tls_config, start_health_check_server,
};
use store::{
    ACTIVE_CONNECTIONS, EVICTED_KEYS, MEMORY_USED, REJECTED_CONNECTIONS, ShardedStore,
    TOTAL_COMMANDS, TOTAL_CONNECTIONS, entry_size, evict_if_needed, expire_random_keys,
    get_timestamp,
};

// Server start time for uptime tracking
static START_TIME: Lazy<Instant> = Lazy::new(Instant::now);

// Thread-local command counter for batching updates
thread_local! {
    static LOCAL_CMD_COUNT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

// ==================== Background Tasks ====================

async fn expiration_task(store: ShardedStore) {
    let mut interval = tokio::time::interval(Duration::from_millis(100));
    loop {
        interval.tick().await;
        expire_random_keys(&store, 20);
    }
}

// ==================== Command Execution ====================

#[inline(always)]
fn execute_command(
    store: &ShardedStore,
    command: &[Bytes],
    writer: &mut RespWriter,
    state: &mut ConnectionState,
    now: u64,
) {
    // Batch counter updates
    LOCAL_CMD_COUNT.with(|count| {
        let new_count = count.get() + 1;
        if new_count >= 256 {
            TOTAL_COMMANDS.fetch_add(256, Ordering::Relaxed);
            count.set(0);
        } else {
            count.set(new_count);
        }
    });

    if command.is_empty() {
        writer.write_error(b"empty command");
        return;
    }

    let cmd = &command[0];

    // AUTH and PING don't require authentication
    let requires_auth = !matches!(cmd.len(), 4 if eq_ignore_case_3(&cmd[..3], b"aut") && (cmd[3] | 0x20) == b'h')
        && !matches!(cmd.len(), 4 if eq_ignore_case_3(&cmd[..3], b"pin") && (cmd[3] | 0x20) == b'g');

    if requires_auth && !state.authenticated {
        writer.write_error(b"NOAUTH Authentication required");
        return;
    }

    match cmd.len() {
        3 => {
            if eq_ignore_case_3(cmd, b"set") {
                handle_set(store, command, writer, now);
                return;
            }
            if eq_ignore_case_3(cmd, b"ttl") {
                handle_ttl(store, command, writer, now);
                return;
            }
            if eq_ignore_case_3(cmd, b"get") {
                if command.len() >= 2 {
                    match store.get(&command[1], now) {
                        Some(value) => writer.write_bulk_string(&value),
                        None => writer.write_null(),
                    }
                } else {
                    writer.write_error(b"wrong number of arguments");
                }
                return;
            }
            if eq_ignore_case_3(cmd, b"del") {
                if command.len() >= 2 {
                    let count = store.delete(&command[1..], now);
                    writer.write_integer(count);
                } else {
                    writer.write_error(b"wrong number of arguments");
                }
                return;
            }
        }
        4 => {
            if eq_ignore_case_3(&cmd[..3], b"pin") && (cmd[3] | 0x20) == b'g' {
                writer.write_simple_string(b"PONG");
                return;
            }
            if eq_ignore_case_3(&cmd[..3], b"key") && (cmd[3] | 0x20) == b's' {
                let keys = store.keys(now);
                writer.write_array(&keys);
                return;
            }
            if eq_ignore_case_3(&cmd[..3], b"sav") && (cmd[3] | 0x20) == b'e' {
                if !CONFIG.persistence.enabled {
                    writer.write_error(b"persistence is disabled");
                    return;
                }
                match save_snapshot_sync(store, &CONFIG.persistence.snapshot_path) {
                    Ok(count) => {
                        eprintln!("Snapshot saved: {} keys", count);
                        writer.write_simple_string(b"OK");
                    }
                    Err(e) => writer.write_error(e.as_bytes()),
                }
                return;
            }
            if eq_ignore_case_3(&cmd[..3], b"inc") && (cmd[3] | 0x20) == b'r' {
                handle_incr(store, command, writer, now, 1);
                return;
            }
            if eq_ignore_case_3(&cmd[..3], b"dec") && (cmd[3] | 0x20) == b'r' {
                handle_incr(store, command, writer, now, -1);
                return;
            }
            if eq_ignore_case_3(&cmd[..3], b"ptt") && (cmd[3] | 0x20) == b'l' {
                handle_pttl(store, command, writer, now);
                return;
            }
            if eq_ignore_case_3(&cmd[..3], b"mge") && (cmd[3] | 0x20) == b't' {
                handle_mget(store, command, writer, now);
                return;
            }
            if eq_ignore_case_3(&cmd[..3], b"mse") && (cmd[3] | 0x20) == b't' {
                handle_mset(store, command, writer, now);
                return;
            }
            if eq_ignore_case_3(&cmd[..3], b"aut") && (cmd[3] | 0x20) == b'h' {
                handle_auth(command, writer, state);
                return;
            }
            if eq_ignore_case_3(&cmd[..3], b"inf") && (cmd[3] | 0x20) == b'o' {
                handle_info(store, writer);
                return;
            }
            if cmd.len() == 4 {
                let lower = [
                    cmd[0] | 0x20,
                    cmd[1] | 0x20,
                    cmd[2] | 0x20,
                    cmd[3] | 0x20,
                ];
                if &lower == b"scan" {
                    handle_scan(store, command, writer, now);
                    return;
                }
            }
        }
        6 => {
            if eq_ignore_case_6(cmd, b"exists") {
                if command.len() >= 2 {
                    let count = store.exists(&command[1..], now);
                    writer.write_integer(count);
                } else {
                    writer.write_error(b"wrong number of arguments");
                }
                return;
            }
            if eq_ignore_case_6(cmd, b"bgsave") {
                if !CONFIG.persistence.enabled {
                    writer.write_error(b"persistence is disabled");
                    return;
                }
                if SAVE_IN_PROGRESS.load(Ordering::Relaxed) {
                    writer.write_error(b"Background save already in progress");
                    return;
                }
                let store_clone = store.clone();
                let path = CONFIG.persistence.snapshot_path.clone();
                std::thread::spawn(move || {
                    let _ = save_snapshot_sync(&store_clone, &path);
                });
                writer.write_simple_string(b"Background saving started");
                return;
            }
            if eq_ignore_case_6(cmd, b"dbsize") {
                writer.write_integer(store.len());
                return;
            }
            if eq_ignore_case_6(cmd, b"config") {
                writer.write_array(&[]);
                return;
            }
            if eq_ignore_case_6(cmd, b"incrby") {
                handle_incrby(store, command, writer, now);
                return;
            }
            if eq_ignore_case_6(cmd, b"decrby") {
                handle_decrby(store, command, writer, now);
                return;
            }
            if eq_ignore_case_6(cmd, b"expire") {
                handle_expire(store, command, writer, now);
                return;
            }
        }
        7 => {
            if cmd.len() == 7 {
                let lower = [
                    cmd[0] | 0x20,
                    cmd[1] | 0x20,
                    cmd[2] | 0x20,
                    cmd[3] | 0x20,
                    cmd[4] | 0x20,
                    cmd[5] | 0x20,
                    cmd[6] | 0x20,
                ];
                if &lower == b"flushdb" {
                    // Calculate actual memory that will be freed by iterating the store
                    // This ensures we only subtract what's actually being cleared
                    let now = get_timestamp();
                    let mut actual_memory = 0u64;
                    for shard in &store.shards {
                        for entry in shard.iter() {
                            let (key, val) = entry.pair();
                            // Only count non-expired keys (expired keys already have memory freed)
                            if val.expiry.is_none_or(|exp| now < exp) {
                                actual_memory += entry_size(key.len(), val.value.len()) as u64;
                            }
                        }
                    }
                    
                    // Clear the store first
                    store.clear();
                    
                    // Now subtract the calculated memory using CAS to handle concurrent operations
                    // We need to handle the case where other threads modified MEMORY_USED
                    // between our calculation and the subtraction
                    loop {
                        let current = MEMORY_USED.load(Ordering::Relaxed);
                        if actual_memory >= current {
                            // Calculated memory >= current (concurrent operations freed more than we calculated)
                            // Set to 0 to avoid underflow - any extra was from concurrent operations
                            if MEMORY_USED.compare_exchange(current, 0, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
                                break;
                            }
                        } else {
                            // Normal case: subtract what we calculated
                            let new_value = current - actual_memory;
                            if MEMORY_USED.compare_exchange(current, new_value, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
                                break;
                            }
                        }
                        // CAS failed - another thread modified MEMORY_USED, retry with new value
                    }
                    writer.write_simple_string(b"OK");
                    return;
                }
                if &lower == b"command" {
                    writer.write_array(&[]);
                    return;
                }
                if &lower == b"persist" {
                    handle_persist(store, command, writer, now);
                    return;
                }
            }
        }
        8 => {
            if cmd.len() == 8 {
                let lower = [
                    cmd[0] | 0x20,
                    cmd[1] | 0x20,
                    cmd[2] | 0x20,
                    cmd[3] | 0x20,
                    cmd[4] | 0x20,
                    cmd[5] | 0x20,
                    cmd[6] | 0x20,
                    cmd[7] | 0x20,
                ];
                if &lower == b"lastsave" {
                    writer.write_integer(LAST_SAVE_TIME.load(Ordering::Relaxed) as usize);
                    return;
                }
                if &lower == b"flushall" {
                    // Calculate actual memory that will be freed by iterating the store
                    // This ensures we only subtract what's actually being cleared
                    let now = get_timestamp();
                    let mut actual_memory = 0u64;
                    for shard in &store.shards {
                        for entry in shard.iter() {
                            let (key, val) = entry.pair();
                            // Only count non-expired keys (expired keys already have memory freed)
                            if val.expiry.is_none_or(|exp| now < exp) {
                                actual_memory += entry_size(key.len(), val.value.len()) as u64;
                            }
                        }
                    }
                    
                    // Clear the store first
                    store.clear();
                    
                    // Now subtract the calculated memory using CAS to handle concurrent operations
                    // We need to handle the case where other threads modified MEMORY_USED
                    // between our calculation and the subtraction
                    loop {
                        let current = MEMORY_USED.load(Ordering::Relaxed);
                        if actual_memory >= current {
                            // Calculated memory >= current (concurrent operations freed more than we calculated)
                            // Set to 0 to avoid underflow - any extra was from concurrent operations
                            if MEMORY_USED.compare_exchange(current, 0, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
                                break;
                            }
                        } else {
                            // Normal case: subtract what we calculated
                            let new_value = current - actual_memory;
                            if MEMORY_USED.compare_exchange(current, new_value, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
                                break;
                            }
                        }
                        // CAS failed - another thread modified MEMORY_USED, retry with new value
                    }
                    writer.write_simple_string(b"OK");
                    return;
                }
            }
        }
        _ => {}
    }

    writer.write_error(b"unknown command");
}

// ==================== Command Handlers ====================

#[inline(always)]
fn handle_set(store: &ShardedStore, command: &[Bytes], writer: &mut RespWriter, now: u64) {
    if command.len() < 3 {
        writer.write_error(b"wrong number of arguments");
        return;
    }

    let key = &command[1];
    let value = &command[2];
    let size = entry_size(key.len(), value.len());

    if !evict_if_needed(store, size) {
        writer.write_error(b"OOM command not allowed when used memory > 'maxmemory'");
        return;
    }

    let mut ttl: Option<u64> = None;
    let mut nx = false;
    let mut xx = false;
    let mut get = false;

    let mut i = 3;
    while i < command.len() {
        let opt = &command[i];
        if opt.len() == 2 {
            let o0 = opt[0] | 0x20;
            let o1 = opt[1] | 0x20;
            if o0 == b'e' && o1 == b'x' {
                if i + 1 >= command.len() {
                    writer.write_error(b"syntax error");
                    return;
                }
                i += 1;
                match parse_u64(&command[i]) {
                    Some(v) if v > 0 => ttl = Some(v),
                    _ => {
                        writer.write_error(b"value is not an integer or out of range");
                        return;
                    }
                }
            } else if o0 == b'p' && o1 == b'x' {
                if i + 1 >= command.len() {
                    writer.write_error(b"syntax error");
                    return;
                }
                i += 1;
                match parse_u64(&command[i]) {
                    Some(v) if v > 0 => ttl = Some(v.div_ceil(1000)),
                    _ => {
                        writer.write_error(b"value is not an integer or out of range");
                        return;
                    }
                }
            } else if o0 == b'n' && o1 == b'x' {
                nx = true;
            } else if o0 == b'x' && o1 == b'x' {
                xx = true;
            } else {
                writer.write_error(b"syntax error");
                return;
            }
        } else if opt.len() == 3
            && (opt[0] | 0x20) == b'g'
            && (opt[1] | 0x20) == b'e'
            && (opt[2] | 0x20) == b't'
        {
            get = true;
        } else {
            writer.write_error(b"syntax error");
            return;
        }
        i += 1;
    }

    if nx && xx {
        writer.write_error(b"XX and NX options at the same time are not compatible");
        return;
    }

    let shard = &store.shards[store.hash(key)];
    let old_value = if nx || xx || get {
        shard.get(key.as_ref()).and_then(|entry| {
            if let Some(expiry) = entry.expiry
                && now >= expiry
            {
                return None;
            }
            Some(entry.value.clone())
        })
    } else {
        None
    };

    let key_exists = old_value.is_some();

    if nx && key_exists {
        if get {
            if let Some(v) = old_value {
                writer.write_bulk_string(&v);
            } else {
                writer.write_null();
            }
        } else {
            writer.write_null();
        }
        return;
    }

    if xx && !key_exists {
        writer.write_null();
        return;
    }

    let old_size = store.set(key.clone(), value.clone(), ttl, now);

    if CONFIG.memory.max_memory > 0 {
        if let Some(old) = old_size {
            MEMORY_USED.fetch_sub(old as u64, Ordering::Relaxed);
        }
        MEMORY_USED.fetch_add(size as u64, Ordering::Relaxed);
    }

    if get {
        match old_value {
            Some(v) => writer.write_bulk_string(&v),
            None => writer.write_null(),
        }
    } else {
        writer.write_simple_string(b"OK");
    }
}

#[inline(always)]
fn handle_ttl(store: &ShardedStore, command: &[Bytes], writer: &mut RespWriter, now: u64) {
    if command.len() < 2 {
        writer.write_error(b"wrong number of arguments");
        return;
    }
    let key = &command[1];
    let shard = &store.shards[store.hash(key)];

    match shard.get(key.as_ref()) {
        Some(entry) => match entry.expiry {
            Some(expiry) => {
                if now >= expiry {
                    // Extract info before dropping to avoid race condition
                    let key_bytes = Bytes::copy_from_slice(key);
                    let key_len = key_bytes.len();
                    let value_len = entry.value.len();
                    drop(entry);
                    
                    // Atomic remove - only one thread can succeed
                    if shard.remove(key_bytes.as_ref()).is_some() && CONFIG.memory.max_memory > 0 {
                        let size = entry_size(key_len, value_len);
                        MEMORY_USED.fetch_sub(size as u64, Ordering::Relaxed);
                    }
                    writer.write_signed_integer(-2);
                } else {
                    writer.write_signed_integer((expiry - now) as i64);
                }
            }
            None => writer.write_signed_integer(-1),
        },
        None => writer.write_signed_integer(-2),
    }
}

#[inline(always)]
fn handle_pttl(store: &ShardedStore, command: &[Bytes], writer: &mut RespWriter, now: u64) {
    if command.len() < 2 {
        writer.write_error(b"wrong number of arguments");
        return;
    }
    let key = &command[1];
    let shard = &store.shards[store.hash(key)];

    match shard.get(key.as_ref()) {
        Some(entry) => match entry.expiry {
            Some(expiry) => {
                if now >= expiry {
                    // Extract info before dropping to avoid race condition
                    let key_bytes = Bytes::copy_from_slice(key);
                    let key_len = key_bytes.len();
                    let value_len = entry.value.len();
                    drop(entry);
                    
                    // Atomic remove - only one thread can succeed
                    if shard.remove(key_bytes.as_ref()).is_some() && CONFIG.memory.max_memory > 0 {
                        let size = entry_size(key_len, value_len);
                        MEMORY_USED.fetch_sub(size as u64, Ordering::Relaxed);
                    }
                    writer.write_signed_integer(-2);
                } else {
                    // Calculate milliseconds with overflow protection
                    let diff = expiry.saturating_sub(now);
                    let millis = diff.saturating_mul(1000);
                    let result = millis.min(i64::MAX as u64) as i64;
                    writer.write_signed_integer(result);
                }
            }
            None => writer.write_signed_integer(-1),
        },
        None => writer.write_signed_integer(-2),
    }
}

#[inline(always)]
fn handle_incr(
    store: &ShardedStore,
    command: &[Bytes],
    writer: &mut RespWriter,
    now: u64,
    delta: i64,
) {
    if command.len() < 2 {
        writer.write_error(b"wrong number of arguments");
        return;
    }
    let key = &command[1];
    let shard = &store.shards[store.hash(key)];

    // Read key once to get value, TTL, and old size atomically
    let (current, existing_ttl, old_size_for_eviction) = match shard.get(key.as_ref()) {
        Some(entry) => {
            if let Some(expiry) = entry.expiry {
                if now >= expiry {
                    // Expired - remove it, treat as 0, no TTL to preserve
                    let key_bytes = Bytes::copy_from_slice(key);
                    let key_len = key_bytes.len();
                    let value_len = entry.value.len();
                    drop(entry);
                    if shard.remove(key_bytes.as_ref()).is_some() && CONFIG.memory.max_memory > 0 {
                        let size = entry_size(key_len, value_len);
                        MEMORY_USED.fetch_sub(size as u64, Ordering::Relaxed);
                    }
                    (0i64, None, 0)
                } else {
                    // Not expired - parse value and preserve TTL
                    let value = match parse_i64(&entry.value) {
                        Some(v) => v,
                        None => {
                            writer.write_error(b"value is not an integer or out of range");
                            return;
                        }
                    };
                    let ttl = Some(expiry.saturating_sub(now));
                    let old_size = entry_size(key.len(), entry.value.len());
                    (value, ttl, old_size)
                }
            } else {
                // No expiry - parse value, no TTL
                let value = match parse_i64(&entry.value) {
                    Some(v) => v,
                    None => {
                        writer.write_error(b"value is not an integer or out of range");
                        return;
                    }
                };
                let old_size = entry_size(key.len(), entry.value.len());
                (value, None, old_size)
            }
        }
        None => (0i64, None, 0),
    };

    let new_val = if delta > 0 {
        match current.checked_add(delta) {
            Some(v) => v,
            None => {
                writer.write_error(b"increment would produce overflow");
                return;
            }
        }
    } else {
        match current.checked_add(delta) {
            Some(v) => v,
            None => {
                writer.write_error(b"decrement would produce overflow");
                return;
            }
        }
    };

    let val_bytes = Bytes::from(new_val.to_string());
    let size = entry_size(key.len(), val_bytes.len());
    
    let net_size = size.saturating_sub(old_size_for_eviction);

    if net_size > 0 && !evict_if_needed(store, net_size) {
        writer.write_error(b"OOM command not allowed when used memory > 'maxmemory'");
        return;
    }

    let old_size = store.set(key.clone(), val_bytes, existing_ttl, now);

    if CONFIG.memory.max_memory > 0 {
        if let Some(old) = old_size {
            MEMORY_USED.fetch_sub(old as u64, Ordering::Relaxed);
        }
        MEMORY_USED.fetch_add(size as u64, Ordering::Relaxed);
    }

    writer.write_signed_integer(new_val);
}

#[inline(always)]
fn handle_incrby(store: &ShardedStore, command: &[Bytes], writer: &mut RespWriter, now: u64) {
    if command.len() < 3 {
        writer.write_error(b"wrong number of arguments");
        return;
    }
    let increment = match parse_i64(&command[2]) {
        Some(v) => v,
        None => {
            writer.write_error(b"value is not an integer or out of range");
            return;
        }
    };
    handle_incr(store, command, writer, now, increment);
}

#[inline(always)]
fn handle_decrby(store: &ShardedStore, command: &[Bytes], writer: &mut RespWriter, now: u64) {
    if command.len() < 3 {
        writer.write_error(b"wrong number of arguments");
        return;
    }
    let decrement = match parse_i64(&command[2]) {
        Some(v) => v,
        None => {
            writer.write_error(b"value is not an integer or out of range");
            return;
        }
    };
    // Check for overflow: -i64::MIN cannot be represented as i64
    let delta = if decrement == i64::MIN {
        writer.write_error(b"value is not an integer or out of range");
        return;
    } else {
        -decrement
    };
    handle_incr(store, command, writer, now, delta);
}

#[inline(always)]
fn handle_mget(store: &ShardedStore, command: &[Bytes], writer: &mut RespWriter, now: u64) {
    if command.len() < 2 {
        writer.write_error(b"wrong number of arguments");
        return;
    }
    writer.write_array_header(command.len() - 1);
    for key in &command[1..] {
        match store.get(key, now) {
            Some(value) => writer.write_bulk_string(&value),
            None => writer.write_null(),
        }
    }
}

#[inline(always)]
fn handle_mset(store: &ShardedStore, command: &[Bytes], writer: &mut RespWriter, now: u64) {
    if command.len() < 3 || !(command.len() - 1).is_multiple_of(2) {
        writer.write_error(b"wrong number of arguments for MSET");
        return;
    }

    let pairs = (command.len() - 1) / 2;
    // Calculate net memory change by checking existing keys
    let mut net_size = 0i64;
    for i in 0..pairs {
        let key = &command[1 + i * 2];
        let value = &command[2 + i * 2];
        let new_size = entry_size(key.len(), value.len()) as i64;
        
        // Check if key exists and get old size
        let old_size = store.get_existing_size(key, now).unwrap_or(0) as i64;
        net_size += new_size - old_size;
    }

    // Only evict if net increase is positive
    if net_size > 0 && !evict_if_needed(store, net_size as usize) {
        writer.write_error(b"OOM command not allowed when used memory > 'maxmemory'");
        return;
    }

    for i in 0..pairs {
        let key = &command[1 + i * 2];
        let value = &command[2 + i * 2];
        let size = entry_size(key.len(), value.len());
        let old_size = store.set(key.clone(), value.clone(), None, now);

        if CONFIG.memory.max_memory > 0 {
            if let Some(old) = old_size {
                MEMORY_USED.fetch_sub(old as u64, Ordering::Relaxed);
            }
            MEMORY_USED.fetch_add(size as u64, Ordering::Relaxed);
        }
    }

    writer.write_simple_string(b"OK");
}

fn handle_auth(command: &[Bytes], writer: &mut RespWriter, state: &mut ConnectionState) {
    if CONFIG.security.password.is_empty() {
        writer.write_error(b"ERR Client sent AUTH, but no password is set");
        return;
    }
    if command.len() < 2 {
        writer.write_error(b"ERR wrong number of arguments for 'auth' command");
        return;
    }
    let provided = command[1].as_ref();
    let expected = CONFIG.security.password.as_bytes();
    if provided.ct_eq(expected).into() {
        state.authenticated = true;
        writer.write_simple_string(b"OK");
    } else {
        writer.write_error(b"ERR invalid password");
    }
}

fn handle_info(store: &ShardedStore, writer: &mut RespWriter) {
    let uptime = START_TIME.elapsed().as_secs();
    let total_commands = TOTAL_COMMANDS.load(Ordering::Relaxed);
    let total_connections = TOTAL_CONNECTIONS.load(Ordering::Relaxed);
    let active_connections = ACTIVE_CONNECTIONS.load(Ordering::Relaxed);
    let db_size = store.len();
    let memory_used = MEMORY_USED.load(Ordering::Relaxed);
    let evicted_keys = EVICTED_KEYS.load(Ordering::Relaxed);
    let max_memory = CONFIG.memory.max_memory;
    let eviction_policy: EvictionPolicy = CONFIG.memory.eviction_policy.parse().unwrap_or_default();
    let rejected_connections = REJECTED_CONNECTIONS.load(Ordering::Relaxed);

    let info = format!(
        "# Server\r\n\
        redis_version:7.0.0\r\n\
        redis_mode:standalone\r\n\
        os:Redistill\r\n\
        arch_bits:64\r\n\
        process_id:{}\r\n\
        uptime_in_seconds:{}\r\n\
        \r\n\
        # Clients\r\n\
        connected_clients:{}\r\n\
        \r\n\
        # Memory\r\n\
        used_memory:{}\r\n\
        used_memory_human:{}\r\n\
        maxmemory:{}\r\n\
        maxmemory_human:{}\r\n\
        maxmemory_policy:{}\r\n\
        evicted_keys:{}\r\n\
        \r\n\
        # Stats\r\n\
        total_connections_received:{}\r\n\
        total_commands_processed:{}\r\n\
        rejected_connections:{}\r\n\
        \r\n\
        # Keyspace\r\n\
        db0:keys={},expires=0,avg_ttl=0\r\n",
        std::process::id(),
        uptime,
        active_connections,
        memory_used,
        format_bytes(memory_used),
        max_memory,
        if max_memory > 0 {
            format_bytes(max_memory)
        } else {
            "unlimited".to_string()
        },
        eviction_policy.as_str(),
        evicted_keys,
        total_connections,
        total_commands,
        rejected_connections,
        db_size
    );
    writer.write_bulk_string(info.as_bytes());
}

#[inline(always)]
fn handle_expire(store: &ShardedStore, command: &[Bytes], writer: &mut RespWriter, now: u64) {
    if command.len() < 3 {
        writer.write_error(b"wrong number of arguments");
        return;
    }
    let key = &command[1];
    let seconds = match parse_i64(&command[2]) {
        Some(v) if v > 0 => v as u64,
        Some(_) => {
            let count = store.delete(&[command[1].clone()], now);
            writer.write_integer(count);
            return;
        }
        None => {
            writer.write_error(b"value is not an integer or out of range");
            return;
        }
    };

    let shard = &store.shards[store.hash(key)];
    if let Some(mut entry) = shard.get_mut(key.as_ref()) {
        if let Some(expiry) = entry.expiry
            && now >= expiry
        {
            // Extract info before dropping to avoid race condition
            let key_bytes = Bytes::copy_from_slice(key);
            let key_len = key_bytes.len();
            let value_len = entry.value.len();
            drop(entry);
            
            // Atomic remove - only one thread can succeed
            if shard.remove(key_bytes.as_ref()).is_some() && CONFIG.memory.max_memory > 0 {
                let size = entry_size(key_len, value_len);
                MEMORY_USED.fetch_sub(size as u64, Ordering::Relaxed);
            }
            writer.write_integer(0);
            return;
        }
        entry.expiry = Some(now + seconds);
        writer.write_integer(1);
    } else {
        writer.write_integer(0);
    }
}

fn handle_scan(store: &ShardedStore, command: &[Bytes], writer: &mut RespWriter, now: u64) {
    if command.len() < 2 {
        writer.write_error(b"wrong number of arguments for 'scan' command");
        return;
    }

    // Parse cursor (must be provided)
    let cursor = match parse_u64(&command[1]) {
        Some(c) => c,
        None => {
            writer.write_error(b"invalid cursor");
            return;
        }
    };

    let mut count_hint = 10; // Default count hint
    let mut pattern: Option<Bytes> = None;

    // Parse optional arguments: MATCH pattern, COUNT count
    let mut i = 2;
    while i < command.len() {
        let arg = &command[i];
        let arg_lower: Vec<u8> = arg.iter().map(|b| b | 0x20).collect();

        if arg_lower == b"match" && i + 1 < command.len() {
            i += 1;
            pattern = Some(command[i].clone());
        } else if arg_lower == b"count" && i + 1 < command.len() {
            i += 1;
            match parse_u64(&command[i]) {
                Some(c) => count_hint = c as usize,
                None => {
                    writer.write_error(b"value is not an integer or out of range");
                    return;
                }
            }
        } else {
            writer.write_error(b"syntax error");
            return;
        }
        i += 1;
    }

    // Perform scan
    let (new_cursor, keys) = store.scan(
        cursor,
        count_hint,
        pattern.as_ref().map(|p| p.as_ref()),
        now,
    );

    // Write response: [cursor, [key1, key2, ...]]
    writer.write_array_header(2);
    writer.write_bulk_string(new_cursor.to_string().as_bytes());
    writer.write_array_header(keys.len());
    for key in &keys {
        writer.write_bulk_string(key);
    }
}

fn handle_persist(store: &ShardedStore, command: &[Bytes], writer: &mut RespWriter, now: u64) {
    if command.len() < 2 {
        writer.write_error(b"wrong number of arguments");
        return;
    }
    let key = &command[1];
    let shard = &store.shards[store.hash(key)];

    if let Some(mut entry) = shard.get_mut(key.as_ref()) {
        if let Some(expiry) = entry.expiry {
            if now >= expiry {
                // Extract info before dropping to avoid race condition
                let key_bytes = Bytes::copy_from_slice(key);
                let key_len = key_bytes.len();
                let value_len = entry.value.len();
                drop(entry);
                
                // Atomic remove - only one thread can succeed
                if shard.remove(key_bytes.as_ref()).is_some() && CONFIG.memory.max_memory > 0 {
                    let size = entry_size(key_len, value_len);
                    MEMORY_USED.fetch_sub(size as u64, Ordering::Relaxed);
                }
                writer.write_integer(0);
            } else {
                entry.expiry = None;
                writer.write_integer(1);
            }
        } else {
            writer.write_integer(0);
        }
    } else {
        writer.write_integer(0);
    }
}

// ==================== Connection Handling ====================

async fn handle_connection(mut stream: MaybeStream, store: ShardedStore) {
    let _ = stream.set_nodelay(CONFIG.performance.tcp_nodelay);

    TOTAL_CONNECTIONS.fetch_add(1, Ordering::Relaxed);
    ACTIVE_CONNECTIONS.fetch_add(1, Ordering::Relaxed);

    let mut parser = RespParser::new();
    let mut writer = RespWriter::new();
    let mut state = ConnectionState::new();
    let mut batch_count = 0;

    let timeout_duration = if CONFIG.server.connection_timeout > 0 {
        Some(Duration::from_secs(CONFIG.server.connection_timeout))
    } else {
        None
    };

    loop {
        let now = get_timestamp();

        let parse_result = if let Some(timeout) = timeout_duration {
            tokio::time::timeout(timeout, parser.parse_command(&mut stream)).await
        } else {
            Ok(parser.parse_command(&mut stream).await)
        };

        match parse_result {
            Ok(Ok(command)) => {
                execute_command(&store, &command, &mut writer, &mut state, now);
                batch_count += 1;

                if batch_count >= CONFIG.server.batch_size
                    || writer.should_flush()
                    || !parser.has_buffered_data()
                {
                    if writer.flush(&mut stream).await.is_err() {
                        break;
                    }
                    batch_count = 0;
                }
            }
            Ok(Err(_)) | Err(_) => break,
        }
    }

    // Flush remaining command count before connection closes
    LOCAL_CMD_COUNT.with(|count| {
        let remaining = count.get();
        if remaining > 0 {
            TOTAL_COMMANDS.fetch_add(remaining, Ordering::Relaxed);
            count.set(0);
        }
    });

    let _ = writer.flush(&mut stream).await;
    ACTIVE_CONNECTIONS.fetch_sub(1, Ordering::Relaxed);
}

// ==================== Main Entry Point ====================

#[tokio::main]
async fn main() {
    // Force config initialization
    let config = &*CONFIG;

    // Initialize store
    let store = ShardedStore::new(config.server.num_shards);

    // Force START_TIME initialization
    let _ = *START_TIME;

    println!();
    println!("╔═══════════════════════════════════════════╗");
    println!("║         Redistill v1.2.5                  ║");
    println!("║   High-Performance Redis-Compatible KV    ║");
    println!("╚═══════════════════════════════════════════╝");
    println!();
    println!("Configuration:");
    println!("   • Shards: {}", config.server.num_shards);
    println!("   • Buffer size: {} bytes", config.server.buffer_size);
    println!(
        "   • Buffer pool: {} buffers",
        config.server.buffer_pool_size
    );
    println!("   • Max connections: {}", config.server.max_connections);
    println!(
        "   • Connection timeout: {}s",
        config.server.connection_timeout
    );
    println!(
        "   • Rate limit: {}",
        if config.server.connection_rate_limit > 0 {
            format!("{} conn/sec", config.server.connection_rate_limit)
        } else {
            "disabled".to_string()
        }
    );
    println!(
        "   • TLS: {}",
        if config.security.tls_enabled {
            "enabled"
        } else {
            "disabled"
        }
    );
    println!(
        "   • TCP_NODELAY: {}",
        if config.performance.tcp_nodelay {
            "enabled"
        } else {
            "disabled"
        }
    );
    println!(
        "   • Max memory: {}",
        if config.memory.max_memory > 0 {
            format_bytes(config.memory.max_memory)
        } else {
            "unlimited".to_string()
        }
    );
    println!("   • Eviction policy: {}", config.memory.eviction_policy);
    println!(
        "   • Persistence: {}",
        if config.persistence.enabled {
            format!(
                "enabled (interval: {}s, path: {})",
                config.persistence.snapshot_interval, config.persistence.snapshot_path
            )
        } else {
            "disabled".to_string()
        }
    );

    // Load snapshot if persistence is enabled
    if config.persistence.enabled {
        print!(
            "Loading snapshot from {}... ",
            config.persistence.snapshot_path
        );
        match load_snapshot(&store, &config.persistence.snapshot_path) {
            Ok(0) => println!("no snapshot found"),
            Ok(count) => println!("loaded {} keys", count),
            Err(e) => {
                eprintln!("failed: {}", e);
                eprintln!("Starting with empty database");
            }
        }
    }

    // Load TLS configuration if enabled
    let tls_acceptor = if config.security.tls_enabled {
        if config.security.tls_cert_path.is_empty() || config.security.tls_key_path.is_empty() {
            eprintln!("TLS enabled but cert/key paths not configured");
            std::process::exit(1);
        }

        match load_tls_config(
            &config.security.tls_cert_path,
            &config.security.tls_key_path,
        )
        .await
        {
            Ok(tls_config) => {
                println!("TLS/SSL enabled");
                println!("   • Certificate: {}", config.security.tls_cert_path);
                println!("   • Private Key: {}", config.security.tls_key_path);
                Some(TlsAcceptor::from(tls_config))
            }
            Err(e) => {
                eprintln!("Failed to load TLS configuration: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        None
    };

    let bind_addr = format!("{}:{}", config.server.bind, config.server.port);
    let listener = TcpListener::bind(&bind_addr).await.unwrap_or_else(|e| {
        eprintln!("Failed to bind to {}: {}", bind_addr, e);
        std::process::exit(1);
    });

    println!("Listening on {}", bind_addr);

    if !config.security.password.is_empty() {
        println!("Authentication enabled");
    } else {
        println!("Authentication disabled");
    }

    // Start health check endpoint if enabled
    if config.server.health_check_port > 0 {
        tokio::spawn(start_health_check_server(config.server.health_check_port));
    }

    // Start passive key expiration background task
    tokio::spawn(expiration_task(store.clone()));

    // Start periodic snapshot background task if enabled
    if config.persistence.enabled && config.persistence.snapshot_interval > 0 {
        tokio::spawn(snapshot_task(
            store.clone(),
            config.persistence.snapshot_interval,
            config.persistence.snapshot_path.clone(),
        ));
    }

    println!();

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((tcp_stream, _)) => {
                        let active = ACTIVE_CONNECTIONS.load(Ordering::Relaxed);
                        if CONFIG.server.max_connections > 0 && active >= CONFIG.server.max_connections {
                            REJECTED_CONNECTIONS.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }

                        if !check_rate_limit() {
                            REJECTED_CONNECTIONS.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }

                        let store_clone = store.clone();
                        let tls_acceptor_clone = tls_acceptor.clone();

                        tokio::spawn(async move {
                            let stream = if let Some(acceptor) = tls_acceptor_clone {
                                match acceptor.accept(tcp_stream).await {
                                    Ok(tls_stream) => MaybeStream::Tls(Box::new(tls_stream)),
                                    Err(e) => {
                                        eprintln!("TLS handshake failed: {}", e);
                                        return;
                                    }
                                }
                            } else {
                                MaybeStream::Plain(tcp_stream)
                            };

                            handle_connection(stream, store_clone).await;
                        });
                    }
                    Err(e) => eprintln!("Accept error: {}", e),
                }
            }
            _ = signal::ctrl_c() => {
                println!("\n\nReceived shutdown signal...");

                if CONFIG.persistence.enabled && CONFIG.persistence.save_on_shutdown {
                    print!("Saving final snapshot... ");
                    match save_snapshot_sync(&store, &CONFIG.persistence.snapshot_path) {
                        Ok(count) => println!("saved {} keys", count),
                        Err(e) => eprintln!("failed: {}", e),
                    }
                }

                println!("Final Stats:");
                println!("   • Total connections: {}", TOTAL_CONNECTIONS.load(Ordering::Relaxed));
                println!("   • Total commands: {}", TOTAL_COMMANDS.load(Ordering::Relaxed));
                println!("   • Active connections: {}", ACTIVE_CONNECTIONS.load(Ordering::Relaxed));
                println!("   • Keys in database: {}", store.len());
                println!("\nRedistill shut down gracefully");
                break;
            }
        }
    }
}
