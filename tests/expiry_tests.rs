// Regression test for the mass-expiry deadlock: the active-expiration sweep
// must not hang the server when many keys expire at once. (The bug: a DashMap
// iterator temporary held a shard read lock across `remove_if`, deadlocking the
// shard the moment an expired key was actually removed.)

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

#[test]
fn server_stays_responsive_during_mass_expiry() {
    let port = free_port();
    let mut cmd = Command::new(BIN);
    cmd.env("REDIS_PORT", port.to_string())
        .env("REDIS_BIND", "127.0.0.1")
        .env("REDIS_HEALTH_CHECK_PORT", "0")
        .env("REDIS_PERSISTENCE_ENABLED", "false")
        .env("REDIS_AOF_ENABLED", "false")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _g = Guard(Some(cmd.spawn().expect("spawn")));
    wait_for_port(port, Duration::from_secs(5));

    let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
    // 200 keys all expiring in ~500ms, over one connection.
    for i in 0..200 {
        let key = format!("ttl:{i}");
        let reply = req(&mut c, &[b"SET", key.as_bytes(), b"v", b"PX", b"500"]);
        assert_eq!(reply, b"+OK\r\n");
    }

    // Let them expire and the active sweep run. Before the fix, the server
    // deadlocked here and the next command timed out.
    std::thread::sleep(Duration::from_millis(1500));

    // The server must still respond — prove it's not deadlocked.
    let pong = req(&mut c, &[b"PING"]);
    assert_eq!(pong, b"+PONG\r\n", "server hung during mass expiry");

    // And a logically-expired key reads as nil.
    let g = req(&mut c, &[b"GET", b"ttl:0"]);
    assert_eq!(g, b"$-1\r\n");
}
