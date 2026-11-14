#!/usr/bin/env bash
# Run the same benchmark suite against Redistill, Redis, and Dragonfly and print
# a side-by-side table. Assumes all three are already running on the given ports
# on this host (see EC2_RUNBOOK.md for how to launch them one at a time, or
# together on distinct ports).
#
# Usage:
#   REDISTILL_PORT=6379 REDIS_PORT=6380 DRAGONFLY_PORT=6381 ./compare.sh
set -euo pipefail
cd "$(dirname "$0")"

REDISTILL_PORT="${REDISTILL_PORT:-6379}"
REDIS_PORT="${REDIS_PORT:-6380}"
DRAGONFLY_PORT="${DRAGONFLY_PORT:-6381}"
HOST="${HOST:-127.0.0.1}"

rm -f results.csv

[ -n "${SKIP_REDISTILL:-}" ] || ./bench.sh "$HOST" "$REDISTILL_PORT" redistill "${REDISTILL_AUTH:-}"
[ -n "${SKIP_REDIS:-}" ]     || ./bench.sh "$HOST" "$REDIS_PORT"     redis     "${REDIS_AUTH:-}"
[ -n "${SKIP_DRAGONFLY:-}" ] || ./bench.sh "$HOST" "$DRAGONFLY_PORT" dragonfly "${DRAGONFLY_AUTH:-}"

echo
echo "================ SUMMARY (rps, higher is better) ================"
# Pivot: one row per (test,payload,pipeline), columns per engine.
python3 - <<'PY'
import csv, collections
rows=collections.defaultdict(dict)
labels=[]
with open("results.csv") as f:
    for r in csv.DictReader(f):
        key=(r["test"], r["payload_bytes"], r["pipeline"])
        rows[key][r["label"]]=(r["rps"], r["p99_ms"])
        if r["label"] not in labels: labels.append(r["label"])
w=14
hdr=f'{"test/d/P":<14}' + "".join(f'{l:>{w}}' for l in labels)
print(hdr); print("-"*len(hdr))
for (t,d,p) in sorted(rows):
    name=f"{t} d{d} P{p}"
    cells=""
    for l in labels:
        v=rows[(t,d,p)].get(l)
        cells += f'{(v[0] if v else "-"):>{w}}'
    print(f'{name:<14}{cells}')
print()
print("p99 latency (ms):")
print(hdr); print("-"*len(hdr))
for (t,d,p) in sorted(rows):
    name=f"{t} d{d} P{p}"
    cells="".join(f'{(rows[(t,d,p)].get(l,["-","-"])[1]):>{w}}' for l in labels)
    print(f'{name:<14}{cells}')
PY
