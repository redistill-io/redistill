#!/usr/bin/env python3
"""Replication chaos / simulation harness for Redistill.

Spawns real primary + replica processes from the release binary, drives
concurrent write load and failure injection, then checks the replica converges
to the primary's exact dataset (keys, values, hash fields, TTL presence).

Usage:
    cargo build --release
    python3 tests/scripts/replication_sim.py            # all scenarios
    python3 tests/scripts/replication_sim.py storm oom  # selected scenarios
    python3 tests/scripts/replication_sim.py --list

No third-party dependencies (raw RESP over sockets).
"""

import os
import random
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BIN = os.environ.get("REDISTILL_BIN", os.path.join(ROOT, "target", "release", "redistill"))
VERBOSE = os.environ.get("SIM_VERBOSE") == "1"


# ---------------------------------------------------------------- RESP client


class RespError(Exception):
    pass


class Client:
    def __init__(self, port, password=None, timeout=5.0):
        self.port = port
        self.sock = socket.create_connection(("127.0.0.1", port), timeout=timeout)
        self.buf = b""
        if password:
            self.call("AUTH", password)

    def close(self):
        try:
            self.sock.close()
        except OSError:
            pass

    def _fill(self):
        chunk = self.sock.recv(65536)
        if not chunk:
            raise ConnectionError("connection closed")
        self.buf += chunk

    def _line(self):
        while b"\r\n" not in self.buf:
            self._fill()
        line, self.buf = self.buf.split(b"\r\n", 1)
        return line

    def _read(self):
        line = self._line()
        t, rest = line[:1], line[1:]
        if t == b"+":
            return rest.decode()
        if t == b"-":
            return RespError(rest.decode())
        if t == b":":
            return int(rest)
        if t == b"$":
            n = int(rest)
            if n < 0:
                return None
            while len(self.buf) < n + 2:
                self._fill()
            data, self.buf = self.buf[:n], self.buf[n + 2 :]
            return data
        if t == b"*":
            n = int(rest)
            if n < 0:
                return None
            return [self._read() for _ in range(n)]
        raise ValueError("bad RESP type %r" % line)

    def call(self, *args, raise_err=True):
        parts = [a if isinstance(a, bytes) else str(a).encode() for a in args]
        out = b"*%d\r\n" % len(parts)
        for p in parts:
            out += b"$%d\r\n%s\r\n" % (len(p), p)
        self.sock.sendall(out)
        r = self._read()
        if isinstance(r, RespError) and raise_err:
            raise r
        return r


def info(port, password=None):
    c = Client(port, password)
    try:
        raw = c.call("INFO")
    finally:
        c.close()
    out = {}
    for line in raw.decode().splitlines():
        if ":" in line:
            k, v = line.split(":", 1)
            out[k.strip()] = v.strip()
    return out


# ---------------------------------------------------------------- processes


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


class Node:
    def __init__(self, name, port=None, replicaof=None, env=None, tmpdir=None):
        self.name = name
        self.port = port or free_port()
        self.replicaof = replicaof
        self.extra_env = dict(env or {})
        self.tmpdir = tmpdir or tempfile.mkdtemp(prefix="redistill-sim-")
        self.proc = None
        self.log_path = os.path.join(self.tmpdir, "%s-%d.log" % (name, self.port))
        self.password = self.extra_env.get("REDIS_PASSWORD") or None

    def start(self):
        env = dict(os.environ)
        env.update(
            {
                "REDIS_PORT": str(self.port),
                "REDIS_BIND": "127.0.0.1",
                "REDIS_HEALTH_CHECK_PORT": "0",
                "REDIS_PERSISTENCE_ENABLED": "false",
                "REDIS_AOF_ENABLED": "false",
                "REDIS_SNAPSHOT_PATH": os.path.join(self.tmpdir, "%s.rdb" % self.name),
                "REDIS_AOF_PATH": os.path.join(self.tmpdir, "%s.aof" % self.name),
            }
        )
        if self.replicaof:
            env["REDIS_REPLICAOF"] = "127.0.0.1:%d" % self.replicaof
        env.update(self.extra_env)
        log = open(self.log_path, "ab")
        self.proc = subprocess.Popen([BIN], env=env, stdout=log, stderr=log, cwd=self.tmpdir)
        deadline = time.time() + 10
        while time.time() < deadline:
            try:
                socket.create_connection(("127.0.0.1", self.port), timeout=0.2).close()
                return self
            except OSError:
                if self.proc.poll() is not None:
                    raise RuntimeError("%s exited early, see %s" % (self.name, self.log_path))
                time.sleep(0.05)
        raise RuntimeError("%s did not start" % self.name)

    def kill(self, sig=signal.SIGKILL):
        if self.proc and self.proc.poll() is None:
            self.proc.send_signal(sig)
            if sig in (signal.SIGKILL, signal.SIGTERM):
                self.proc.wait(timeout=35)

    def client(self):
        return Client(self.port, self.password)

    def info(self):
        return info(self.port, self.password)


class Cluster:
    """Tracks every spawned node so each scenario tears down cleanly."""

    def __init__(self):
        self.nodes = []
        self.tmpdir = tempfile.mkdtemp(prefix="redistill-sim-")

    def node(self, name, **kw):
        n = Node(name, tmpdir=self.tmpdir, **kw)
        self.nodes.append(n)
        return n.start()

    def close(self):
        for n in self.nodes:
            try:
                if n.proc and n.proc.poll() is None:
                    n.proc.send_signal(signal.SIGCONT)
                n.kill()
            except Exception:
                pass


# ---------------------------------------------------------------- helpers


def wait_until(pred, timeout=15.0, what="condition"):
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        try:
            if pred():
                return
        except (OSError, ConnectionError, RespError) as e:
            last = e
        time.sleep(0.05)
    raise AssertionError("timeout waiting for %s (last error: %s)" % (what, last))


def wait_link_up(replica, timeout=15.0):
    wait_until(lambda: replica.info().get("master_link_status") == "up", timeout, "%s link up" % replica.name)


def wait_caught_up(primary, replica, timeout=20.0):
    """Wait for replica offset to equal primary offset, stably (no in-flight writes)."""

    def ok():
        p1 = int(primary.info()["master_repl_offset"])
        r = replica.info()
        if r.get("master_link_status") != "up":
            return False
        p2 = int(primary.info()["master_repl_offset"])
        return p1 == p2 == int(r["master_repl_offset"])

    wait_until(ok, timeout, "%s offset to catch up with %s" % (replica.name, primary.name))


def dump(node):
    """Return {key: ('s', value) | ('h', {field: value}), with TTL-presence flag}."""
    c = node.client()
    try:
        keys = c.call("KEYS", "*") or []
        out = {}
        for k in keys:
            t = c.call("TYPE", k)
            if t == "string":
                v = c.call("GET", k)
                if v is None:
                    continue
                val = ("s", v)
            elif t == "hash":
                flat = c.call("HGETALL", k) or []
                if not flat:
                    continue
                val = ("h", tuple(sorted(zip(flat[0::2], flat[1::2]))))
            else:
                continue
            ttl = c.call("TTL", k)
            out[k] = (val, ttl >= 0)
        return out
    finally:
        c.close()


def assert_same(primary, replica, label=""):
    a, b = dump(primary), dump(replica)
    if a == b:
        print("    [ok] %s%s == %s (%d keys)" % (label, primary.name, replica.name, len(a)))
        return
    only_a = sorted(set(a) - set(b))[:5]
    only_b = sorted(set(b) - set(a))[:5]
    diff = [k for k in set(a) & set(b) if a[k] != b[k]][:5]
    msg = "%s%s(%d) != %s(%d): only-primary=%s only-replica=%s differing=%s" % (
        label, primary.name, len(a), replica.name, len(b), only_a, only_b,
        [(k, a[k], b[k]) for k in diff],
    )
    raise AssertionError(msg)


class Load:
    """Concurrent random writers on a small, overlapping keyspace."""

    def __init__(self, node, threads=8, keyspace=200, ttl=False, value_size=16, ops=None):
        self.node = node
        self.threads = threads
        self.keyspace = keyspace
        self.ttl = ttl
        self.value_size = value_size
        self.ops = ops
        self.stop_ev = threading.Event()
        self.count = 0
        self.errors = 0
        self.workers = []
        self.lock = threading.Lock()

    def _op(self, c, rnd):
        k = "k%d" % rnd.randrange(self.keyspace)
        h = "h%d" % rnd.randrange(self.keyspace // 4 or 1)
        v = rnd.randbytes(self.value_size).hex()
        choice = rnd.random()
        if choice < 0.30:
            c.call("SET", k, v)
        elif choice < 0.40:
            c.call("SET", k, v, "EX", rnd.randint(30, 300)) if self.ttl else c.call("SET", k, v)
        elif choice < 0.55:
            c.call("INCRBY", "ctr%d" % rnd.randrange(10), rnd.randint(1, 5))
        elif choice < 0.65:
            args = []
            for _ in range(rnd.randint(2, 5)):
                args += ["k%d" % rnd.randrange(self.keyspace), rnd.randbytes(4).hex()]
            c.call("MSET", *args)
        elif choice < 0.78:
            c.call("HSET", h, "f%d" % rnd.randrange(8), v)
        elif choice < 0.83:
            c.call("HDEL", h, "f%d" % rnd.randrange(8))
        elif choice < 0.92:
            c.call("DEL", k)
        elif choice < 0.97 and self.ttl:
            c.call("EXPIRE", k, rnd.randint(30, 300))
        elif self.ttl:
            c.call("PERSIST", k)
        else:
            c.call("INCR", "ctr%d" % rnd.randrange(10))

    def _run(self, seed):
        rnd = random.Random(seed)
        c = None
        while not self.stop_ev.is_set():
            try:
                if c is None:
                    c = self.node.client()
                self._op(c, rnd)
                with self.lock:
                    self.count += 1
                    if self.ops and self.count >= self.ops:
                        self.stop_ev.set()
            except RespError:
                with self.lock:
                    self.errors += 1
            except (OSError, ConnectionError):
                if c:
                    c.close()
                c = None
                time.sleep(0.05)
        if c:
            c.close()

    def start(self):
        for i in range(self.threads):
            t = threading.Thread(target=self._run, args=(random.randrange(1 << 30) + i,), daemon=True)
            t.start()
            self.workers.append(t)
        return self

    def stop(self):
        self.stop_ev.set()
        for t in self.workers:
            t.join(timeout=10)
        return self


# ---------------------------------------------------------------- scenarios


def sc_storm(cl):
    """Concurrent writers with a replica attached throughout."""
    p = cl.node("primary")
    r = cl.node("replica", replicaof=p.port)
    wait_link_up(r)
    load = Load(p, threads=12, ttl=True).start()
    time.sleep(4)
    load.stop()
    print("    %d ops, %d errors" % (load.count, load.errors))
    wait_caught_up(p, r)
    assert_same(p, r)


def sc_attach_mid_storm(cl):
    """Replicas attach (full-sync cut) while writes are in flight."""
    p = cl.node("primary")
    load = Load(p, threads=12, ttl=True).start()
    time.sleep(1)
    r1 = cl.node("replica1", replicaof=p.port)
    time.sleep(0.5)
    r2 = cl.node("replica2", replicaof=p.port)
    time.sleep(2)
    load.stop()
    print("    %d ops" % load.count)
    for r in (r1, r2):
        wait_caught_up(p, r)
        assert_same(p, r)


def sc_replica_crash_churn(cl):
    """SIGKILL and restart the replica repeatedly during load."""
    p = cl.node("primary")
    r = cl.node("replica", replicaof=p.port)
    wait_link_up(r)
    load = Load(p, threads=8).start()
    for i in range(5):
        time.sleep(0.6)
        r.kill()
        time.sleep(0.2)
        r.start()
    time.sleep(1)
    load.stop()
    wait_caught_up(p, r)
    assert_same(p, r)


def sc_slow_replica_lag(cl):
    """Tiny backlog + paused replica: primary must drop it, replica must full-resync."""
    p = cl.node("primary", env={"REDIS_REPL_BACKLOG_SIZE": "16"})
    r = cl.node("replica", replicaof=p.port)
    wait_link_up(r)
    load = Load(p, threads=8, value_size=2048).start()
    time.sleep(0.5)
    r.proc.send_signal(signal.SIGSTOP)
    time.sleep(1.5)
    r.proc.send_signal(signal.SIGCONT)
    time.sleep(1.5)
    load.stop()
    wait_caught_up(p, r, timeout=30)
    assert_same(p, r)
    log = open(p.log_path, "rb").read()
    if b"lagged" not in log:
        print("    [note] primary never logged a lag drop (backlog may not have overflowed)")


def sc_primary_restart(cl):
    """Primary SIGKILL + restart (no persistence): replica must converge to the new (empty+new) image."""
    p = cl.node("primary")
    r = cl.node("replica", replicaof=p.port)
    wait_link_up(r)
    load = Load(p, threads=4, ops=2000).start()
    for t in load.workers:
        t.join()
    wait_caught_up(p, r)
    p.kill()
    wait_until(lambda: r.info().get("master_link_status") == "down", 10, "link down")
    p.start()
    c = p.client()
    c.call("SET", "after-restart", "1")
    c.close()
    wait_caught_up(p, r)
    assert_same(p, r)


def sc_promote_and_swap(cl):
    """Manual failover: promote replica, repoint old primary at it, keep writing."""
    p = cl.node("primary")
    r = cl.node("replica", replicaof=p.port)
    wait_link_up(r)
    load = Load(p, threads=8).start()
    time.sleep(2)
    load.stop()
    wait_caught_up(p, r)
    c = r.client()
    c.call("REPLICAOF", "NO", "ONE")
    c.call("SET", "post-failover", "yes")
    c.close()
    c = p.client()
    c.call("REPLICAOF", "127.0.0.1", r.port)
    c.close()
    wait_link_up(p)
    load = Load(r, threads=8).start()
    time.sleep(2)
    load.stop()
    wait_caught_up(r, p)
    assert_same(r, p, "after swap: ")


def sc_aof_rewrite_under_load(cl):
    """AOF on both sides (write_order path), BGREWRITEAOF mid-load, then restart replica from its AOF."""
    env = {"REDIS_AOF_ENABLED": "true", "REDIS_AOF_FSYNC": "everysec"}
    p = cl.node("primary", env=env)
    r = cl.node("replica", replicaof=p.port, env=env)
    wait_link_up(r)
    load = Load(p, threads=8, ttl=True).start()
    for _ in range(3):
        time.sleep(0.7)
        for n in (p, r):
            c = n.client()
            c.call("BGREWRITEAOF")
            c.close()
    load.stop()
    wait_caught_up(p, r)
    assert_same(p, r)
    # Replica restarts from its own AOF while the primary is down: local image must match.
    expected = dump(p)
    p.kill(signal.SIGTERM)
    r.kill(signal.SIGTERM)
    r.replicaof = None  # boot standalone to inspect the persisted image
    r.start()
    got = dump(r)
    if got != expected:
        raise AssertionError("replica AOF image differs after restart: %d vs %d keys" % (len(got), len(expected)))
    print("    [ok] replica AOF restart image matches (%d keys)" % len(got))


def sc_auth(cl):
    """requirepass + masterauth: correct auth syncs, wrong auth never links."""
    p = cl.node("primary", env={"REDIS_PASSWORD": "s3cret"})
    good = cl.node("good", replicaof=p.port, env={"REDIS_MASTERAUTH": "s3cret"})
    bad = cl.node("bad", replicaof=p.port, env={"REDIS_MASTERAUTH": "nope"})
    c = p.client()
    c.call("SET", "x", "1")
    c.close()
    wait_caught_up(p, good)
    assert_same(p, good)
    time.sleep(1)
    assert bad.info().get("master_link_status") == "down", "bad masterauth replica linked up"
    print("    [ok] wrong masterauth stays down")
    # Unauthenticated PSYNC must not leak data.
    s = socket.create_connection(("127.0.0.1", p.port))
    s.sendall(b"*3\r\n$5\r\nPSYNC\r\n$1\r\n?\r\n$2\r\n-1\r\n")
    reply = s.recv(4096)
    s.close()
    assert b"NOAUTH" in reply.split(b"\r\n")[0], reply
    print("    [ok] unauthenticated PSYNC rejected")


def sc_oom_rejected_write(cl):
    """A write the primary rejects with OOM must not be applied on the replica."""
    p = cl.node("primary", env={"REDIS_MAX_MEMORY": "200000", "REDIS_EVICTION_POLICY": "noeviction"})
    r = cl.node("replica", replicaof=p.port)
    wait_link_up(r)
    c = p.client()
    rejected = None
    for i in range(10000):
        res = c.call("SET", "fill%d" % i, "x" * 500, raise_err=False)
        if isinstance(res, RespError):
            rejected = "fill%d" % i
            break
    c.close()
    assert rejected, "never hit OOM"
    print("    primary rejected %s with OOM" % rejected)
    wait_caught_up(p, r)
    assert_same(p, r)


def sc_eviction(cl):
    """allkeys-lru on the primary: evicted keys must disappear from the replica too."""
    p = cl.node("primary", env={"REDIS_MAX_MEMORY": "300000", "REDIS_EVICTION_POLICY": "allkeys-lru"})
    r = cl.node("replica", replicaof=p.port)
    wait_link_up(r)
    load = Load(p, threads=8, keyspace=5000, value_size=200).start()
    time.sleep(3)
    load.stop()
    wait_caught_up(p, r)
    assert_same(p, r)


def sc_lazy_expiry(cl):
    """Keys lazily expired on primary reads are reclaimed on the replica (after its grace window)."""
    p = cl.node("primary")
    r = cl.node("replica", replicaof=p.port)
    wait_link_up(r)
    c = p.client()
    n = 500
    for i in range(n):
        c.call("SET", "tmp%d" % i, "v", "EX", 1)
    c.call("SET", "keep", "v")
    time.sleep(1.5)
    for i in range(n):
        c.call("GET", "tmp%d" % i)  # lazily removes on primary (no DEL on the stream)
    c.close()
    wait_caught_up(p, r)
    pd = Client(p.port).call("DBSIZE")
    print("    DBSIZE primary=%d replica=%d right after lazy expiry" % (pd, Client(r.port).call("DBSIZE")))
    # Replica grace is 10s; the sampler (20 random shards / 100ms) then drains
    # sparse keyspaces exponentially, so require most keys reclaimed, not all.
    wait_until(lambda: Client(r.port).call("DBSIZE") <= pd + n // 4, 45, "replica to reclaim expired keys")
    print("    DBSIZE replica=%d after grace window" % Client(r.port).call("DBSIZE"))
    assert_same(p, r)


def sc_readonly(cl):
    """Client writes on a read-only replica are rejected; replica-of-replica PSYNC rejected."""
    p = cl.node("primary")
    r = cl.node("replica", replicaof=p.port)
    wait_link_up(r)
    c = r.client()
    for cmd in (("SET", "a", "1"), ("DEL", "a"), ("HSET", "h", "f", "v"), ("FLUSHALL",), ("EXPIRE", "a", "5")):
        res = c.call(*cmd, raise_err=False)
        assert isinstance(res, RespError) and str(res).startswith("READONLY"), (cmd, res)
    c.close()
    rr = cl.node("subreplica", replicaof=r.port)
    time.sleep(1)
    assert rr.info().get("master_link_status") == "down"
    print("    [ok] read-only enforced, chained replication refused")


SCENARIOS = {
    "storm": sc_storm,
    "attach": sc_attach_mid_storm,
    "churn": sc_replica_crash_churn,
    "lag": sc_slow_replica_lag,
    "primary-restart": sc_primary_restart,
    "failover": sc_promote_and_swap,
    "aof": sc_aof_rewrite_under_load,
    "auth": sc_auth,
    "readonly": sc_readonly,
    "eviction": sc_eviction,
    "oom": sc_oom_rejected_write,
    "lazy-expiry": sc_lazy_expiry,
}


def main(argv):
    if "--list" in argv:
        for k, f in SCENARIOS.items():
            print("%-16s %s" % (k, f.__doc__))
        return 0
    if not os.path.exists(BIN):
        print("binary not found: %s (run cargo build --release)" % BIN)
        return 2
    names = [a for a in argv if not a.startswith("-")] or list(SCENARIOS)
    failed = []
    for name in names:
        fn = SCENARIOS[name]
        cl = Cluster()
        print("== %s: %s" % (name, fn.__doc__))
        t0 = time.time()
        try:
            fn(cl)
            print("   PASS (%.1fs)" % (time.time() - t0))
        except Exception as e:
            print("   FAIL: %s: %s" % (type(e).__name__, e))
            print("   logs: %s" % cl.tmpdir)
            failed.append(name)
        finally:
            cl.close()
    print("\n%d/%d passed%s" % (len(names) - len(failed), len(names), (" — failed: " + ", ".join(failed)) if failed else ""))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
