# Persistence benchmark harness

`persistence_bench.sh` is the reproducible workload we use to judge the
performance cost of each persistence mode (`none`, `rdb`, and — once it lands —
`aof-no`, `aof-everysec`, `aof-always`).

## What it does

1. Spawns a fresh release build of `redistill` on a free ephemeral port with
   persistence configured per the selected mode.
2. Waits for the server to answer `PING`.
3. Runs `redis-benchmark -t set -t get` for pipeline depths `1`, `16`, `128`
   (500,000 requests, 50 clients, 64-byte values).
4. Emits one TSV row per test on stdout:

   `mode  test  pipeline  clients  requests  rps  avg_ms  p50_ms  p99_ms`

5. Shuts the server down (SIGTERM, respecting the grace window) and removes
   the temp directory.

## Requirements

- `redis-benchmark` on `PATH` (ships with `redis-tools` / `brew install redis`).
- `python3` on `PATH` (used only for port selection and readiness probe).
- A release build: `cargo build --release`.

## Running

```bash
# Single mode
./persistence_bench.sh none > results.tsv

# All available modes, combined with header
{
  printf "mode\ttest\tpipeline\tclients\trequests\trps\tavg_ms\tp50_ms\tp99_ms\n"
  ./persistence_bench.sh none
  ./persistence_bench.sh rdb
  # ./persistence_bench.sh aof-no        # once AOF ships
  # ./persistence_bench.sh aof-everysec  #
  # ./persistence_bench.sh aof-always    #
} > results.tsv
```

## Baselines

- `baselines/persistence_pre_aof.tsv` — measured before AOF code existed.
  Captures `none` and `rdb` only.
- `baselines/persistence_with_aof.tsv` — measured after AOF write-path + replay
  landed. Covers all five modes. `none` and `rdb` here should stay within
  noise of the pre-AOF baseline (the AOF code must not leak cost into
  non-AOF paths).

## Expected cost per mode

Macbook-class hardware, `-n 500000 -c 50 -d 64`:

| Mode | SET P=1 vs none | SET P=16 vs none | SET P=128 vs none |
|---|---|---|---|
| `rdb` | ≈0 (noise) | ≈0 | ≈0 |
| `aof-no` | ≈0 | ~20% slower | ~45% slower |
| `aof-everysec` | ≈0 | ~20% slower | ~45% slower |
| `aof-always` | ~550× slower | unusable | unusable |

### Why AOF cost shows up at high pipeline depth

The current write path serializes through a single `Mutex<BufWriter<File>>`.
At P=1 the mutex is uncontended (one in-flight command per connection × 50
connections, network latency dominates). At P=128 many commands queue on the
mutex, which explains the ~45% drop on a pure-write benchmark. Real
applications rarely pipeline 128 writes; interactive SET/GET workloads at
P=1–8 should see near-zero overhead from `aof-everysec` or `aof-no`.

A future PR can swap the mutex for a per-connection buffer + background
writer task to claw back the P=16+ number. Not in scope for the initial AOF
PR.

### Running `aof-always`

`aof-always` issues an `fsync` on every write. On a MacBook SSD that's ~3 ms
per command, so a full 500k-request run can take 30+ minutes per pipeline
depth. Use a smaller request count:

```bash
BENCH_REQUESTS=20000 ./persistence_bench.sh aof-always > aof_always.tsv
```

## Interpreting noise

Results vary ±3–5% run-to-run on a loaded developer laptop. Don't chase a
single-digit regression. Re-run twice and compare medians if a number looks
off.
