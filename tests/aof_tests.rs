// AOF (Append-Only File) persistence integration tests.
//
// Unix-only because we exercise crash durability with SIGKILL, which has no
// portable equivalent on Windows.

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

/// Send raw RESP bytes, read one reply, return the bytes up to and including
/// the first `\r\n` (sufficient for simple string / integer replies used here).
fn send_and_read_reply(conn: &mut TcpStream, req: &[u8]) -> Vec<u8> {
    conn.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    conn.write_all(req).unwrap();
    let mut buf = vec![0u8; 128];
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

fn spawn_server(port: u16, aof_path: &str, fsync: &str) -> Child {
    Command::new(BIN)
        .env("REDIS_PORT", port.to_string())
        .env("REDIS_BIND", "127.0.0.1")
        .env("REDIS_HEALTH_CHECK_PORT", "0")
        .env("REDIS_PERSISTENCE_ENABLED", "false")
        .env("REDIS_AOF_ENABLED", "true")
        .env("REDIS_AOF_PATH", aof_path)
        .env("REDIS_AOF_FSYNC", fsync)
        .env("REDIS_SHUTDOWN_GRACE_SECS", "5")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn redistill")
}

fn tmp_aof(stem: &str) -> String {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    dir.join(format!("redistill-aof-{}-{}-{}.aof", stem, pid, nanos))
        .to_string_lossy()
        .into_owned()
}

#[test]
fn test_aof_always_survives_sigkill() {
    // `always` fsyncs on every write → a SIGKILL immediately after OK must
    // leave the value durable.
    let port = free_port();
    let aof = tmp_aof("always");
    let _ = std::fs::remove_file(&aof);

    let mut guard = Guard(Some(spawn_server(port, &aof, "always")));
    wait_for_port(port, Duration::from_secs(5));

    {
        let mut conn = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let r = send_and_read_reply(
            &mut conn,
            b"*3\r\n$3\r\nSET\r\n$5\r\ncrash\r\n$5\r\nvalue\r\n",
        );
        assert_eq!(&r, b"+OK\r\n", "SET did not return +OK: {:?}", r);
    }

    // SIGKILL — no graceful shutdown, no final sync. `always` must have
    // already fsynced on the SET above.
    let child = guard.0.as_mut().unwrap();
    let _ = child.kill();
    let _ = child.wait();
    guard.0 = None;

    // Restart the server pointing at the same AOF.
    let port2 = free_port();
    let guard2 = Guard(Some(spawn_server(port2, &aof, "always")));
    wait_for_port(port2, Duration::from_secs(5));

    let mut conn = TcpStream::connect(("127.0.0.1", port2)).unwrap();
    let r = send_and_read_reply(&mut conn, b"*2\r\n$3\r\nGET\r\n$5\r\ncrash\r\n");
    assert_eq!(
        &r, b"$5\r\nvalue\r\n",
        "value not replayed from AOF after kill -9: {:?}",
        r
    );

    drop(guard2);
    let _ = std::fs::remove_file(&aof);
}

#[test]
fn test_aof_everysec_survives_graceful_shutdown() {
    // `everysec` only guarantees durability via the background timer or final
    // sync. A graceful SIGTERM path runs the final sync, so the data must be
    // recoverable.
    let port = free_port();
    let aof = tmp_aof("everysec");
    let _ = std::fs::remove_file(&aof);

    let child = spawn_server(port, &aof, "everysec");
    let mut guard = Guard(Some(child));
    wait_for_port(port, Duration::from_secs(5));

    {
        let mut conn = TcpStream::connect(("127.0.0.1", port)).unwrap();
        // Write several kinds of commands that must replay correctly.
        let r = send_and_read_reply(&mut conn, b"*3\r\n$3\r\nSET\r\n$3\r\nfoo\r\n$3\r\nbar\r\n");
        assert_eq!(&r, b"+OK\r\n");
        let r = send_and_read_reply(&mut conn, b"*2\r\n$4\r\nINCR\r\n$3\r\ncnt\r\n");
        assert_eq!(&r, b":1\r\n");
        let r = send_and_read_reply(&mut conn, b"*2\r\n$4\r\nINCR\r\n$3\r\ncnt\r\n");
        assert_eq!(&r, b":2\r\n");
        let r = send_and_read_reply(
            &mut conn,
            b"*4\r\n$4\r\nHSET\r\n$1\r\nh\r\n$1\r\nf\r\n$1\r\nv\r\n",
        );
        assert_eq!(&r, b":1\r\n");
    }

    // Graceful shutdown.
    let pid = guard.0.as_ref().unwrap().id();
    let _ = Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status();
    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline {
        if let Some(_) = guard.0.as_mut().unwrap().try_wait().unwrap() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = guard.0.as_mut().unwrap().wait();
    guard.0 = None;

    // Restart.
    let port2 = free_port();
    let guard2 = Guard(Some(spawn_server(port2, &aof, "everysec")));
    wait_for_port(port2, Duration::from_secs(5));

    let mut conn = TcpStream::connect(("127.0.0.1", port2)).unwrap();
    let r = send_and_read_reply(&mut conn, b"*2\r\n$3\r\nGET\r\n$3\r\nfoo\r\n");
    assert_eq!(&r, b"$3\r\nbar\r\n", "SET did not replay: {:?}", r);
    let r = send_and_read_reply(&mut conn, b"*2\r\n$3\r\nGET\r\n$3\r\ncnt\r\n");
    assert_eq!(&r, b"$1\r\n2\r\n", "INCR state did not replay: {:?}", r);
    let r = send_and_read_reply(&mut conn, b"*3\r\n$4\r\nHGET\r\n$1\r\nh\r\n$1\r\nf\r\n");
    assert_eq!(&r, b"$1\r\nv\r\n", "HSET did not replay: {:?}", r);

    drop(guard2);
    let _ = std::fs::remove_file(&aof);
}

#[test]
fn test_aof_replay_refuses_corrupt_file() {
    // Write a half-written AOF and confirm the server refuses to start.
    let port = free_port();
    let aof = tmp_aof("corrupt");
    // `*3\r\n$3\r\nSET\r\n` — a truncated command (header says 3 parts, only
    // one provided).
    std::fs::write(&aof, b"*3\r\n$3\r\nSET\r\n").unwrap();

    let mut child = Command::new(BIN)
        .env("REDIS_PORT", port.to_string())
        .env("REDIS_BIND", "127.0.0.1")
        .env("REDIS_HEALTH_CHECK_PORT", "0")
        .env("REDIS_PERSISTENCE_ENABLED", "false")
        .env("REDIS_AOF_ENABLED", "true")
        .env("REDIS_AOF_PATH", &aof)
        .env("REDIS_AOF_FSYNC", "no")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");

    // Expect the process to exit with non-zero within a couple of seconds.
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        match child.try_wait().unwrap() {
            Some(s) => break s,
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("server should have refused to start on a corrupt AOF");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    assert!(
        !status.success(),
        "server should have exited non-zero on corrupt AOF, got {:?}",
        status
    );

    let _ = std::fs::remove_file(&aof);
}
