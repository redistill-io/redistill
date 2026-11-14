use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_redistill");

fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
}

fn wait_for_port(port: u16, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("server on {} never came up", port);
}

struct Guard(Option<Child>);
impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(mut c) = self.0.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Guard {
    fn stop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn base_cmd(port: u16) -> Command {
    let mut c = Command::new(BIN);
    c.env("REDIS_PORT", port.to_string())
        .env("REDIS_BIND", "127.0.0.1")
        .env("REDIS_HEALTH_CHECK_PORT", "0")
        .env("REDIS_PERSISTENCE_ENABLED", "false")
        .env("REDIS_AOF_ENABLED", "false")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    c
}

fn spawn_primary(port: u16) -> Child {
    base_cmd(port).spawn().expect("spawn primary")
}

fn spawn_replica(port: u16, master_port: u16) -> Child {
    base_cmd(port)
        .env("REDIS_REPLICAOF", format!("127.0.0.1:{}", master_port))
        .spawn()
        .expect("spawn replica")
}

fn encode(parts: &[&[u8]]) -> Vec<u8> {
    let mut v = format!("*{}\r\n", parts.len()).into_bytes();
    for p in parts {
        v.extend_from_slice(format!("${}\r\n", p.len()).as_bytes());
        v.extend_from_slice(p);
        v.extend_from_slice(b"\r\n");
    }
    v
}

fn req(conn: &mut TcpStream, parts: &[&[u8]]) -> Vec<u8> {
    conn.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    conn.write_all(&encode(parts)).unwrap();
    let mut buf = vec![0u8; 256];
    let n = conn.read(&mut buf).unwrap();
    buf.truncate(n);
    buf
}

/// Poll GET on a fresh connection until it returns the expected bulk string.
fn wait_for_value(port: u16, key: &[u8], expected: &[u8], timeout: Duration) {
    let want = {
        let mut v = format!("${}\r\n", expected.len()).into_bytes();
        v.extend_from_slice(expected);
        v.extend_from_slice(b"\r\n");
        v
    };
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(mut c) = TcpStream::connect(("127.0.0.1", port))
            && req(&mut c, &[b"GET", key]) == want
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "replica never converged on key {:?}",
        String::from_utf8_lossy(key)
    );
}

fn wait_for_link(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(mut conn) = TcpStream::connect(("127.0.0.1", port)) {
            conn.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            conn.write_all(&encode(&[b"INFO"])).unwrap();
            let mut buf = [0u8; 4096];
            if let Ok(n) = conn.read(&mut buf)
                && buf[..n]
                    .windows(b"master_link_status:up".len())
                    .any(|w| w == b"master_link_status:up")
            {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("replica link never came up");
}

fn wait_for_link_down(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(mut conn) = TcpStream::connect(("127.0.0.1", port)) {
            conn.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            conn.write_all(&encode(&[b"INFO"])).unwrap();
            let mut buf = [0u8; 4096];
            if let Ok(n) = conn.read(&mut buf)
                && buf[..n]
                    .windows(b"master_link_status:down".len())
                    .any(|w| w == b"master_link_status:down")
            {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("replica link stayed up after its primary changed role");
}

fn temp_aof() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir()
        .join(format!(
            "redistill-replication-{}-{nanos}.aof",
            std::process::id()
        ))
        .to_string_lossy()
        .into_owned()
}

#[test]
fn replica_full_syncs_existing_data() {
    let mport = free_port();
    let _mp = Guard(Some(spawn_primary(mport)));
    wait_for_port(mport, Duration::from_secs(5));

    let mut m = TcpStream::connect(("127.0.0.1", mport)).unwrap();
    assert_eq!(req(&mut m, &[b"SET", b"pre", b"loaded"]), b"+OK\r\n");

    let rport = free_port();
    let _rp = Guard(Some(spawn_replica(rport, mport)));
    wait_for_port(rport, Duration::from_secs(5));

    wait_for_value(rport, b"pre", b"loaded", Duration::from_secs(5));
}

#[test]
fn writes_propagate_to_replica() {
    let mport = free_port();
    let _mp = Guard(Some(spawn_primary(mport)));
    wait_for_port(mport, Duration::from_secs(5));
    let rport = free_port();
    let _rp = Guard(Some(spawn_replica(rport, mport)));
    wait_for_port(rport, Duration::from_secs(5));

    let mut m = TcpStream::connect(("127.0.0.1", mport)).unwrap();
    assert_eq!(req(&mut m, &[b"SET", b"live", b"yes"]), b"+OK\r\n");
    wait_for_value(rport, b"live", b"yes", Duration::from_secs(5));
}

#[test]
fn replica_rejects_writes() {
    let mport = free_port();
    let _mp = Guard(Some(spawn_primary(mport)));
    wait_for_port(mport, Duration::from_secs(5));
    let rport = free_port();
    let _rp = Guard(Some(spawn_replica(rport, mport)));
    wait_for_port(rport, Duration::from_secs(5));
    // Give the replica a moment to establish replica role.
    std::thread::sleep(Duration::from_millis(800));

    let mut r = TcpStream::connect(("127.0.0.1", rport)).unwrap();
    let reply = req(&mut r, &[b"SET", b"x", b"1"]);
    assert!(
        reply.starts_with(b"-READONLY"),
        "expected READONLY, got {:?}",
        String::from_utf8_lossy(&reply)
    );
}

#[test]
fn incrby_applies_exactly_once() {
    let mport = free_port();
    let _mp = Guard(Some(spawn_primary(mport)));
    wait_for_port(mport, Duration::from_secs(5));

    let mut m = TcpStream::connect(("127.0.0.1", mport)).unwrap();
    assert_eq!(req(&mut m, &[b"SET", b"counter", b"0"]), b"+OK\r\n");

    let rport = free_port();
    let _rp = Guard(Some(spawn_replica(rport, mport)));
    wait_for_port(rport, Duration::from_secs(5));

    for _ in 0..50 {
        let reply = req(&mut m, &[b"INCRBY", b"counter", b"1"]);
        assert!(
            reply.starts_with(b":"),
            "got {:?}",
            String::from_utf8_lossy(&reply)
        );
    }
    // Replica must converge to exactly 50 — proves the snapshot/stream cut
    // applied every INCRBY exactly once (no gap, no double-apply).
    wait_for_value(rport, b"counter", b"50", Duration::from_secs(5));
}

#[test]
fn promoted_replica_accepts_writes() {
    let mport = free_port();
    let _mp = Guard(Some(spawn_primary(mport)));
    wait_for_port(mport, Duration::from_secs(5));
    let rport = free_port();
    let _rp = Guard(Some(spawn_replica(rport, mport)));
    wait_for_port(rport, Duration::from_secs(5));
    std::thread::sleep(Duration::from_millis(800));

    let mut r = TcpStream::connect(("127.0.0.1", rport)).unwrap();
    assert!(req(&mut r, &[b"SET", b"x", b"1"]).starts_with(b"-READONLY"));
    assert_eq!(req(&mut r, &[b"REPLICAOF", b"NO", b"ONE"]), b"+OK\r\n");
    assert_eq!(req(&mut r, &[b"SET", b"x", b"1"]), b"+OK\r\n");
}

#[test]
fn concurrent_mset_and_set_keep_replica_in_primary_order() {
    let mport = free_port();
    let _mp = Guard(Some(spawn_primary(mport)));
    wait_for_port(mport, Duration::from_secs(5));
    let rport = free_port();
    let _rp = Guard(Some(spawn_replica(rport, mport)));
    wait_for_port(rport, Duration::from_secs(5));
    wait_for_link(rport);

    let mut parts = Vec::with_capacity(600_003);
    parts.push(b"MSET".to_vec());
    parts.push(b"early".to_vec());
    parts.push(b"A".to_vec());
    for i in 0..299_999 {
        parts.push(format!("fill:{i}").into_bytes());
        parts.push(b"x".to_vec());
    }
    parts.push(b"last".to_vec());
    parts.push(b"x".to_vec());
    let mset = std::thread::spawn(move || {
        let refs: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
        let mut conn = TcpStream::connect(("127.0.0.1", mport)).unwrap();
        conn.set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        conn.write_all(&encode(&refs)).unwrap();
        let mut buf = [0; 64];
        let n = conn.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"+OK\r\n");
    });

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut interleaved = false;
    while Instant::now() < deadline && !mset.is_finished() {
        let mut conn = TcpStream::connect(("127.0.0.1", mport)).unwrap();
        if req(&mut conn, &[b"GET", b"early"]) == b"$1\r\nA\r\n" {
            assert_eq!(req(&mut conn, &[b"SET", b"early", b"B"]), b"+OK\r\n");
            interleaved = true;
            break;
        }
    }
    mset.join().unwrap();
    assert!(
        interleaved,
        "MSET completed before the overlapping SET could be sent"
    );
    let mut primary = TcpStream::connect(("127.0.0.1", mport)).unwrap();
    assert_eq!(req(&mut primary, &[b"SET", b"fence", b"done"]), b"+OK\r\n");
    wait_for_value(rport, b"fence", b"done", Duration::from_secs(15));
    let mut replica = TcpStream::connect(("127.0.0.1", rport)).unwrap();
    assert_eq!(req(&mut primary, &[b"GET", b"early"]), b"$1\r\nB\r\n");
    assert_eq!(req(&mut replica, &[b"GET", b"early"]), b"$1\r\nB\r\n");
}

#[test]
fn primary_eviction_removes_key_from_replica() {
    let mport = free_port();
    let _mp = Guard(Some(
        base_cmd(mport)
            .env("REDIS_MAX_MEMORY", "150")
            .env("REDIS_NUM_SHARDS", "1")
            .env("REDIS_EVICTION_POLICY", "allkeys-lru")
            .spawn()
            .unwrap(),
    ));
    wait_for_port(mport, Duration::from_secs(5));
    let rport = free_port();
    let _rp = Guard(Some(spawn_replica(rport, mport)));
    wait_for_port(rport, Duration::from_secs(5));
    wait_for_link(rport);

    let value = b"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
    let mut primary = TcpStream::connect(("127.0.0.1", mport)).unwrap();
    assert_eq!(req(&mut primary, &[b"SET", b"first", value]), b"+OK\r\n");
    wait_for_value(rport, b"first", value, Duration::from_secs(5));
    assert_eq!(req(&mut primary, &[b"SET", b"second", value]), b"+OK\r\n");
    wait_for_value(rport, b"second", value, Duration::from_secs(5));
    let mut replica = TcpStream::connect(("127.0.0.1", rport)).unwrap();
    assert_eq!(req(&mut primary, &[b"GET", b"first"]), b"$-1\r\n");
    assert_eq!(req(&mut replica, &[b"GET", b"first"]), b"$-1\r\n");
}

#[test]
fn oom_rejected_write_is_not_replicated() {
    let mport = free_port();
    let _mp = Guard(Some(
        base_cmd(mport)
            .env("REDIS_MAX_MEMORY", "150")
            .env("REDIS_NUM_SHARDS", "1")
            .env("REDIS_EVICTION_POLICY", "noeviction")
            .spawn()
            .unwrap(),
    ));
    wait_for_port(mport, Duration::from_secs(5));
    let rport = free_port();
    let _rp = Guard(Some(spawn_replica(rport, mport)));
    wait_for_port(rport, Duration::from_secs(5));
    wait_for_link(rport);

    let value = b"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
    let mut primary = TcpStream::connect(("127.0.0.1", mport)).unwrap();
    assert_eq!(req(&mut primary, &[b"SET", b"first", value]), b"+OK\r\n");
    let rejected = req(&mut primary, &[b"SET", b"second", value]);
    assert!(rejected.starts_with(b"-"), "expected OOM, got {:?}", rejected);
    let rejected = req(&mut primary, &[b"INCR", b"counter"]);
    assert!(rejected.starts_with(b"-"), "expected OOM, got {:?}", rejected);
    // A later accepted write fences the stream: once it lands, anything
    // propagated before it has been applied too.
    assert_eq!(req(&mut primary, &[b"DEL", b"first"]), b":1\r\n");
    assert_eq!(req(&mut primary, &[b"SET", b"fence", b"1"]), b"+OK\r\n");
    wait_for_value(rport, b"fence", b"1", Duration::from_secs(5));
    let mut replica = TcpStream::connect(("127.0.0.1", rport)).unwrap();
    assert_eq!(req(&mut replica, &[b"GET", b"second"]), b"$-1\r\n");
    assert_eq!(req(&mut replica, &[b"GET", b"counter"]), b"$-1\r\n");
}

#[test]
fn replica_aof_restores_full_sync_after_offline_restart() {
    let mport = free_port();
    let mut mp = Guard(Some(spawn_primary(mport)));
    wait_for_port(mport, Duration::from_secs(5));
    let mut primary = TcpStream::connect(("127.0.0.1", mport)).unwrap();
    assert_eq!(req(&mut primary, &[b"SET", b"counter", b"10"]), b"+OK\r\n");

    let rport = free_port();
    let aof = temp_aof();
    let spawn_with_aof = || {
        base_cmd(rport)
            .env("REDIS_REPLICAOF", format!("127.0.0.1:{mport}"))
            .env("REDIS_AOF_ENABLED", "true")
            .env("REDIS_AOF_PATH", &aof)
            .env("REDIS_AOF_FSYNC", "always")
            .env("REDIS_AOF_REWRITE_PERCENTAGE", "0")
            .spawn()
            .unwrap()
    };
    let mut rp = Guard(Some(spawn_with_aof()));
    wait_for_port(rport, Duration::from_secs(5));
    wait_for_value(rport, b"counter", b"10", Duration::from_secs(5));
    assert_eq!(req(&mut primary, &[b"INCR", b"counter"]), b":11\r\n");
    wait_for_value(rport, b"counter", b"11", Duration::from_secs(5));

    rp.stop();
    mp.stop();
    let restarted = Guard(Some(spawn_with_aof()));
    wait_for_port(rport, Duration::from_secs(5));
    let mut replica = TcpStream::connect(("127.0.0.1", rport)).unwrap();
    assert_eq!(req(&mut replica, &[b"GET", b"counter"]), b"$2\r\n11\r\n");
    drop(restarted);
    let _ = std::fs::remove_file(&aof);
}

#[test]
fn demoted_primary_disconnects_its_existing_replicas() {
    let old_primary_port = free_port();
    let _old_primary = Guard(Some(spawn_primary(old_primary_port)));
    wait_for_port(old_primary_port, Duration::from_secs(5));
    let replica_port = free_port();
    let _replica = Guard(Some(spawn_replica(replica_port, old_primary_port)));
    wait_for_port(replica_port, Duration::from_secs(5));
    wait_for_link(replica_port);

    let new_primary_port = free_port();
    let _new_primary = Guard(Some(spawn_primary(new_primary_port)));
    wait_for_port(new_primary_port, Duration::from_secs(5));
    let mut old_primary = TcpStream::connect(("127.0.0.1", old_primary_port)).unwrap();
    let port = new_primary_port.to_string();
    assert_eq!(
        req(
            &mut old_primary,
            &[b"REPLICAOF", b"127.0.0.1", port.as_bytes()]
        ),
        b"+OK\r\n"
    );
    wait_for_link_down(replica_port);
}

#[test]
fn replica_reconnects_with_a_new_full_snapshot() {
    let mport = free_port();
    let mut primary_process = Guard(Some(spawn_primary(mport)));
    wait_for_port(mport, Duration::from_secs(5));
    let rport = free_port();
    let _replica_process = Guard(Some(spawn_replica(rport, mport)));
    wait_for_port(rport, Duration::from_secs(5));
    wait_for_link(rport);

    let mut primary = TcpStream::connect(("127.0.0.1", mport)).unwrap();
    assert_eq!(req(&mut primary, &[b"SET", b"old", b"value"]), b"+OK\r\n");
    wait_for_value(rport, b"old", b"value", Duration::from_secs(5));

    primary_process.stop();
    wait_for_link_down(rport);
    primary_process.0 = Some(spawn_primary(mport));
    wait_for_port(mport, Duration::from_secs(5));
    let mut replacement = TcpStream::connect(("127.0.0.1", mport)).unwrap();
    assert_eq!(
        req(&mut replacement, &[b"SET", b"new", b"value"]),
        b"+OK\r\n"
    );
    wait_for_value(rport, b"new", b"value", Duration::from_secs(10));
    wait_for_link(rport);
    let mut replica = TcpStream::connect(("127.0.0.1", rport)).unwrap();
    assert_eq!(req(&mut replica, &[b"GET", b"old"]), b"$-1\r\n");
}

#[test]
fn repoint_replica_replaces_previous_master_image() {
    let old_port = free_port();
    let new_port = free_port();
    let _old = Guard(Some(spawn_primary(old_port)));
    let _new = Guard(Some(spawn_primary(new_port)));
    wait_for_port(old_port, Duration::from_secs(5));
    wait_for_port(new_port, Duration::from_secs(5));
    let mut old = TcpStream::connect(("127.0.0.1", old_port)).unwrap();
    let mut new = TcpStream::connect(("127.0.0.1", new_port)).unwrap();
    assert_eq!(req(&mut old, &[b"SET", b"old", b"value"]), b"+OK\r\n");
    assert_eq!(req(&mut new, &[b"SET", b"new", b"value"]), b"+OK\r\n");

    let replica_port = free_port();
    let _replica = Guard(Some(spawn_replica(replica_port, old_port)));
    wait_for_port(replica_port, Duration::from_secs(5));
    wait_for_value(replica_port, b"old", b"value", Duration::from_secs(5));
    let mut replica = TcpStream::connect(("127.0.0.1", replica_port)).unwrap();
    let port = new_port.to_string();
    assert_eq!(
        req(&mut replica, &[b"REPLICAOF", b"127.0.0.1", port.as_bytes()]),
        b"+OK\r\n"
    );
    wait_for_value(replica_port, b"new", b"value", Duration::from_secs(5));
    assert_eq!(req(&mut replica, &[b"GET", b"old"]), b"$-1\r\n");
}

#[test]
fn replica_aof_full_sync_overrides_older_local_rdb_on_restart() {
    let mport = free_port();
    let mut primary_process = Guard(Some(spawn_primary(mport)));
    wait_for_port(mport, Duration::from_secs(5));
    let rport = free_port();
    let aof = temp_aof();
    let rdb = format!("{aof}.rdb");

    let mut seed = Guard(Some(
        base_cmd(rport)
            .env("REDIS_PERSISTENCE_ENABLED", "true")
            .env("REDIS_SNAPSHOT_PATH", &rdb)
            .spawn()
            .unwrap(),
    ));
    wait_for_port(rport, Duration::from_secs(5));
    let mut seed_conn = TcpStream::connect(("127.0.0.1", rport)).unwrap();
    assert_eq!(req(&mut seed_conn, &[b"SET", b"stale", b"old"]), b"+OK\r\n");
    assert_eq!(req(&mut seed_conn, &[b"SAVE"]), b"+OK\r\n");
    seed.stop();

    let spawn_persistent_replica = || {
        base_cmd(rport)
            .env("REDIS_REPLICAOF", format!("127.0.0.1:{mport}"))
            .env("REDIS_PERSISTENCE_ENABLED", "true")
            .env("REDIS_SNAPSHOT_PATH", &rdb)
            .env("REDIS_AOF_ENABLED", "true")
            .env("REDIS_AOF_PATH", &aof)
            .env("REDIS_AOF_FSYNC", "always")
            .env("REDIS_AOF_REWRITE_PERCENTAGE", "0")
            .spawn()
            .unwrap()
    };
    let mut replica_process = Guard(Some(spawn_persistent_replica()));
    wait_for_port(rport, Duration::from_secs(5));
    wait_for_link(rport);
    let mut replica = TcpStream::connect(("127.0.0.1", rport)).unwrap();
    assert_eq!(req(&mut replica, &[b"GET", b"stale"]), b"$-1\r\n");

    replica_process.stop();
    primary_process.stop();
    let restarted = Guard(Some(spawn_persistent_replica()));
    wait_for_port(rport, Duration::from_secs(5));
    let mut replica = TcpStream::connect(("127.0.0.1", rport)).unwrap();
    assert_eq!(req(&mut replica, &[b"GET", b"stale"]), b"$-1\r\n");
    drop(restarted);
    let _ = std::fs::remove_file(&aof);
    let _ = std::fs::remove_file(&rdb);
}
