# Benchmarks

Measured with `bench/run.sh`. Raw output is in [`bench/results/`](../bench/results/).

## Setup (2026-11-11)

| | |
|---|---|
| CPU | AMD Ryzen 5 7500F, 6 cores / 12 threads, desktop under normal background load |
| Kernel | Linux 7.3.3-1-cachyos |
| nirvor | 0.1.0, full pipeline: ACL, blocklist check, query log (anonymized), metrics. Per-client rate limit and RRL off |
| Unbound | 1.26.2, `module-config: iterator`, forwarding, `serve-expired: yes` |
| Pinning | Each server gets 4 threads on CPUs 0–3. The load generator runs on CPUs 4–11 |
| Load | `nirvor-bench` (a dnsperf-style UDP generator in `crates/bench`): 64 queries in flight, 200 queries (100 popular global and Bangladeshi domains × A/AAAA), 10 s per trial, 3 trials alternating servers, after a warm-up pass |
| Upstreams | Both servers forward to 1.1.1.1 and 8.8.8.8 |

dnsperf could not be built on the test machine, because its `libck` dependency is not packaged. `run.sh` uses dnsperf automatically when it is installed.

## Results: warm cache

| | Throughput (median of 3) | p50 | p95 | p99 | Lost |
|---|---|---|---|---|---|
| nirvor | **103,912 qps** (99.8k–107.7k) | 0.044 ms | 0.127 ms | 2.0–3.2 ms | 0 |
| Unbound | **148,725 qps** (119.5k–151.9k) | 0.027 ms | 0.087 ms | 2.0–4.7 ms | 0 |

nirvor reaches about **70%** of Unbound's warm-cache throughput on the same cores.

- **Tail latency.** The p99 of both is about 2 ms. That comes from the few names with TTLs of 0–10 s (e.g. `grameenphone.com` TTL 0, `paypal.com` TTL 1) that must go upstream during the run.
- **Small ISPs.** 100k qps on 4 cores is far above what a small ISP needs. Typical peaks are 2–20k qps.

## What changed during development

| Version | Change | nirvor qps | Note |
|---|---|---|---|
| Phase 2 | First cache | 103k | 136 lost queries: concurrent misses on short-TTL names flooded upstreams |
| Phase 2 | Request coalescing, ms-precision prefetch | 103k | Losses fixed |
| Phase 3 | Adaptive timeouts | 94k | 0 lost. p99 matches Unbound |
| Phase 7 | Full pipeline (ACL, policy, query log) | 104k | Answering cached queries inline, plus SO_REUSEPORT, made up for the extra work |

## Possible improvements (not done)

- A wire-format fast path for cache hits: patch the ID and TTLs in stored bytes instead of re-encoding the message.
- Lock-free query-log and top-N counters (per-thread shards).

## How to reproduce

```sh
sudo pacman -S unbound          # or your distribution's package
bench/run.sh                    # SECONDS_RUN, CONCURRENCY, THREADS, TRIALS can be overridden
```

The numbers depend on the machine. Desktop background load moved single runs by ±30% before CPU pinning was added, so compare medians from the same session.
