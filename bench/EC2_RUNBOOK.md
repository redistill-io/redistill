# EC2 Performance Runbook — Redistill vs Redis vs Dragonfly

Goal: a fair, reproducible throughput + latency comparison on the **common command
subset Redistill supports today** (`SET` / `GET` / `INCR`), plus a Redistill-only
replication-overhead and failover measurement.

> **Fairness note.** Redis and Dragonfly implement far more than Redistill. We
> deliberately restrict the benchmark to `SET/GET/INCR` so we compare the same
> work. `redis-benchmark`'s default mix includes `LPUSH/SADD/...` which Redistill
> doesn't implement — those would error and skew results. The scripts here pin
> `-t set,get,incr`.

---

## 1. Instances

Use **two** identical instances in the **same AZ + same cluster placement group**
(realistic network) — one runs the server, one runs the load generator. Running
the client on the server box (loopback) measures raw engine speed but hides NIC
limits; do both if you want the full picture.

- **Recommended:** `c7i.8xlarge` (32 vCPU) — matches Redistill's tuned config
  (`num_shards = 2048`). For a cheaper run, `c7i.2xlarge`.
- **OS:** Ubuntu 24.04 LTS, root volume gp3 ≥ 20 GB.
- **Security group:** allow the server ports (6379–6381) from the client instance
  only. Open nothing to 0.0.0.0/0.

Set CPU governor to performance and raise file limits on the **server** box:
```bash
sudo sh -c 'echo performance | tee /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor' 2>/dev/null || true
ulimit -n 1048576
```

---

## 2. Load generator (client box)

```bash
sudo apt-get update
sudo apt-get install -y redis-tools git build-essential python3
# optional, better percentiles + mixed ratios:
sudo apt-get install -y memtier-benchmark || true
```

Copy the `bench/` directory from this repo to the client box:
```bash
scp -r bench/ ubuntu@<client-ip>:~/bench
chmod +x ~/bench/*.sh
```

---

## 3. Servers (server box) — launch ONE engine per port

Run all three on distinct ports so `compare.sh` can hit them. Persistence OFF for
the throughput comparison (all three engines, apples-to-apples). Run a second pass
with AOF/snapshots on if you care about the durability path.

### Redis 7.x (port 6380)
```bash
sudo apt-get install -y redis-server
redis-server --port 6380 --save '' --appendonly no \
  --maxmemory 0 --protected-mode no --daemonize yes
```

### Dragonfly (port 6381)
```bash
# latest release binary
curl -L -o dragonfly.tar.gz https://github.com/dragonflydb/dragonfly/releases/latest/download/dragonfly-x86_64.tar.gz
tar xzf dragonfly.tar.gz
sudo ./dragonfly-x86_64 --port 6381 --save_schedule '' --dbfilename '' \
  --cache_mode=false --daemonize=true || \
  nohup ./dragonfly-x86_64 --port 6381 --dbfilename '' >/tmp/df.log 2>&1 &
```

### Redistill (port 6379)
Build on the box (or cross-compile `x86_64-unknown-linux-gnu` and scp the binary):
```bash
# on the server box, in the repo checkout:
cargo build --release
REDIS_PORT=6379 REDIS_BIND=0.0.0.0 REDIS_HEALTH_CHECK_PORT=0 \
  REDIS_PERSISTENCE_ENABLED=false REDIS_AOF_ENABLED=false \
  ./target/release/redistill > /tmp/redistill.log 2>&1 &
```

Sanity-check each:
```bash
redis-cli -p 6379 ping && redis-cli -p 6380 ping && redis-cli -p 6381 ping
```

---

## 4. Run the comparison

From the **client box**, pointing `HOST` at the server's private IP:
```bash
cd ~/bench
HOST=<server-private-ip> \
REDISTILL_PORT=6379 REDIS_PORT=6380 DRAGONFLY_PORT=6381 \
REQUESTS=3000000 CLIENTS=50 \
./compare.sh
```

This sweeps `SET/GET/INCR` × payloads `{64,256}B` × pipeline `{1,16}` and prints a
side-by-side rps table + p99 latency table, and writes `results.csv`.

- **Pipeline 1** ≈ latency-bound per-op throughput.
- **Pipeline 16** ≈ saturated throughput (the big ops/sec headline).

Run 3× and take the median; ignore the first (warm-up) run.

### Optional: memtier (mixed 1:10 SET:GET, real percentiles)
```bash
memtier_benchmark -s <server-ip> -p 6379 --protocol=redis \
  --ratio=1:10 --data-size=128 -c 50 -t 8 --pipeline=16 \
  --test-time=30 --hide-histogram
# repeat for -p 6380 (redis) and -p 6381 (dragonfly)
```

---

## 5. Redistill replication overhead + failover

### 5a. Write-throughput cost of having a replica
Measure primary `SET` rps with **no** replica, then with **one** replica attached
(the broadcast fan-out adds per-write work):
```bash
# baseline (no replica) — from client box
redis-benchmark -h <server-ip> -p 6379 -t set -n 3000000 -c 50 -P 16 -d 64 -q

# attach a replica on the server box (second instance, port 6390)
REDIS_PORT=6390 REDIS_BIND=0.0.0.0 REDIS_HEALTH_CHECK_PORT=0 \
  REDIS_PERSISTENCE_ENABLED=false REDIS_AOF_ENABLED=false \
  REDIS_REPLICAOF=127.0.0.1:6379 ./target/release/redistill >/tmp/replica.log 2>&1 &
redis-cli -p 6390 INFO | grep master_link_status   # expect: up

# with replica attached — rerun the same SET benchmark
redis-benchmark -h <server-ip> -p 6379 -t set -n 3000000 -c 50 -P 16 -d 64 -q
```
Report the delta (expected: a few % from the broadcast send per write).

### 5b. Replica read throughput
```bash
redis-cli -p 6379 MSET k1 v k2 v k3 v >/dev/null
redis-benchmark -h <server-ip> -p 6390 -t get -n 3000000 -c 50 -P 16 -d 64 -q
```

### 5c. Failover detection latency (true crash)
```bash
# write a marker on the primary
redis-cli -p 6379 set marker before
# hard-kill the primary (simulates node death — NOT graceful SIGTERM)
kill -9 $(pgrep -f 'redistill.*6379')
# poll the replica's link status; time how long until it flips to down
while :; do
  S=$(redis-cli -p 6390 INFO | grep -o 'master_link_status:[a-z]*'); echo "$(date +%s.%N) $S";
  [ "$S" = "master_link_status:down" ] && break; sleep 0.2;
done
```
(Promotion is manual in v1: `redis-cli -p 6390 REPLICAOF NO ONE` then the replica
accepts writes. Note the graceful-SIGTERM caveat: a *gracefully* stopped primary
keeps replica streams open for its drain window — use `kill -9` to model failover.)

---

## 6. What to capture for the MR

- `results.csv` + the `compare.sh` summary table (rps + p99), 3-run median.
- Replication overhead: primary SET rps with/without a replica (% delta).
- Replica GET rps.
- Failover detection time from 5c.
- Instance type, kernel, and the exact server launch flags (paste them).

## 7. Caveats / gotchas

- **Single-threaded clients lie.** Use `-c 50`+ and `-P 16` so the server, not the
  client, is the bottleneck. Watch server CPU (`top`) — if a server pins 1 core
  while others idle, that's a real finding (sharding/threading limit).
- **Dragonfly is multi-threaded by design** and will likely top raw throughput;
  the interesting numbers are p99 latency and per-core efficiency.
- **Redistill `num_shards`** defaults to 2048 (tuned for ~32 cores). On a small
  instance, lower it (`REDIS_NUM_SHARDS`/config) — too many shards on few cores
  adds overhead.
- Keep all three on the **same box + same flags** for each pass. Don't compare a
  persistence-on engine against persistence-off ones.
