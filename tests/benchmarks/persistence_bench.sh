#!/usr/bin/env bash
# Persistence benchmark harness.
#
# Spawns a fresh redistill (release build) with the requested persistence
# config, runs a standardized redis-benchmark workload, emits a TSV row
# per test to stdout, and cleans up.
#
# Usage:
#   ./persistence_bench.sh <mode> [> results.tsv]
#
# Modes:
#   none        persistence disabled entirely
#   rdb         RDB snapshots enabled, no periodic saves during the run
#   aof-no      (reserved; added when AOF ships)
#   aof-everysec (reserved)
#   aof-always  (reserved)
#
# Output columns (tab-separated):
#   mode  test  pipeline  clients  requests  rps  avg_latency_ms  p50_ms  p99_ms
#
# Requires: redis-benchmark on PATH, a release build at target/release/redistill.

set -euo pipefail

MODE="${1:-}"
if [[ -z "$MODE" ]]; then
    echo "usage: $0 <none|rdb|aof-no|aof-everysec|aof-always>" >&2
    exit 2
fi

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
BIN="$REPO_ROOT/target/release/redistill"
if [[ ! -x "$BIN" ]]; then
    echo "release binary missing: $BIN (run: cargo build --release)" >&2
    exit 1
fi

if ! command -v redis-benchmark >/dev/null; then
    echo "redis-benchmark not found on PATH" >&2
    exit 1
fi

# --- pick a free port ----------------------------------------------------
PORT=$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')
TMPDIR=$(mktemp -d)
SNAPSHOT="$TMPDIR/redistill.rdb"

# --- persistence env per mode -------------------------------------------
case "$MODE" in
    none)
        PERSIST_ENV=(
            "REDIS_PERSISTENCE_ENABLED=false"
        )
        ;;
    rdb)
        PERSIST_ENV=(
            "REDIS_PERSISTENCE_ENABLED=true"
            "REDIS_SNAPSHOT_PATH=$SNAPSHOT"
            # Disable periodic saves so we measure pure write path, not the
            # occasional snapshot stall.
            "REDIS_SNAPSHOT_INTERVAL=0"
            "REDIS_SAVE_ON_SHUTDOWN=true"
        )
        ;;
    aof-no)
        PERSIST_ENV=(
            "REDIS_PERSISTENCE_ENABLED=false"
            "REDIS_AOF_ENABLED=true"
            "REDIS_AOF_PATH=$TMPDIR/redistill.aof"
            "REDIS_AOF_FSYNC=no"
        )
        ;;
    aof-everysec)
        PERSIST_ENV=(
            "REDIS_PERSISTENCE_ENABLED=false"
            "REDIS_AOF_ENABLED=true"
            "REDIS_AOF_PATH=$TMPDIR/redistill.aof"
            "REDIS_AOF_FSYNC=everysec"
        )
        ;;
    aof-always)
        PERSIST_ENV=(
            "REDIS_PERSISTENCE_ENABLED=false"
            "REDIS_AOF_ENABLED=true"
            "REDIS_AOF_PATH=$TMPDIR/redistill.aof"
            "REDIS_AOF_FSYNC=always"
        )
        ;;
    *)
        echo "unknown mode: $MODE" >&2
        exit 2
        ;;
esac

# --- spawn server --------------------------------------------------------
LOGFILE="$TMPDIR/server.log"
env \
    "REDIS_PORT=$PORT" \
    "REDIS_BIND=127.0.0.1" \
    "REDIS_HEALTH_CHECK_PORT=0" \
    "REDIS_SHUTDOWN_GRACE_SECS=10" \
    "${PERSIST_ENV[@]}" \
    "$BIN" >"$LOGFILE" 2>&1 &
SERVER_PID=$!

cleanup() {
    # Graceful shutdown first so snapshot (if enabled) gets written.
    if kill -0 "$SERVER_PID" 2>/dev/null; then
        kill -TERM "$SERVER_PID" 2>/dev/null || true
        # Give it up to the grace window to exit cleanly.
        for _ in $(seq 1 50); do
            kill -0 "$SERVER_PID" 2>/dev/null || break
            sleep 0.1
        done
        kill -KILL "$SERVER_PID" 2>/dev/null || true
    fi
    rm -rf "$TMPDIR"
}
trap cleanup EXIT

# --- wait for readiness --------------------------------------------------
# Send a single RESP PING and read back +PONG\r\n. Avoids redis-benchmark's
# connection-pool startup cost just for a readiness check.
probe() {
    python3 - "$1" <<'PY' >/dev/null 2>&1
import socket, sys
port = int(sys.argv[1])
s = socket.socket()
s.settimeout(0.5)
try:
    s.connect(("127.0.0.1", port))
    s.sendall(b"*1\r\n$4\r\nPING\r\n")
    data = s.recv(16)
    sys.exit(0 if data.startswith(b"+PONG") else 1)
except Exception:
    sys.exit(1)
finally:
    s.close()
PY
}
for _ in $(seq 1 50); do
    if probe "$PORT"; then
        READY=1
        break
    fi
    sleep 0.1
done
if [[ "${READY:-0}" != "1" ]]; then
    echo "server on port $PORT never became ready; log:" >&2
    cat "$LOGFILE" >&2
    exit 1
fi

# --- run standardized workload ------------------------------------------
# Workload matrix: commands × pipeline depths. Small enough to finish in
# seconds, large enough to amortize warm-up. Fixed params for reproducibility.
# Tunable via env so aof-always (which fsyncs every write) can run a shorter
# matrix without rewriting the script.
REQUESTS="${BENCH_REQUESTS:-500000}"
CLIENTS="${BENCH_CLIENTS:-50}"
DATA_SIZE="${BENCH_DATA_SIZE:-64}"

run_one() {
    local test="$1" pipeline="$2"
    # redis-benchmark's CSV mode only emits rps, so parse the default output
    # ourselves: we want rps + latency percentiles. The human summary includes:
    #   "... throughput summary: X.XX requests per second"
    #   "latency summary ... avg X min X p50 X p95 X p99 X max X" (newer builds)
    local out
    out=$(redis-benchmark -h 127.0.0.1 -p "$PORT" \
        -t "$test" -n "$REQUESTS" -c "$CLIENTS" -P "$pipeline" -d "$DATA_SIZE" 2>&1 || true)

    local rps avg p50 p99
    rps=$(grep -E "throughput summary" <<<"$out" | awk -F':' '{print $2}' | awk '{print $1}' | head -1)
    # Latency line looks like:
    #   latency summary (msec):
    #           avg       min       p50       p95       p99       max
    #         0.135     0.024     0.127     0.263     0.511    12.511
    avg=$(awk '/avg.*min.*p50/{getline; print $1; exit}' <<<"$out")
    p50=$(awk '/avg.*min.*p50/{getline; print $3; exit}' <<<"$out")
    p99=$(awk '/avg.*min.*p50/{getline; print $5; exit}' <<<"$out")

    printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n" \
        "$MODE" "$test" "$pipeline" "$CLIENTS" "$REQUESTS" \
        "${rps:-NA}" "${avg:-NA}" "${p50:-NA}" "${p99:-NA}"
}

# Header only if stdout is a terminal or if caller wants it (pipe it yourself).
if [[ -t 1 ]]; then
    printf "mode\ttest\tpipeline\tclients\trequests\trps\tavg_ms\tp50_ms\tp99_ms\n"
fi

for test in set get; do
    for p in 1 16 128; do
        run_one "$test" "$p"
    done
done
