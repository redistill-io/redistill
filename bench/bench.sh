#!/usr/bin/env bash
# Benchmark one target (Redistill / Redis / Dragonfly) over the RESP protocol.
#
# Usage:  ./bench.sh <host> <port> <label> [auth_password]
# Example: ./bench.sh 127.0.0.1 6379 redistill
#
# Only exercises commands Redistill actually implements (SET / GET / INCR), so
# the comparison against Redis/Dragonfly is apples-to-apples. Results are
# appended to results.csv in this directory.
set -euo pipefail

HOST="${1:?host}"; PORT="${2:?port}"; LABEL="${3:?label}"; AUTH="${4:-}"
AUTH_ARG=(); [ -n "$AUTH" ] && AUTH_ARG=(-a "$AUTH")

OUT="$(dirname "$0")/results.csv"
[ -f "$OUT" ] || echo "label,test,payload_bytes,pipeline,clients,rps,avg_latency_ms,p50_ms,p99_ms" > "$OUT"

REQUESTS="${REQUESTS:-2000000}"
CLIENTS="${CLIENTS:-50}"

command -v redis-benchmark >/dev/null || { echo "redis-benchmark not found (install redis-tools)"; exit 1; }

echo "== $LABEL @ $HOST:$PORT  (requests=$REQUESTS clients=$CLIENTS) =="

# Sweep payload sizes and pipeline depths. P=1 isolates per-op latency;
# P=16 measures pipelined throughput (the headline ops/sec number).
for D in 64 256; do
  for P in 1 16; do
    for T in set get incr; do
      # --csv line: "test","rps","avg_ms","min_ms","p50_ms","p95_ms","p99_ms","max_ms"
      LINE=$(redis-benchmark -h "$HOST" -p "$PORT" "${AUTH_ARG[@]+"${AUTH_ARG[@]}"}" \
              -t "$T" -n "$REQUESTS" -c "$CLIENTS" -P "$P" -d "$D" -q --csv 2>/dev/null | tail -1)
      RPS=$(echo "$LINE"   | awk -F',' '{gsub(/"/,"",$2); print $2}')
      AVG=$(echo "$LINE"   | awk -F',' '{gsub(/"/,"",$3); print $3}')
      P50=$(echo "$LINE"   | awk -F',' '{gsub(/"/,"",$5); print $5}')
      P99=$(echo "$LINE"   | awk -F',' '{gsub(/"/,"",$7); print $7}')
      printf "  %-4s d=%-3s P=%-2s -> %s rps (p50=%s p99=%s ms)\n" "$T" "$D" "$P" "$RPS" "$P50" "$P99"
      echo "$LABEL,$T,$D,$P,$CLIENTS,$RPS,$AVG,$P50,$P99" >> "$OUT"
    done
  done
done
echo "  -> appended to $OUT"
