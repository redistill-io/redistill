# Replication Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add v1 asynchronous primary→replica replication to Redistill: a primary streams a consistent snapshot plus a live RESP write-command stream to one or more read-only replicas, with a manual `REPLICAOF NO ONE` promotion primitive.

**Architecture:** Writes already funnel through `aof_log(command)` after every successful mutation. We fold an `on_write(command)` helper at those same sites that both logs to AOF (existing) and feeds a `tokio::sync::broadcast` channel (new). A global `Replication` value (mirroring the `AOF` `OnceCell`) holds the role, a `parking_lot::RwLock<()>` write-barrier, an `AtomicU64` replication offset, the broadcast sender, and a random `replid`. The full-sync "consistency cut" takes the barrier exclusively so the bincode store snapshot is quiescent and aligns to a single offset; the new replica subscribes to the broadcast under that same exclusive section, guaranteeing exactly-once delivery. Replicas apply received frames through the existing synchronous `execute_command` replay path. Read-only enforcement and the consistency barrier are single additions at the top of `execute_command`.

**Tech Stack:** Rust 2024, Tokio (`broadcast`, `net`, `io-util`), `parking_lot::RwLock`/`Mutex`, `bincode` 1.3, `bytes::Bytes`, `fastrand`, `serde`. Integration tests spawn the real binary (`env!("CARGO_BIN_EXE_redistill")`) configured via env vars and drive it over raw TCP RESP.

---

## File Structure

| File | Responsibility |
|------|----------------|
| `src/replication.rs` (**create**) | `Replication` struct (role, offset, barrier, broadcast sender, replid); command serialization; primary-side full-sync + per-replica fan-out task; replica-side client loop (connect, handshake, load snapshot, apply stream, reconnect). |
| `src/config.rs` (**modify**) | `ReplicationConfig` sub-struct + `Default` + env overrides + wire into `Config`. |
| `src/persistence.rs` (**modify**) | Refactor save/load to share a `Write`/`Read`-based core; expose `dump_store_to_bytes` / `load_store_from_bytes`. |
| `src/main.rs` (**modify**) | Init global `REPLICATION`; `on_write` tap; read-guard + read-only gate at top of `execute_command`; `REPLICAOF`/`REPLCONF`/`PSYNC` dispatch; PSYNC hand-off in `handle_connection`; INFO replication section; spawn replica client task at startup if configured. |
| `src/metrics.rs` (**modify**) | Replication gauges synced in `encode`. |
| `src/server.rs` (**modify**) | Add `from_master: bool` + `replconf_port: Option<u16>` to `ConnectionState`. |
| `src/lib.rs` (**modify**) | Mirror `ConnectionState` fields in the test stub; `pub mod replication;` re-export. |
| `tests/replication_tests.rs` (**create**) | Two-node integration tests. |

---

## Phase 1 — Persistence: in-memory snapshot dump/load

### Task 1: Refactor snapshot save to a `Write`-generic core and expose `dump_store_to_bytes`

**Files:**
- Modify: `src/persistence.rs`
- Test: `tests/replication_tests.rs` (create) — unit-style roundtrip test compiled as an integration test using the library crate.

- [ ] **Step 1: Write the failing test**

Create `tests/replication_tests.rs`:

```rust
// tests/replication_tests.rs
use redistill::persistence::{dump_store_to_bytes, load_store_from_bytes};
use redistill::store::ShardedStore;
use bytes::Bytes;

fn now() -> u64 {
    redistill::store::get_timestamp()
}

#[test]
fn dump_and_load_roundtrip_strings_and_hashes() {
    let src = ShardedStore::new(16);
    let t = now();
    src.set(Bytes::from_static(b"k1"), Bytes::from_static(b"v1"), None, t);
    src.set(Bytes::from_static(b"k2"), Bytes::from_static(b"v2"), Some(t + 100), t);
    src.hset(b"h1", &[Bytes::from_static(b"f1"), Bytes::from_static(b"hv1")], t)
        .unwrap();

    let bytes = dump_store_to_bytes(&src).expect("dump");

    let dst = ShardedStore::new(16);
    let count = load_store_from_bytes(&dst, &bytes).expect("load");

    assert_eq!(count, 3);
    assert_eq!(dst.get(b"k1", now()), Some(Bytes::from_static(b"v1")));
    assert_eq!(dst.get(b"k2", now()), Some(Bytes::from_static(b"v2")));
    let hv = dst.hget(b"h1", b"f1", now()).unwrap();
    assert_eq!(hv, Some(Bytes::from_static(b"hv1")));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test replication_tests dump_and_load_roundtrip -- --nocapture`
Expected: FAIL — compile error, `dump_store_to_bytes` / `load_store_from_bytes` not found.

- [ ] **Step 3: Refactor `save_snapshot_sync` onto a `Write` core and add `dump_store_to_bytes`**

In `src/persistence.rs`, extract the body of `save_snapshot_sync` (which currently writes to a `BufWriter<File>`) into a generic helper, then add an in-memory entry point. Keep the existing `save_snapshot_sync` working by delegating to the core. The core must write the same framing the file format uses: magic `b"RDST"`, version `u8`, timestamp `u64` LE, entry-count `u64` LE, then for each entry a `u64` LE length prefix followed by the `bincode::serialize(&SnapshotEntry)` bytes.

```rust
use std::io::{Cursor, Write};

/// Serialize the whole store into the RDST snapshot byte format, in memory.
/// Same framing as `save_snapshot_sync` writes to disk.
pub fn dump_store_to_bytes(store: &ShardedStore) -> Result<Vec<u8>, String> {
    let mut buf = Cursor::new(Vec::with_capacity(64 * 1024));
    let _count = write_snapshot(&mut buf, store)?;
    Ok(buf.into_inner())
}

/// Core writer shared by the file path and the in-memory path.
/// Returns the number of entries written.
fn write_snapshot<W: Write>(w: &mut W, store: &ShardedStore) -> Result<usize, String> {
    let now = crate::store::get_timestamp();
    // Header
    w.write_all(b"RDST").map_err(|e| e.to_string())?;
    w.write_all(&[1u8]).map_err(|e| e.to_string())?; // version
    w.write_all(&now.to_le_bytes()).map_err(|e| e.to_string())?;

    // Collect entries first so we know the count up front (in-memory path
    // can't seek-back like the file path does).
    let keys = store.keys(now);
    w.write_all(&(keys.len() as u64).to_le_bytes())
        .map_err(|e| e.to_string())?;

    let mut written = 0usize;
    for key in &keys {
        let entry = match build_snapshot_entry(store, key, now) {
            Some(e) => e,
            None => continue, // expired between keys() and read
        };
        let encoded =
            bincode::serialize(&entry).map_err(|e| format!("serialize: {}", e))?;
        w.write_all(&(encoded.len() as u64).to_le_bytes())
            .map_err(|e| e.to_string())?;
        w.write_all(&encoded).map_err(|e| e.to_string())?;
        written += 1;
    }
    Ok(written)
}
```

Add a private `build_snapshot_entry(store, key, now) -> Option<SnapshotEntry>` that reads the current value for `key` and returns the `SnapshotEntry { key, value: SnapshotValue::String|Hash, expiry }` already used by the existing save logic. Reuse the exact `SnapshotEntry` / `SnapshotValue` types defined in this file. If `save_snapshot_sync` already contains this per-key logic inline, lift it verbatim into `build_snapshot_entry` and have both `save_snapshot_sync` and `write_snapshot` call it.

> Note: the count is now written exactly once up front (no seek-back), so `dump_store_to_bytes` works on a non-seekable buffer. Update `save_snapshot_sync` to call `write_snapshot` too, removing its seek-back logic.

- [ ] **Step 4: Add `load_store_from_bytes`**

```rust
use std::io::Read;

/// Load a store from RDST snapshot bytes (the in-memory counterpart of
/// `load_snapshot`). Clears nothing; caller clears the store first.
pub fn load_store_from_bytes(store: &ShardedStore, data: &[u8]) -> Result<usize, String> {
    let mut cur = Cursor::new(data);
    read_snapshot(&mut cur, store)
}

/// Core reader shared by the file path and the in-memory path.
fn read_snapshot<R: Read>(r: &mut R, store: &ShardedStore) -> Result<usize, String> {
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic).map_err(|e| e.to_string())?;
    if &magic != b"RDST" {
        return Err("bad snapshot magic".to_string());
    }
    let mut ver = [0u8; 1];
    r.read_exact(&mut ver).map_err(|e| e.to_string())?;
    let mut ts = [0u8; 8];
    r.read_exact(&mut ts).map_err(|e| e.to_string())?;
    let mut cnt = [0u8; 8];
    r.read_exact(&mut cnt).map_err(|e| e.to_string())?;
    let count = u64::from_le_bytes(cnt);

    let now = crate::store::get_timestamp();
    let mut loaded = 0usize;
    for _ in 0..count {
        let mut lenb = [0u8; 8];
        r.read_exact(&mut lenb).map_err(|e| e.to_string())?;
        let len = u64::from_le_bytes(lenb) as usize;
        let mut entry_data = vec![0u8; len];
        r.read_exact(&mut entry_data).map_err(|e| e.to_string())?;
        apply_snapshot_entry(store, &entry_data, now)?;
        loaded += 1;
    }
    Ok(loaded)
}
```

Add a private `apply_snapshot_entry(store, &[u8], now)` that performs the `bincode::deserialize::<SnapshotEntry>` + insert logic already present in `load_snapshot`. Lift that per-entry body out of `load_snapshot` into this helper and have `load_snapshot` call `read_snapshot` over its `BufReader<File>`.

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test --test replication_tests dump_and_load_roundtrip -- --nocapture`
Expected: PASS.

- [ ] **Step 6: Verify nothing else broke**

Run: `cargo test --test aof_tests && cargo build`
Expected: PASS / clean build (the file save/load still works through the refactored core).

- [ ] **Step 7: Commit**

```bash
git add src/persistence.rs tests/replication_tests.rs
git commit -m "refactor(persistence): share snapshot core; add in-memory dump/load"
```

---

## Phase 2 — Config surface

### Task 2: Add `ReplicationConfig`

**Files:**
- Modify: `src/config.rs`
- Test: `tests/replication_tests.rs`

- [ ] **Step 1: Write the failing test**

Append to `tests/replication_tests.rs`:

```rust
#[test]
fn replication_config_defaults() {
    let c = redistill::config::ReplicationConfig::default();
    assert_eq!(c.replicaof, "");
    assert_eq!(c.masterauth, "");
    assert!(c.replica_read_only);
    assert_eq!(c.repl_backlog_size, 1_048_576);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test replication_tests replication_config_defaults`
Expected: FAIL — `ReplicationConfig` not found.

- [ ] **Step 3: Add the sub-struct, defaults, and wire into `Config`**

In `src/config.rs`, following the existing `ServerConfig` pattern:

```rust
fn default_replicaof() -> String { String::new() }
fn default_masterauth() -> String { String::new() }
fn default_replica_read_only() -> bool { true }
fn default_repl_backlog_size() -> usize { 1_048_576 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplicationConfig {
    /// "host:port" of the primary to replicate from. Empty = act as primary.
    #[serde(default = "default_replicaof")]
    pub replicaof: String,
    /// Password used when authenticating to the primary.
    #[serde(default = "default_masterauth")]
    pub masterauth: String,
    /// Reject client writes while acting as a replica.
    #[serde(default = "default_replica_read_only")]
    pub replica_read_only: bool,
    /// Broadcast lag-buffer capacity (messages) before a slow replica is dropped.
    #[serde(default = "default_repl_backlog_size")]
    pub repl_backlog_size: usize,
}

impl Default for ReplicationConfig {
    fn default() -> Self {
        Self {
            replicaof: default_replicaof(),
            masterauth: default_masterauth(),
            replica_read_only: default_replica_read_only(),
            repl_backlog_size: default_repl_backlog_size(),
        }
    }
}
```

Add to the top-level `Config` struct (next to `persistence`):

```rust
    #[serde(default)]
    pub replication: ReplicationConfig,
```

- [ ] **Step 4: Add env overrides**

In the env-override function (where `REDIS_PORT` etc. are handled), add:

```rust
    if let Ok(v) = std::env::var("REDIS_REPLICAOF") {
        config.replication.replicaof = v;
    }
    if let Ok(v) = std::env::var("REDIS_MASTERAUTH") {
        config.replication.masterauth = v;
    }
    if let Ok(v) = std::env::var("REDIS_REPLICA_READ_ONLY")
        && let Ok(b) = v.parse()
    {
        config.replication.replica_read_only = b;
    }
    if let Ok(v) = std::env::var("REDIS_REPL_BACKLOG_SIZE")
        && let Ok(n) = v.parse()
    {
        config.replication.repl_backlog_size = n;
    }
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test --test replication_tests replication_config_defaults`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/config.rs
git commit -m "feat(config): add [replication] section with env overrides"
```

---

## Phase 3 — Replication core module

### Task 3: Create `Replication` with replid, offset, and command serialization

**Files:**
- Create: `src/replication.rs`
- Modify: `src/lib.rs` (add `pub mod replication;`), `src/main.rs` (add `mod replication;`)
- Test: `tests/replication_tests.rs`

- [ ] **Step 1: Write the failing test**

Append to `tests/replication_tests.rs`:

```rust
use redistill::replication::{encode_command, gen_replid, Replication, Role};

#[test]
fn replid_is_40_hex_chars() {
    let id = gen_replid();
    assert_eq!(id.len(), 40);
    assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
}

#[test]
fn encode_command_produces_resp_array() {
    let cmd = vec![Bytes::from_static(b"SET"), Bytes::from_static(b"k"), Bytes::from_static(b"v")];
    let out = encode_command(&cmd);
    assert_eq!(&out[..], b"*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$1\r\nv\r\n");
}

#[test]
fn new_replication_is_primary_with_zero_offset() {
    let r = Replication::new(16);
    assert!(matches!(r.role(), Role::Primary));
    assert_eq!(r.offset(), 0);
    assert_eq!(r.replid().len(), 40);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test replication_tests replid_is_40_hex_chars`
Expected: FAIL — module/types not found.

- [ ] **Step 3: Create `src/replication.rs`**

```rust
//! Asynchronous primary→replica replication.
//!
//! A primary feeds every successful write into a broadcast channel and tracks
//! a monotonic byte offset. Replicas receive a consistent snapshot followed by
//! the live command stream and apply it through the normal command path.

use bytes::Bytes;
use parking_lot::{Mutex, RwLock};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::broadcast;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Primary,
    Replica,
}

/// State describing the primary this node replicates from (replica role only).
#[derive(Debug, Clone)]
pub struct MasterInfo {
    pub host: String,
    pub port: u16,
    /// true once the initial sync completed and the stream is live.
    pub link_up: bool,
}

/// A single frame on the replication stream: (end_offset, resp_bytes).
pub type Frame = (u64, Bytes);

pub struct Replication {
    role: RwLock<Role>,
    master: RwLock<Option<MasterInfo>>,
    replid: RwLock<String>,
    offset: AtomicU64,
    /// Guards (offset increment + broadcast send) so frame offsets are
    /// monotonic and consistent with broadcast order.
    feed_lock: Mutex<()>,
    sender: broadcast::Sender<Frame>,
    /// Held shared by every command; held exclusively during a full-sync cut so
    /// the snapshot is quiescent and aligns with a single offset.
    pub barrier: RwLock<()>,
    /// Count of currently connected replicas (for INFO / metrics).
    replica_count: AtomicU64,
}

impl Replication {
    pub fn new(backlog: usize) -> Self {
        let (sender, _rx) = broadcast::channel(backlog.max(16));
        Self {
            role: RwLock::new(Role::Primary),
            master: RwLock::new(None),
            replid: RwLock::new(gen_replid()),
            offset: AtomicU64::new(0),
            feed_lock: Mutex::new(()),
            sender,
            barrier: RwLock::new(()),
            replica_count: AtomicU64::new(0),
        }
    }

    pub fn role(&self) -> Role {
        *self.role.read()
    }
    pub fn offset(&self) -> u64 {
        self.offset.load(Ordering::Relaxed)
    }
    pub fn replid(&self) -> String {
        self.replid.read().clone()
    }
    pub fn master(&self) -> Option<MasterInfo> {
        self.master.read().clone()
    }
    pub fn replica_count(&self) -> u64 {
        self.replica_count.load(Ordering::Relaxed)
    }
    pub fn incr_replicas(&self) {
        self.replica_count.fetch_add(1, Ordering::Relaxed);
    }
    pub fn decr_replicas(&self) {
        self.replica_count.fetch_sub(1, Ordering::Relaxed);
    }
    pub fn subscribe(&self) -> broadcast::Receiver<Frame> {
        self.sender.subscribe()
    }

    /// Feed a write command to all replicas. No-op (cheap) when there are no
    /// subscribers. Called from the synchronous command path.
    pub fn feed(&self, command: &[Bytes]) {
        let frame = encode_command(command);
        let _g = self.feed_lock.lock();
        let new_off = self.offset.fetch_add(frame.len() as u64, Ordering::Relaxed)
            + frame.len() as u64;
        // Ignore send errors: zero subscribers is the common case.
        let _ = self.sender.send((new_off, frame));
    }

    /// Become a replica of host:port. Sets role + master info; the actual
    /// connection is driven by the replica client task.
    pub fn set_master(&self, host: String, port: u16) {
        *self.role.write() = Role::Replica;
        *self.master.write() = Some(MasterInfo { host, port, link_up: false });
    }

    pub fn set_link_up(&self, up: bool) {
        if let Some(m) = self.master.write().as_mut() {
            m.link_up = up;
        }
    }

    /// Promote to primary (`REPLICAOF NO ONE`). Fresh replid, keep offset.
    pub fn promote(&self) {
        *self.role.write() = Role::Primary;
        *self.master.write() = None;
        *self.replid.write() = gen_replid();
    }
}

/// Generate a 40-char lowercase hex run id.
pub fn gen_replid() -> String {
    let mut s = String::with_capacity(40);
    for _ in 0..40 {
        let nibble = fastrand::u8(0..16);
        s.push(char::from_digit(nibble as u32, 16).unwrap());
    }
    s
}

/// Serialize a command as a RESP array frame (same wire shape as the AOF).
pub fn encode_command(command: &[Bytes]) -> Bytes {
    let mut buf = Vec::with_capacity(16 + command.iter().map(|p| p.len() + 16).sum::<usize>());
    buf.extend_from_slice(format!("*{}\r\n", command.len()).as_bytes());
    for part in command {
        buf.extend_from_slice(format!("${}\r\n", part.len()).as_bytes());
        buf.extend_from_slice(part);
        buf.extend_from_slice(b"\r\n");
    }
    Bytes::from(buf)
}
```

In `src/lib.rs` add `pub mod replication;`. In `src/main.rs` add `mod replication;` near the other `mod` declarations (or `use redistill::replication;` if main uses the lib crate — match the existing module style in main.rs).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test replication_tests replid_is_40_hex_chars encode_command_produces_resp_array new_replication_is_primary_with_zero_offset`
Expected: PASS (all three).

- [ ] **Step 5: Commit**

```bash
git add src/replication.rs src/lib.rs src/main.rs
git commit -m "feat(replication): core Replication struct, replid, command encode"
```

---

## Phase 4 — Wire the write tap, barrier, and global init

### Task 4: Add global `REPLICATION`, `on_write` tap, and command-scope barrier guard

**Files:**
- Modify: `src/main.rs`
- Test: covered by Task 8 integration tests (offset advance is asserted via `INFO replication`). No isolated unit test — this wires globals into the running server.

- [ ] **Step 1: Declare the global and initialize it at startup**

Near the `AOF` static in `src/main.rs`:

```rust
static REPLICATION: once_cell::sync::OnceCell<Arc<replication::Replication>> =
    once_cell::sync::OnceCell::new();

#[inline(always)]
fn repl() -> &'static Arc<replication::Replication> {
    REPLICATION.get().expect("REPLICATION initialized at startup")
}
```

In `main()` startup (before the accept loop, near where AOF is set up), unconditionally initialize it:

```rust
let _ = REPLICATION.set(Arc::new(replication::Replication::new(
    CONFIG.replication.repl_backlog_size,
)));
```

- [ ] **Step 2: Fold the write tap into `on_write`**

Add:

```rust
/// Called after every successful write command: append to AOF (if enabled)
/// and feed the replication stream (if any replicas).
#[inline(always)]
fn on_write(command: &[Bytes]) {
    aof_log(command);
    if let Some(r) = REPLICATION.get() {
        r.feed(command);
    }
}
```

Replace every `aof_log(command);` call site in `execute_command` with `on_write(command);`. (Sites include SET, DEL, INCR/DECR, MSET, HSET, HDEL, EXPIRE, PERSIST, FLUSHDB, FLUSHALL — every place that currently calls `aof_log`.) Then make `aof_log` private/unchanged; it is now only called via `on_write`.

- [ ] **Step 3: Add the command-scope barrier read-guard at the top of `execute_command`**

Immediately after the `if command.is_empty()` check in `execute_command`, before dispatch:

```rust
    // Hold the replication barrier shared for the whole command so a full-sync
    // cut (which takes it exclusively) sees a quiescent store. Cheap, concurrent
    // read-lock; only contended briefly during a replica's initial sync.
    let _repl_guard = REPLICATION.get().map(|r| r.barrier.read());
```

Because `execute_command` returns from many branches, binding the guard at function scope drops it correctly on every path.

- [ ] **Step 4: Build and smoke-test**

Run: `cargo build && cargo test --test aof_tests`
Expected: clean build; AOF tests still pass (behavior unchanged when no replicas connected — `feed` sends to zero subscribers).

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat(replication): global state, on_write tap, command barrier"
```

---

## Phase 5 — Primary side: REPLCONF / PSYNC / fan-out

### Task 5: Handle `REPLCONF`, add `from_master`/`replconf_port` to ConnectionState

**Files:**
- Modify: `src/server.rs`, `src/lib.rs`, `src/main.rs`

- [ ] **Step 1: Extend `ConnectionState`**

In `src/server.rs`:

```rust
pub struct ConnectionState {
    pub authenticated: bool,
    /// True only for the internal replica-apply path; bypasses the read-only gate.
    pub from_master: bool,
    /// Replica's advertised listening port (from REPLCONF listening-port).
    pub replconf_port: Option<u16>,
}

impl ConnectionState {
    pub fn new() -> Self {
        Self {
            authenticated: CONFIG.security.password.is_empty(),
            from_master: false,
            replconf_port: None,
        }
    }
}
```

Mirror the new fields in the `src/lib.rs` test stub `ConnectionState` (set `from_master: true` is NOT desired there; keep `from_master: false`, `replconf_port: None`, and the stub's `authenticated: true`).

- [ ] **Step 2: Dispatch `REPLCONF` (reply +OK, record port)**

In `execute_command`, add a branch for the 8-byte command `REPLCONF` (case-insensitive). It accepts `listening-port <port>` and any other sub-args we ignore:

```rust
    // REPLCONF <option> <value> ...  -> +OK
    if cmd.len() == 8 && cmd.eq_ignore_ascii_case(b"REPLCONF") {
        if command.len() >= 3 && command[1].eq_ignore_ascii_case(b"listening-port") {
            if let Ok(s) = std::str::from_utf8(&command[2]) {
                state.replconf_port = s.parse().ok();
            }
        }
        writer.write_simple_string(b"OK");
        return;
    }
```

Place it alongside the other command-length branches (use the generic `cmd.eq_ignore_ascii_case` style; confirm `Bytes` derefs to `[u8]` so `eq_ignore_ascii_case` is available — it is via `<[u8]>::eq_ignore_ascii_case`).

- [ ] **Step 3: Build**

Run: `cargo build`
Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add src/server.rs src/lib.rs src/main.rs
git commit -m "feat(replication): REPLCONF handling and ConnectionState fields"
```

---

### Task 6: PSYNC hand-off in `handle_connection` + full-sync + fan-out task

**Files:**
- Modify: `src/main.rs`
- Create logic in: `src/replication.rs` (`serve_replica`)

- [ ] **Step 1: Add `serve_replica` to `src/replication.rs`**

This takes over a connection after the client sends `PSYNC`. It performs the consistency cut, sends `+FULLRESYNC <replid> <offset>`, the length-prefixed snapshot, then streams frames until the socket dies.

```rust
use crate::persistence::dump_store_to_bytes;
use crate::store::ShardedStore;
use tokio::io::{AsyncWrite, AsyncWriteExt};

/// Serve a replica over `stream` after it issued PSYNC. Runs until the replica
/// disconnects or a write error occurs.
pub async fn serve_replica<S>(
    repl: &Replication,
    store: &ShardedStore,
    stream: &mut S,
) -> std::io::Result<()>
where
    S: AsyncWrite + Unpin,
{
    // --- Consistency cut: exclusive barrier => quiescent store + single offset.
    let (snapshot, cut_offset, replid, mut rx) = {
        let _ex = repl.barrier.write();
        let rx = repl.subscribe(); // subscribe inside the exclusive section
        let cut = repl.offset();
        let snap = dump_store_to_bytes(store)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        (snap, cut, repl.replid(), rx)
    };

    // --- FULLRESYNC line.
    let line = format!("+FULLRESYNC {} {}\r\n", replid, cut_offset);
    stream.write_all(line.as_bytes()).await?;

    // --- Length-prefixed snapshot payload: "$<len>\r\n<bytes>" (no trailing CRLF).
    let hdr = format!("${}\r\n", snapshot.len());
    stream.write_all(hdr.as_bytes()).await?;
    stream.write_all(&snapshot).await?;
    stream.flush().await?;

    repl.incr_replicas();
    let result = stream_loop(&mut rx, cut_offset, stream).await;
    repl.decr_replicas();
    result
}

async fn stream_loop<S>(
    rx: &mut broadcast::Receiver<Frame>,
    cut_offset: u64,
    stream: &mut S,
) -> std::io::Result<()>
where
    S: AsyncWrite + Unpin,
{
    loop {
        match rx.recv().await {
            Ok((end_off, bytes)) => {
                // Discard frames already captured in the snapshot.
                if end_off <= cut_offset {
                    continue;
                }
                stream.write_all(&bytes).await?;
                stream.flush().await?;
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {
                // Replica too slow: drop it so it reconnects and full-resyncs.
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "replica lagged off the backlog",
                ));
            }
            Err(broadcast::error::RecvError::Closed) => return Ok(()),
        }
    }
}
```

- [ ] **Step 2: Detect PSYNC in `handle_connection` and hand off**

In `src/main.rs` `handle_connection`, the loop parses a frame then calls `execute_command`. Before dispatching, intercept PSYNC:

```rust
        // After parsing `command` (Vec<Bytes>) and before execute_command:
        if !command.is_empty()
            && command[0].len() == 5
            && command[0].eq_ignore_ascii_case(b"PSYNC")
        {
            // Flush any pending buffered response first.
            let _ = writer.flush(&mut stream).await;
            if let Some(r) = REPLICATION.get() {
                if let Err(e) = replication::serve_replica(r, &store, &mut stream).await {
                    tracing::info!(error = %e, "replica connection ended");
                }
            }
            return; // connection is consumed by replication
        }
```

(Use the same variable names already present in `handle_connection`: the parsed command vector, `writer`, `stream`, `store`. Adjust to match.)

- [ ] **Step 3: Build**

Run: `cargo build`
Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs src/replication.rs
git commit -m "feat(replication): PSYNC full-sync + live fan-out (primary side)"
```

---

## Phase 6 — Replica side: client loop + read-only gate + promotion

### Task 7: Replica client task, REPLICAOF dispatch, read-only enforcement, promotion

**Files:**
- Modify: `src/main.rs`, `src/replication.rs`

- [ ] **Step 1: Add the replica client loop to `src/replication.rs`**

Connects to the primary, runs the handshake, loads the snapshot, then applies the live stream by feeding frames into a provided synchronous apply callback. Reconnects with backoff. The apply callback is passed in from `main.rs` so `replication.rs` does not depend on `execute_command` directly.

```rust
use tokio::io::{AsyncReadExt, BufReader, AsyncBufReadExt};
use tokio::net::TcpStream;
use std::time::Duration;

/// Run the replica side forever: (re)connect to `host:port`, full-sync, then
/// apply the live stream. `clear` wipes the local store before loading a
/// snapshot; `load` installs snapshot bytes; `apply` runs one RESP command
/// frame through the normal command path. `should_run` returns false once this
/// node is no longer a replica of this master (e.g. promoted or re-pointed).
pub async fn replica_client(
    repl: std::sync::Arc<Replication>,
    host: String,
    port: u16,
    masterauth: String,
    clear: impl Fn() + Send + Sync,
    load: impl Fn(&[u8]) + Send + Sync,
    apply: impl Fn(Vec<Bytes>) + Send + Sync,
) {
    let mut backoff = Duration::from_millis(200);
    loop {
        // Stop if we are no longer a replica of this exact master.
        match repl.master() {
            Some(m) if m.host == host && m.port == port => {}
            _ => return,
        }

        match connect_and_sync(&repl, &host, port, &masterauth, &clear, &load, &apply).await {
            Ok(()) => {
                // stream ended cleanly (master closed) -> reconnect
            }
            Err(e) => {
                tracing::warn!(error = %e, host, port, "replica sync failed; retrying");
            }
        }
        repl.set_link_up(false);
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(5));
    }
}

async fn connect_and_sync(
    repl: &Replication,
    host: &str,
    port: u16,
    masterauth: &str,
    clear: &(impl Fn() + Send + Sync),
    load: &(impl Fn(&[u8]) + Send + Sync),
    apply: &(impl Fn(Vec<Bytes>) + Send + Sync),
) -> std::io::Result<()> {
    let stream = TcpStream::connect((host, port)).await?;
    let (rd, mut wr) = stream.into_split();
    let mut reader = BufReader::new(rd);

    // Handshake.
    wr.write_all(b"*1\r\n$4\r\nPING\r\n").await?;
    read_line(&mut reader).await?; // +PONG
    if !masterauth.is_empty() {
        let auth = encode_command(&[Bytes::from_static(b"AUTH"), Bytes::from(masterauth.as_bytes().to_vec())]);
        wr.write_all(&auth).await?;
        read_line(&mut reader).await?; // +OK
    }
    let lport = encode_command(&[
        Bytes::from_static(b"REPLCONF"),
        Bytes::from_static(b"listening-port"),
        Bytes::from(CONFIG_PORT.to_string().into_bytes()),
    ]);
    wr.write_all(&lport).await?;
    read_line(&mut reader).await?; // +OK
    wr.write_all(b"*3\r\n$5\r\nPSYNC\r\n$1\r\n?\r\n$2\r\n-1\r\n").await?;

    // +FULLRESYNC <replid> <offset>
    let line = read_line(&mut reader).await?;
    let _ = line; // (replid/offset parsed for future partial resync)

    // Snapshot: $<len>\r\n<bytes>
    let len = read_bulk_len(&mut reader).await?;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    clear();
    load(&buf);
    repl.set_link_up(true);
    tracing::info!(host, port, bytes = len, "replica full sync complete");

    // Live stream: parse RESP arrays forever, apply each.
    loop {
        match read_command(&mut reader).await? {
            Some(cmd) => apply(cmd),
            None => return Ok(()), // EOF
        }
    }
}
```

Add helper async fns `read_line` (reads one `\r\n`-terminated line as `Vec<u8>`), `read_bulk_len` (reads a `$<len>\r\n` header and returns `len`), and `read_command` (parses one RESP `*N` array into `Vec<Bytes>`, returns `Ok(None)` on clean EOF). Model `read_command` on `aof::CommandReader::next_command` but async over the `BufReader`. `CONFIG_PORT` is the local server port; pass it in instead of a global if cleaner — add a `listening_port: u16` parameter to `replica_client`/`connect_and_sync` and thread `CONFIG.server.port` from the caller.

- [ ] **Step 2: Spawn the replica task — at startup and on `REPLICAOF`**

Add a helper in `main.rs` that wires the callbacks and spawns the task:

```rust
fn spawn_replica_task(store: ShardedStore, host: String, port: u16) {
    let r = repl().clone();
    let masterauth = CONFIG.replication.masterauth.clone();
    let listening_port = CONFIG.server.port;
    let clear_store = store.clone();
    let load_store = store.clone();
    let apply_store = store.clone();

    tokio::spawn(async move {
        replication::replica_client(
            r,
            host,
            port,
            masterauth,
            move || clear_store.clear(),
            move |bytes: &[u8]| {
                if let Err(e) = persistence::load_store_from_bytes(&load_store, bytes) {
                    tracing::error!(error = %e, "failed to load replica snapshot");
                }
            },
            move |cmd: Vec<Bytes>| {
                // Apply via the normal path with an authenticated, master-marked state.
                let mut scratch = ConnectionState::new();
                scratch.authenticated = true;
                scratch.from_master = true;
                let mut w = RespWriter::new();
                let now = store::get_timestamp();
                execute_command(&apply_store, &cmd, &mut w, &mut scratch, now);
                w.clear(); // discard reply; replica applies silently
            },
            listening_port,
        )
        .await;
    });
}
```

At startup, after initializing `REPLICATION`, if `!CONFIG.replication.replicaof.is_empty()`, parse `host:port` and call `repl().set_master(host.clone(), port); spawn_replica_task(store.clone(), host, port);`.

- [ ] **Step 3: Dispatch `REPLICAOF` / `SLAVEOF`**

In `execute_command` add a branch (command length 9 for `REPLICAOF`, 7 for `SLAVEOF`):

```rust
    if (cmd.len() == 9 && cmd.eq_ignore_ascii_case(b"REPLICAOF"))
        || (cmd.len() == 7 && cmd.eq_ignore_ascii_case(b"SLAVEOF"))
    {
        if command.len() != 3 {
            writer.write_error(b"wrong number of arguments for 'replicaof'");
            return;
        }
        // REPLICAOF NO ONE -> promote
        if command[1].eq_ignore_ascii_case(b"NO") && command[2].eq_ignore_ascii_case(b"ONE") {
            repl().promote();
            writer.write_simple_string(b"OK");
            return;
        }
        let host = match std::str::from_utf8(&command[1]) {
            Ok(h) => h.to_string(),
            Err(_) => { writer.write_error(b"invalid host"); return; }
        };
        let port: u16 = match std::str::from_utf8(&command[2]).ok().and_then(|s| s.parse().ok()) {
            Some(p) => p,
            None => { writer.write_error(b"invalid port"); return; }
        };
        // Self-replication guard.
        if (host == "127.0.0.1" || host == "localhost" || host == CONFIG.server.bind)
            && port == CONFIG.server.port
        {
            writer.write_error(b"REPLICAOF would create a replication cycle to self");
            return;
        }
        repl().set_master(host.clone(), port);
        // NOTE: spawning the replica task requires the store handle, which
        // execute_command does have (`store`). See Step 4.
        spawn_replica_task(store.clone(), host, port);
        writer.write_simple_string(b"OK");
        return;
    }
```

> `execute_command` is synchronous but `spawn_replica_task` only calls `tokio::spawn`, which requires a runtime handle — it works because `handle_connection` runs inside the Tokio runtime. `store` is `&ShardedStore` (an `Arc` newtype); `.clone()` is cheap.

- [ ] **Step 4: Read-only gate**

At the top of `execute_command`, right after the barrier guard, add the read-only check. Reuse the write-command classification: writes are exactly the commands that call `on_write`. Implement a small predicate:

```rust
    // Reject client writes on a read-only replica. The internal apply path sets
    // `from_master = true` and bypasses this.
    if !state.from_master
        && CONFIG.replication.replica_read_only
        && REPLICATION.get().map(|r| r.role()) == Some(replication::Role::Replica)
        && is_write_command(cmd)
    {
        writer.write_error(b"READONLY You can't write against a read only replica.");
        return;
    }
```

Add `is_write_command(cmd: &[u8]) -> bool` matching the write verbs: `set, del, incr, incrby, decr, decrby, mset, hset, hdel, expire, persist, flushdb, flushall, getset, setex, setnx, append, setrange`. Keep the list aligned with the call sites that invoke `on_write` — if a verb calls `on_write`, it must be listed here. (Note: `-READONLY` is written via `write_error`, which prefixes `-ERR `; to emit the exact `-READONLY` prefix, add a `write_error_raw(&mut self, &[u8])` to `RespWriter` that writes `-` + message + CRLF without the `ERR ` prefix, and use it here. Add that method in `src/protocol.rs`.)

- [ ] **Step 5: Build**

Run: `cargo build`
Expected: clean build.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs src/replication.rs src/protocol.rs
git commit -m "feat(replication): replica client, REPLICAOF, read-only gate, promotion"
```

---

## Phase 7 — Observability

### Task 8: INFO replication section + Prometheus gauges

**Files:**
- Modify: `src/main.rs` (`handle_info`), `src/metrics.rs`

- [ ] **Step 1: Add the `# Replication` block to INFO**

In `handle_info`, append a replication section built from `repl()`:

```rust
    let r = repl();
    match r.role() {
        replication::Role::Primary => {
            info.push_str(&format!(
                "# Replication\r\nrole:master\r\nconnected_slaves:{}\r\nmaster_replid:{}\r\nmaster_repl_offset:{}\r\n",
                r.replica_count(), r.replid(), r.offset()
            ));
        }
        replication::Role::Replica => {
            let m = r.master();
            let (h, p, up) = m.map(|m| (m.host, m.port, m.link_up)).unwrap_or_default();
            info.push_str(&format!(
                "# Replication\r\nrole:slave\r\nmaster_host:{}\r\nmaster_port:{}\r\nmaster_link_status:{}\r\nslave_read_only:{}\r\nmaster_repl_offset:{}\r\n",
                h, p, if up { "up" } else { "down" },
                if CONFIG.replication.replica_read_only { 1 } else { 0 },
                r.offset()
            ));
        }
    }
```

(Adapt `info` to however `handle_info` accumulates its output string/buffer.)

- [ ] **Step 2: Add Prometheus gauges**

In `src/metrics.rs`, register three gauges (following the existing pre-created handle pattern): `redistill_connected_replicas`, `redistill_master_repl_offset`, `redistill_replica_link_up`. In `encode(&store)`, sync them from `crate::repl()` (or pass the replication handle in): set connected_replicas = `replica_count()`, master_repl_offset = `offset()`, replica_link_up = `1` if role==Replica and link up else `0`.

> If `metrics::encode` cannot see the `REPLICATION` global (lives in `main.rs`), move the global into `replication.rs` as a `pub static` accessor (`replication::global()`) so both `main.rs` and `metrics.rs` can read it. Prefer this: declare `static REPLICATION` and `pub fn global()` in `replication.rs`, and have `main.rs` call `replication::init(...)` / `replication::global()`.

- [ ] **Step 3: Build**

Run: `cargo build && cargo clippy --all-targets -- -D warnings`
Expected: clean build, no clippy errors.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs src/metrics.rs src/replication.rs
git commit -m "feat(replication): INFO replication section + Prometheus gauges"
```

---

## Phase 8 — Two-node integration tests

### Task 9: End-to-end replication tests

**Files:**
- Modify: `tests/replication_tests.rs`

- [ ] **Step 1: Add the two-node harness**

Append a harness modeled on `tests/aof_tests.rs` (`free_port`, `wait_for_port`, `Guard`, `send_and_read_reply`). Add a `spawn_primary(port)` and `spawn_replica(port, master_port)`:

```rust
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

fn cmd(parts: &[&[u8]]) -> Vec<u8> {
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
    conn.write_all(&cmd(parts)).unwrap();
    let mut buf = vec![0u8; 256];
    let n = conn.read(&mut buf).unwrap();
    buf.truncate(n);
    buf
}

/// Poll a GET on the replica until it returns the expected bulk string or times out.
fn wait_for_value(conn: &mut TcpStream, key: &[u8], expected: &[u8], timeout: Duration) {
    let deadline = Instant::now() + timeout;
    let want = {
        let mut v = format!("${}\r\n", expected.len()).into_bytes();
        v.extend_from_slice(expected);
        v.extend_from_slice(b"\r\n");
        v
    };
    while Instant::now() < deadline {
        let r = req(conn, &[b"GET", key]);
        if r == want {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("replica never converged on key");
}
```

- [ ] **Step 2: Test — full sync of pre-existing data**

```rust
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

    let mut r = TcpStream::connect(("127.0.0.1", rport)).unwrap();
    wait_for_value(&mut r, b"pre", b"loaded", Duration::from_secs(5));
}
```

- [ ] **Step 3: Test — live propagation**

```rust
#[test]
fn writes_propagate_to_replica() {
    let mport = free_port();
    let _mp = Guard(Some(spawn_primary(mport)));
    wait_for_port(mport, Duration::from_secs(5));
    let rport = free_port();
    let _rp = Guard(Some(spawn_replica(rport, mport)));
    wait_for_port(rport, Duration::from_secs(5));

    let mut m = TcpStream::connect(("127.0.0.1", mport)).unwrap();
    let mut r = TcpStream::connect(("127.0.0.1", rport)).unwrap();

    assert_eq!(req(&mut m, &[b"SET", b"live", b"yes"]), b"+OK\r\n");
    wait_for_value(&mut r, b"live", b"yes", Duration::from_secs(5));
}
```

- [ ] **Step 4: Test — read-only rejection**

```rust
#[test]
fn replica_rejects_writes() {
    let mport = free_port();
    let _mp = Guard(Some(spawn_primary(mport)));
    wait_for_port(mport, Duration::from_secs(5));
    let rport = free_port();
    let _rp = Guard(Some(spawn_replica(rport, mport)));
    wait_for_port(rport, Duration::from_secs(5));

    let mut r = TcpStream::connect(("127.0.0.1", rport)).unwrap();
    let reply = req(&mut r, &[b"SET", b"x", b"1"]);
    assert!(reply.starts_with(b"-READONLY"), "got {:?}", reply);
}
```

- [ ] **Step 5: Test — non-idempotent consistency (INCRBY during/after sync)**

```rust
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

    // Issue many INCRBY on the primary.
    for _ in 0..50 {
        let reply = req(&mut m, &[b"INCRBY", b"counter", b"1"]);
        assert!(reply.starts_with(b":"));
    }
    // Replica must converge to exactly 50 (not 49, not 51).
    let mut r = TcpStream::connect(("127.0.0.1", rport)).unwrap();
    wait_for_value(&mut r, b"counter", b"50", Duration::from_secs(5));
}
```

- [ ] **Step 6: Test — promotion**

```rust
#[test]
fn promoted_replica_accepts_writes() {
    let mport = free_port();
    let _mp = Guard(Some(spawn_primary(mport)));
    wait_for_port(mport, Duration::from_secs(5));
    let rport = free_port();
    let _rp = Guard(Some(spawn_replica(rport, mport)));
    wait_for_port(rport, Duration::from_secs(5));

    let mut r = TcpStream::connect(("127.0.0.1", rport)).unwrap();
    assert!(req(&mut r, &[b"SET", b"x", b"1"]).starts_with(b"-READONLY"));

    assert_eq!(req(&mut r, &[b"REPLICAOF", b"NO", b"ONE"]), b"+OK\r\n");
    assert_eq!(req(&mut r, &[b"SET", b"x", b"1"]), b"+OK\r\n");
}
```

- [ ] **Step 7: Run the full suite**

Run: `cargo test --test replication_tests`
Expected: all tests PASS. (If propagation tests flake, increase the `wait_for_value` timeout — the replica reconnect backoff starts at 200ms.)

- [ ] **Step 8: Commit**

```bash
git add tests/replication_tests.rs
git commit -m "test(replication): two-node integration tests"
```

---

## Phase 9 — Docs & final verification

### Task 10: Update CLAUDE.md and config docs; full verification

**Files:**
- Modify: `CLAUDE.md`, sample `redistill.toml` (if one exists in repo)

- [ ] **Step 1: Document the module**

Add a `replication.rs` row to the Module Layout table in `CLAUDE.md` and a "Replication" bullet under Key Design Patterns summarizing: `on_write` tap → broadcast fan-out; consistency cut via the exclusive barrier; replica applies through `execute_command`; manual `REPLICAOF`/`REPLICAOF NO ONE`; full-resync-only.

- [ ] **Step 2: Document config**

Add the `[replication]` keys (`replicaof`, `masterauth`, `replica_read_only`, `repl_backlog_size`) and their `REDIS_*` env overrides to the sample config and the Configuration section of `CLAUDE.md`.

- [ ] **Step 3: Full verification**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: formatted, no clippy warnings, all tests (unit + AOF + replication + existing integration) PASS.

- [ ] **Step 4: Commit**

```bash
git add CLAUDE.md redistill.toml
git commit -m "docs: document replication module and [replication] config"
```

---

## Self-Review Notes (for the implementer)

- **Spec coverage:** roles (Task 3/4), `REPLICAOF`+config trigger (Task 7), PSYNC-shaped handshake (Tasks 6–7), consistency cut via exclusive barrier (Task 6), replica apply via `execute_command` (Task 7), read-only + `from_master` bypass (Tasks 5,7), promotion (Task 7), async-only (no `WAIT` — intentionally absent), auth/TLS reuse (handshake AUTH in Task 7; TLS rides existing `MaybeStream` for the primary serve path — note: the *replica client* in Task 7 dials plaintext `TcpStream`; wrapping the outbound dial in TLS is deferred and called out below), observability (Task 8), failure/edge cases: lag-drop (Task 6 `stream_loop`), reconnect backoff (Task 7), self-replication guard (Task 7), re-point to new master (Task 7 `replica_client` `should_run` check), full-resync-only (no backlog replay).
- **Known v1 limitation to call out in the PR:** the outbound replica→primary dial is plaintext. If the primary requires TLS, the replica link needs a `tokio-rustls` client connector — deferred to a follow-up. Document this in CLAUDE.md.
- **Perf note:** the per-command barrier read-guard and the `feed_lock` mutex add small fixed costs on the write path; both are uncontended except during a full-sync cut. Revisit if profiling shows contention (a future optimization, not v1).
- **Type consistency checks:** `Frame = (u64, Bytes)` used consistently in `feed`/`subscribe`/`serve_replica`/`stream_loop`. `Role` enum used in main gate, INFO, metrics. `from_master`/`replconf_port` added in both `server.rs` and `lib.rs` stubs. `encode_command` returns `Bytes` everywhere. `dump_store_to_bytes`/`load_store_from_bytes` names match Task 1 and Task 7 callbacks.
