# Contributing to Forgeclaw

## Getting Started

```sh
git clone https://github.com/trevorWieland/forgeclaw.git
cd forgeclaw
just bootstrap   # installs Rust, cargo tools, git hooks — idempotent
just install     # fetches deps, verifies build
just ci          # runs full local CI check
```

Requires: `just` task runner (`cargo binstall just` or `brew install just`)

## Development Workflow

1. Create a branch from `main`
2. Make your changes
3. Run `just ci` — must pass before pushing
4. Open a PR against `main`
5. All 9 CI checks must pass
6. Squash-merge (only merge strategy enabled)

## Code Quality Rules

These are enforced by CI and pre-commit hooks. PRs that violate them will not merge.

### Linting

- **clippy pedantic** is enabled workspace-wide with strict individual lints
- `unwrap()`, `panic!()`, `todo!()`, `unimplemented!()` are **denied**
- `println!()`, `eprintln!()`, `dbg!()` are **denied** — use `tracing` instead
- `unsafe` code is **forbidden**

### No Inline Lint Suppression

`#[allow(...)]` and `#[expect(...)]` are **prohibited in source files**. This is enforced at compile time (`clippy::allow_attributes = "deny"`) and by CI grep.

If a lint needs to be relaxed for a specific crate, add it to that crate's `[lints.clippy]` section in its `Cargo.toml` with a comment explaining why. This keeps suppression decisions visible at the project level.

### File Size Limits

- **Max 500 lines per `.rs` file** — split into submodules when approaching the limit
- **Max 100 lines per function** — enforced by clippy `too_many_lines`

### Error Handling

- `thiserror` in library crates — typed, structured errors
- `anyhow` only in binary crates (`forgeclaw`, `agent-runner`)
- Never swallow errors silently

### Dependencies

- Versions are pinned in the root `Cargo.toml` via `[workspace.dependencies]`
- Crates reference them with `dep.workspace = true`
- New external deps must use permissive licenses (MIT, Apache-2.0, BSD, ISC, etc.) — `cargo deny check` enforces this
- Minimize feature flags — don't use `features = ["full"]`

## Testing

Run tests with nextest, not `cargo test`:

```sh
just test                    # run all tests
just test -- --filter foo    # filter tests
just coverage                # generate lcov coverage report
```

Use these testing libraries:
- `insta` for snapshot testing (serialization formats, error messages)
- `proptest` for property-based testing (state machines, codecs)
- `wiremock` for HTTP mock servers (provider tests)
- `testcontainers` for integration tests needing Docker (database, container runtime)

## Commands Reference

```sh
just --list       # show all available commands
just bootstrap    # install all tooling (first-time setup)
just install      # fetch deps + build
just ci           # full local CI check
just test         # run tests
just lint         # run clippy
just fmt          # check formatting
just fmt-fix      # auto-fix formatting
just fix          # auto-fix everything (format + clippy)
just deny         # audit dependencies
just coverage     # generate coverage report
just doc          # build documentation
just machete      # detect unused deps
just clean        # remove build artifacts
```

## License

By contributing, you agree that your contributions will be licensed under the MIT OR Apache-2.0 dual license.
