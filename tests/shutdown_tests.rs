// Graceful shutdown integration tests.
//
// These spawn the actual `redistill` binary and send real POSIX signals to it,
// so they only run on Unix. Windows doesn't have SIGTERM; the shutdown path
// there is exercised via `ctrl_c` which cannot be raised portably from a test.

#![cfg(unix)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_redistill");

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

fn wait_for_port(port: u16, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("server on port {} never came up", port);
}

fn send_sigterm(child: &Child) {
    // Avoid pulling in `libc` as a dev-dep just for this. The `kill(1)` CLI
    // is POSIX-standard and lets us target by PID.
    let status = Command::new("kill")
        .arg("-TERM")
        .arg(child.id().to_string())
        .status()
        .expect("spawn kill(1)");
    assert!(status.success(), "kill -TERM failed");
}

struct ServerGuard(Option<Child>);

impl Drop for ServerGuard {
    fn drop(&mut self) {
        if let Some(mut c) = self.0.take() {
            // Best-effort: ensure the child is not left running if a test fails.
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn tmp_path(stem: &str) -> String {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    dir.join(format!("redistill-{}-{}-{}.rdb", stem, pid, nanos))
        .to_string_lossy()
        .into_owned()
}

#[test]
fn test_sigterm_triggers_graceful_shutdown() {
    let port = free_port();
    let snapshot = tmp_path("shutdown");
    // Make sure no stale file exists.
    let _ = std::fs::remove_file(&snapshot);

    let child = Command::new(BIN)
        .env("REDIS_PORT", port.to_string())
        .env("REDIS_BIND", "127.0.0.1")
        .env("REDIS_HEALTH_CHECK_PORT", "0")
        .env("REDIS_PERSISTENCE_ENABLED", "true")
        .env("REDIS_SNAPSHOT_PATH", &snapshot)
        .env("REDIS_SNAPSHOT_INTERVAL", "0") // no periodic saves during test
        .env("REDIS_SAVE_ON_SHUTDOWN", "true")
        .env("REDIS_SHUTDOWN_GRACE_SECS", "10")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn redistill");
    let mut guard = ServerGuard(Some(child));

    wait_for_port(port, Duration::from_secs(5));

    // Do one SET to seed the store, then disconnect cleanly.
    {
        let mut conn = TcpStream::connect(("127.0.0.1", port)).expect("tcp connect");
        conn.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        conn.write_all(b"*3\r\n$3\r\nSET\r\n$3\r\nfoo\r\n$3\r\nbar\r\n")
            .unwrap();
        let mut buf = [0u8; 16];
        let n = conn.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"+OK\r\n");
    }

    // Signal shutdown.
    let child = guard.0.as_mut().unwrap();
    send_sigterm(child);

    // Must exit well within the 10s grace window for an idle server.
    let exit_deadline = Instant::now() + Duration::from_secs(8);
    let status = loop {
        match child.try_wait().expect("try_wait") {
            Some(s) => break s,
            None => {
                if Instant::now() >= exit_deadline {
                    panic!("server did not exit within grace window");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    assert!(status.success(), "server exited non-zero: {:?}", status);
    guard.0 = None; // drop Guard no-op

    // Snapshot file must exist and be non-empty.
    let meta = std::fs::metadata(&snapshot).expect("snapshot file missing");
    assert!(meta.len() > 0, "snapshot file is empty");
    let _ = std::fs::remove_file(&snapshot);
}

#[test]
fn test_shutdown_drains_idle_connection_promptly() {
    // Grace period is intentionally long (20s); we assert the server exits
    // much faster, which proves the connection-drain signal (watch::changed)
    // actually wakes up an idle handler instead of waiting the full grace.
    let port = free_port();
    let snapshot = tmp_path("drain");
    let _ = std::fs::remove_file(&snapshot);

    let child = Command::new(BIN)
        .env("REDIS_PORT", port.to_string())
        .env("REDIS_BIND", "127.0.0.1")
        .env("REDIS_HEALTH_CHECK_PORT", "0")
        .env("REDIS_PERSISTENCE_ENABLED", "false")
        .env("REDIS_SNAPSHOT_PATH", &snapshot)
        .env("REDIS_SHUTDOWN_GRACE_SECS", "20")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn redistill");
    let mut guard = ServerGuard(Some(child));

    wait_for_port(port, Duration::from_secs(5));

    // Open a connection and keep it idle (no commands sent).
    let _idle_conn = TcpStream::connect(("127.0.0.1", port)).expect("idle conn");
    // Small settle delay to ensure the server registered the connection.
    std::thread::sleep(Duration::from_millis(100));

    let child = guard.0.as_mut().unwrap();
    let sent_at = Instant::now();
    send_sigterm(child);

    // Expect quick exit — well under the 20s grace.
    let exit_deadline = sent_at + Duration::from_secs(5);
    let status = loop {
        match child.try_wait().expect("try_wait") {
            Some(s) => break s,
            None => {
                if Instant::now() >= exit_deadline {
                    panic!(
                        "server did not drain idle connection within 5s (grace was 20s) — \
                         shutdown_rx.changed() is not waking the connection handler"
                    );
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    assert!(status.success(), "server exited non-zero: {:?}", status);
    guard.0 = None;
    let _ = std::fs::remove_file(&snapshot);
}
