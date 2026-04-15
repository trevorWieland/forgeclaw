# IPC Decode Benchmarks

Run:

```bash
cargo bench -p forgeclaw-ipc --bench decode
```

This benchmark suite exercises representative decode traffic classes:

- Known valid frames at multiple payload sizes (`ready`, `output_delta`, `output_complete`)
- Unknown-type frames (forward-compat path)
- Malformed known-type frames
- Invalid UTF-8 frames

Use these outputs as a before/after baseline when changing codec decode behavior.
