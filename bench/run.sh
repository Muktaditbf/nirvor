#!/usr/bin/env bash
# Benchmarks nirvor against Unbound on this machine, both forwarding to the same upstreams.
# Measures the warm-cache path: a warm-up pass first, then a timed run per server.
# Uses dnsperf when installed, otherwise the bundled nirvor-bench load generator.
set -euo pipefail
cd "$(dirname "$0")/.."

SECONDS_RUN=${SECONDS_RUN:-20}
CONCURRENCY=${CONCURRENCY:-64}
THREADS=${THREADS:-4}          # server threads, pinned to CPUs 0..THREADS-1
LOAD_CPUS=${LOAD_CPUS:-$THREADS-$(($(nproc)-1))}  # load generator gets the rest
TRIALS=${TRIALS:-3}
OUT=bench/results/$(date +%Y-%m-%d).md
WORK=$(mktemp -d)
trap 'kill $(jobs -p) 2>/dev/null || true; rm -rf "$WORK"' EXIT

cargo build --release -q -p nirvor -p nirvor-bench
sed "s/NUM_THREADS/$THREADS/" bench/unbound.conf > "$WORK/unbound.conf"

TOKIO_WORKER_THREADS=$THREADS taskset -c 0-$((THREADS-1)) ./target/release/nirvor -c bench/nirvor-bench.toml > "$WORK/nirvor.log" 2>&1 &
taskset -c 0-$((THREADS-1)) unbound -c "$WORK/unbound.conf" > "$WORK/unbound.log" 2>&1 &
sleep 2

load() { # server-addr seconds
  if command -v dnsperf >/dev/null; then
    taskset -c "$LOAD_CPUS" dnsperf -s "${1%:*}" -p "${1##*:}" -d bench/queries.txt -l "$2" -q "$CONCURRENCY"
  else
    taskset -c "$LOAD_CPUS" ./target/release/nirvor-bench -s "$1" -d bench/queries.txt -l "$2" -c "$CONCURRENCY"
  fi
}

{
  echo "# Benchmark $(date -Iseconds)"
  echo
  echo "Warm-cache throughput, both servers forwarding to 1.1.1.1 and 8.8.8.8 on the same machine."
  echo
  echo "| | |"
  echo "|---|---|"
  echo "| CPU | $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2 | xargs), $(nproc) threads |"
  echo "| Pinning | each server: $THREADS threads on CPUs 0-$((THREADS-1)); load generator on CPUs $LOAD_CPUS |"
  echo "| Kernel | $(uname -r) |"
  echo "| nirvor | $(./target/release/nirvor --version) |"
  echo "| Unbound | $(unbound -V | head -1) (num-threads $THREADS) |"
  echo "| Load tool | $(command -v dnsperf >/dev/null && dnsperf -h 2>&1 | head -1 || echo nirvor-bench) |"
  echo "| Queries | $(grep -vc '^#' bench/queries.txt) queries in bench/queries.txt, ${CONCURRENCY} in flight, ${SECONDS_RUN} s per trial, $TRIALS trials alternating servers |"
  echo
  load 127.0.0.1:5300 5 > /dev/null   # warm-up: fills both caches
  load 127.0.0.1:5301 5 > /dev/null
  for trial in $(seq "$TRIALS"); do
    for target in "nirvor 127.0.0.1:5300" "unbound 127.0.0.1:5301"; do
      set -- $target
      echo "### $1, trial $trial"
      echo
      echo '```'
      load "$2" "$SECONDS_RUN"
      echo '```'
      echo
    done
  done
} | tee "$OUT"
echo "Saved to $OUT"
