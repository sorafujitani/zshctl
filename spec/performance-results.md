# Performance results

Captured on 2026-08-17 with the release profile at source working tree state,
Rust 1.85.0, macOS 14.7.3 (Darwin 23.6.0), and Apple M3 Pro. The exact hyperfine
samples and medians are checked in under `benchmark-results/`; rerun with
`ZSHCTL_BIN=$PWD/target/release/zshctl scripts/benchmark.sh`.

| Workload | Definition | p50 / RSS | Budget | Result |
| --- | --- | ---: | ---: | --- |
| Shell startup | `zsh -dfc` sources `zshctl.zsh` | 7.15 ms | 25 ms | pass |
| Cold request | stopped daemon through healthy start | 29.76 ms | 250 ms | pass |
| Warm request | status against a healthy daemon | 3.07 ms | 10 ms | pass |
| Daemon idle memory | RSS after health | 3.72 MiB | 30 MiB | pass |
| Large history query | newest 1,000 distinct commands from 100,000 SQLite rows | 65.59 ms | 100 ms | pass |

These are local reference results, not universal performance claims. The
benchmark script enforces the release budgets and records the hardware, Rust
version, dataset size, and cold/warm definitions in `summary.json`.
