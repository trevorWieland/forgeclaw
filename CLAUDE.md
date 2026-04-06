# Forgeclaw — Project Conventions

## Architecture

Forgeclaw is a compose-native AI runtime — a Rust Cargo workspace with 12 library crates, a main binary, and an agent-runner binary. See the design docs for full context:

- `MOTIVATIONS.md` — why this project exists
- `HLD.md` — high-level design and system architecture
- `ROADMAP.md` — phased build plan with exit criteria
- `docs/` — detailed specs for each subsystem

## Workspace Structure

All crates live under `crates/`:
- Library crates: `core`, `providers`, `channels`, `container`, `ipc`, `auth`, `store`, `scheduler`, `router`, `queue`, `tanren`, `health`
- Binary crates: `forgeclaw` (main), `agent-runner`

Dependency versions are pinned in the root `Cargo.toml` via `[workspace.dependencies]`. Crates reference them with `dep.workspace = true`.

## Rust Conventions

- **Edition**: 2024
- **Error handling**: `thiserror` in library crates, `anyhow` only in binary crates (`forgeclaw`, `agent-runner`)
- **Unsafe code**: Forbidden workspace-wide (`unsafe_code = "forbid"`)
- **No panics**: `unwrap()`, `panic!()`, `todo!()`, `unimplemented!()` are all denied
- **No debug output**: `println!()`, `eprintln!()`, `dbg!()` are denied — use `tracing` instead

## Quality Rules

### No inline lint suppression
`#[allow(...)]` and `#[expect(...)]` are prohibited in source files. If a lint needs to be relaxed:
1. Add it to the crate's `[lints.clippy]` section in its `Cargo.toml`
2. This is a project-level decision, not an inline one

### File size limits
- **Max 500 lines per `.rs` file** — split into modules when approaching the limit
- **Max 100 lines per function** — enforced by clippy `too_many_lines`

### Testing
- Run tests with `cargo nextest run`, not `cargo test`
- Use `insta` for snapshot testing (serialization formats, error messages)
- Use `proptest` for property-based testing (state machines, codecs)

## Task Runner

We use [`just`](https://github.com/casey/just) instead of `make`. Reasons:
- No tab-vs-space footguns — just uses normal indentation
- No `$$` escaping — shell variables work naturally
- Native recipe arguments — `just test --filter foo` works directly
- Cross-platform — runs natively on Linux, macOS, and Windows
- Built-in `just --list` — self-documenting commands

Install: `cargo binstall just` (or `brew install just` on macOS)

## Commands

```sh
just bootstrap   # Install all tooling (first-time setup)
just install     # Fetch deps + build
just ci          # Run full CI check locally (do this before committing)
just test        # Run tests (supports args: just test -- --filter foo)
just lint        # Run clippy
just fmt         # Check formatting
just fmt-fix     # Auto-fix formatting
just coverage    # Generate coverage report
just deny        # Audit dependencies
just --list      # Show all available commands
```
