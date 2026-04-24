// AOF rewrite/compaction integration tests.
//
// Verifies that BGREWRITEAOF (a) shrinks a pathologically redundant log,
// (b) preserves state across a restart against the rewritten file, and
// (c) reports a sensible error when AOF is disabled.

#![cfg(unix)]

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

fn send_and_read_reply(conn: &mut TcpStream, req: &[u8]) -> Vec<u8> {
    conn.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    conn.write_all(req).unwrap();
    let mut buf = vec![0u8; 256];
    let n = conn.read(&mut buf).unwrap();
    buf.truncate(n);
    buf
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

fn spawn_with_aof(port: u16, aof_path: &str) -> Child {
    Command::new(BIN)
        .env("REDIS_PORT", port.to_string())
        .env("REDIS_BIND", "127.0.0.1")
        .env("REDIS_HEALTH_CHECK_PORT", "0")
        .env("REDIS_PERSISTENCE_ENABLED", "false")
        .env("REDIS_AOF_ENABLED", "true")
        .env("REDIS_AOF_PATH", aof_path)
        .env("REDIS_AOF_FSYNC", "everysec")
        // Disable auto-rewrite — we drive it manually via BGREWRITEAOF.
        .env("REDIS_AOF_REWRITE_PERCENTAGE", "0")
        .env("REDIS_SHUTDOWN_GRACE_SECS", "5")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn")
}

fn graceful_shutdown(child: &mut Child) {
    let _ = Command::new("kill")
        .arg("-TERM")
        .arg(child.id().to_string())
        .status();
    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.wait();
}

fn tmp_aof(stem: &str) -> String {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    dir.join(format!(
        "redistill-aof-rewrite-{}-{}-{}.aof",
        stem, pid, nanos
    ))
    .to_string_lossy()
    .into_owned()
}

#[test]
fn test_bgrewriteaof_shrinks_log_and_preserves_state() {
    let port = free_port();
    let aof = tmp_aof("shrink");
    let _ = std::fs::remove_file(&aof);

    let mut guard = Guard(Some(spawn_with_aof(port, &aof)));
    wait_for_port(port, Duration::from_secs(5));

    // Write the same keys many times — the log accumulates N entries while
    // live state is only ever K distinct keys.
    {
        let mut conn = TcpStream::connect(("127.0.0.1", port)).unwrap();
        for i in 0..200 {
            // Overwrite 10 keys 200 times each.
            for k in 0..10 {
                let key = format!("key{}", k);
                let value = format!("value-{}-{}", k, i);
                let req = format!(
                    "*3\r\n$3\r\nSET\r\n${}\r\n{}\r\n${}\r\n{}\r\n",
                    key.len(),
                    key,
                    value.len(),
                    value
                );
                let r = send_and_read_reply(&mut conn, req.as_bytes());
                assert_eq!(&r, b"+OK\r\n");
            }
        }
    }

    // Size before rewrite. Need to give everysec fsync a moment to land
    // buffered bytes on disk so the file-size check is stable.
    std::thread::sleep(Duration::from_millis(1200));
    let size_before = std::fs::metadata(&aof).unwrap().len();
    assert!(
        size_before > 0,
        "AOF file should have content before rewrite"
    );

    // Kick off rewrite.
    {
        let mut conn = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let r = send_and_read_reply(&mut conn, b"*1\r\n$12\r\nBGREWRITEAOF\r\n");
        assert!(
            r.starts_with(b"+Background append only file rewriting started"),
            "unexpected BGREWRITEAOF reply: {:?}",
            String::from_utf8_lossy(&r)
        );
    }

    // Poll until the rewrite lands on disk. A race here is inherent (the
    // reply is sent before spawn_blocking completes); cap at 10s.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut size_after = size_before;
    while Instant::now() < deadline {
        let s = std::fs::metadata(&aof).unwrap().len();
        if s < size_before / 4 {
            size_after = s;
            break;
        }
        size_after = s;
        std::thread::sleep(Duration::from_millis(100));
    }

    assert!(
        size_after * 4 < size_before,
        "rewrite did not shrink log meaningfully: {} -> {}",
        size_before,
        size_after
    );

    // Graceful shutdown to force the final fsync.
    let child = guard.0.as_mut().unwrap();
    graceful_shutdown(child);
    guard.0 = None;

    // Restart against the compacted AOF and verify final state survives.
    let port2 = free_port();
    let guard2 = Guard(Some(spawn_with_aof(port2, &aof)));
    wait_for_port(port2, Duration::from_secs(5));

    let mut conn = TcpStream::connect(("127.0.0.1", port2)).unwrap();
    for k in 0..10 {
        let key = format!("key{}", k);
        let req = format!("*2\r\n$3\r\nGET\r\n${}\r\n{}\r\n", key.len(), key);
        let r = send_and_read_reply(&mut conn, req.as_bytes());
        let expected_val = format!("value-{}-199", k);
        let expected_reply = format!("${}\r\n{}\r\n", expected_val.len(), expected_val);
        assert_eq!(
            &r,
            expected_reply.as_bytes(),
            "post-rewrite value for {} wrong: {:?}",
            key,
            String::from_utf8_lossy(&r)
        );
    }

    drop(guard2);
    let _ = std::fs::remove_file(&aof);
}

#[test]
fn test_bgrewriteaof_errors_when_aof_disabled() {
    let port = free_port();
    // Note: no AOF enabled.
    let child = Command::new(BIN)
        .env("REDIS_PORT", port.to_string())
        .env("REDIS_BIND", "127.0.0.1")
        .env("REDIS_HEALTH_CHECK_PORT", "0")
        .env("REDIS_PERSISTENCE_ENABLED", "false")
        .env("REDIS_AOF_ENABLED", "false")
        .env("REDIS_SHUTDOWN_GRACE_SECS", "5")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    let guard = Guard(Some(child));
    wait_for_port(port, Duration::from_secs(5));

    let mut conn = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let r = send_and_read_reply(&mut conn, b"*1\r\n$12\r\nBGREWRITEAOF\r\n");
    // Error replies start with `-`. The exact text includes "AOF is disabled".
    assert!(
        r.starts_with(b"-"),
        "expected error reply, got: {:?}",
        String::from_utf8_lossy(&r)
    );
    assert!(
        std::str::from_utf8(&r).unwrap().contains("AOF is disabled"),
        "unexpected error message: {:?}",
        String::from_utf8_lossy(&r)
    );

    drop(guard);
}
