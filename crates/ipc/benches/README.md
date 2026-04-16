# IPC Perf Harness

The IPC crate has two complementary perf surfaces:

1. **Deterministic regression gate** — `crates/ipc/tests/perf_regression.rs`.
   Runs under `cargo nextest`, so it's included automatically in `just ci`'s
   `test` step (the committed CI gate). Asserts:
   - **Functional invariants** (hardware-independent): buffer reclaim
     after a 1 MiB frame, allocation-free reject path for oversized
     length headers, allocation-free reject path for zero-length frames.
   - **Wall-clock ceilings** (CI-noise-aware): per-iteration decode
     budgets for `ready`, `output_delta` (1 KiB, 64 KiB),
     `output_complete`, `heartbeat`, and the adversarial unknown-type +
     malformed-known 256 KiB paths. Ceilings are 5× local best-of-5;
     tightened to 3× when `$CI=true`.

2. **Criterion microbenchmarks** — `crates/ipc/benches/decode.rs` and
   `crates/ipc/benches/buffer_retention.rs`. Run locally for absolute
   numbers and before/after comparisons. Not wired into CI (criterion
   wall-clock compare is too flaky for a gate); used for tuning.

## Running

```sh
# Deterministic gate (same as CI):
just bench-gate              # nextest + bench --no-run
cargo nextest run -p forgeclaw-ipc --test perf_regression

# Criterion micro-benches, local only:
cargo bench -p forgeclaw-ipc --bench decode
cargo bench -p forgeclaw-ipc --bench buffer_retention
```

## What it catches

- Decode latency regressions on the happy path.
- Adversarial-path cost blowups (unknown type, malformed known).
- Buffer retention regressions (a 1 MiB frame that pins capacity
  forever).
- Oversize-length rejection that accidentally allocates.

## What it does **not** catch

Throughput under contention, multi-task runtime scheduling, or socket
I/O — those need end-to-end benchmarks which live outside this crate.
