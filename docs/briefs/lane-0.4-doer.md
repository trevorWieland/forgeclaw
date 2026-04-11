# Lane 0.4 Doer Brief — `forgeclaw-ipc`

## Intent

Lane 0.4 builds the host-container protocol boundary for Forgeclaw. This crate is not just an internal Rust helper: it is the stable contract that future container, agent-runner, and polyglot adapters depend on.

Implement the `forgeclaw-ipc` crate so Phase 0 ends with a typed, tested IPC foundation: protocol message types, length-prefixed JSON framing, and minimal Unix socket host/client plumbing with a correct handshake lifecycle.

Do not treat this as "just serialize some enums." The protocol boundary is load-bearing for the project.

## Read First

These docs are the source of truth. Reference them; do not duplicate or reinterpret them casually.

- `ROADMAP.md`
  Use Lane 0.4 and Phase 0 exit criteria as the scope boundary.
- `docs/IPC_PROTOCOL.md`
  This is the protocol spec. Message shapes, framing, lifecycle, transport, max frame size, and error handling live here.
- `docs/CRATE_GUIDE.md`
  Confirms the `ipc` crate responsibility, API surface, and expected test surface.
- `docs/DESIGN_PRINCIPLES.md`
  Pay particular attention to:
  - Protocol-First Polyglot
  - Compile-Time Correctness
  - Security Through Isolation, Not Trust
  - Fail Explicitly, Recover Automatically
- `HLD.md`
  Re-read the `ipc` crate section and the message-processing flow to stay aligned with the overall architecture.
- `CLAUDE.md`
  Honor all workspace conventions and quality rules.

## Scope

Implement Lane 0.4 in `crates/ipc/`.

Expected deliverables from the roadmap:

- Protocol message types:
  - `HostToContainer`
  - `ContainerToHost`
- Frame codec:
  - 4-byte big-endian length prefix
  - UTF-8 JSON payload
  - explicit max frame size enforcement
- Unix socket server on the host side
- Unix socket client on the container side
- Handshake flow:
  - container connects
  - container sends `Ready`
  - host sends `Init`
- Unit/integration-style tests for:
  - codec roundtrip
  - message coverage
  - connection lifecycle
  - malformed frame handling

Phase 0 is still foundation-only. Do not pull in container orchestration behavior, provider logic, or agent execution logic that belongs to later lanes.

## Non-Negotiable Constraints

- Base from `origin/main`, not from the old `lane-0.3` branch tip.
- `just ci` must pass locally before handoff.
- Use stable Rust 2024 only.
- `unsafe` is forbidden.
- No inline `#[allow(...)]` or `#[expect(...)]`.
- No `unwrap!`, `panic!`, `todo!`, `dbg!`, `println!`, or `eprintln!`.
- `thiserror` in library code; do not introduce `anyhow` here.
- Public types need doc comments.
- Keep `.rs` files under 500 lines and functions under 100 lines.
- Tests must run through the existing workspace flow (`cargo nextest` via `just ci`), not ad hoc `cargo test`.

## Design Decisions You Must Make and Justify

Make these decisions explicitly in your PR description or handoff notes.

### 1. Protocol type modeling

Decide how to represent the protocol so it is:

- faithful to `docs/IPC_PROTOCOL.md`
- strongly typed in Rust
- easy to evolve additively in future lanes
- friendly to future polyglot adapters

Questions to resolve:

- How much structure lives in nested payload structs vs flat enum variants?
- How do you encode message discriminators cleanly with serde?
- Where do protocol-version concepts live?
- What compatibility posture do you want around unknown fields or future additive fields?

### 2. Framing implementation

Choose the framing abstraction carefully.

Questions to resolve:

- Do you use `tokio-util`’s length-delimited support directly, or a thin wrapper around it, or a fully custom codec?
- Where is max-frame-size enforcement applied?
- How do you keep malformed-frame behavior explicit and testable instead of hidden in helper layers?

The implementation should make it obvious that the wire contract is "4-byte big-endian length prefix + JSON payload", not an incidental consequence of helper defaults.

### 3. Host/client API shape

Design the `IpcServer`, `IpcClient`, and any channel/session abstractions so later lanes can consume them without tearing them back apart.

Questions to resolve:

- What object owns the socket path and bind lifecycle?
- What object represents an accepted connection?
- How do send/receive APIs expose typed messages without leaking transport details into later crates?
- How do you keep the handshake lifecycle testable without prematurely embedding container-manager policy?

### 4. Error boundaries

Decide what the crate’s own errors look like and what they intentionally hide or preserve.

Questions to resolve:

- How do you distinguish protocol errors, transport errors, oversize frames, malformed JSON, and version mismatch?
- Which errors are recoverable by callers and which imply connection teardown?
- Are error messages safe to log without echoing untrusted payloads in full?

### 5. Test strategy

Choose a test structure that proves the crate is a stable foundation, not just that happy-path examples work.

At minimum, think through:

- roundtrip coverage for every message type
- malformed or truncated frames
- oversize frames
- handshake success path
- disconnect/close behavior
- protocol-version mismatch behavior, if supported in the implementation

If property tests materially improve confidence in framing or message roundtrips, use them.

## Guidance

- Keep the protocol crate focused. This lane should not invent the full container state machine or agent runtime.
- Prefer small, named protocol structs over anonymous JSON-ish maps.
- Keep the public API unsurprising. Later crates should be able to depend on this without intimate knowledge of implementation details.
- Favor explicit teardown and lifecycle behavior over "drop probably handles it."
- If the spec and current roadmap wording appear to differ in a small way, follow the spec and document the reconciliation clearly.

## Deliverables Checklist

- `crates/ipc` has concrete dependencies and a real implementation.
- Protocol message enums and supporting structs are defined and documented.
- Serialization shape matches `docs/IPC_PROTOCOL.md`.
- Frame codec implements the documented wire format and rejects oversized frames.
- Host-side Unix socket server exists.
- Container-side Unix socket client exists.
- Handshake path is implemented and tested.
- Error types are explicit and crate-local.
- Tests cover all protocol message types plus lifecycle and malformed-input cases.
- Public API docs are present and useful.

## Acceptance Criteria

This lane is ready for audit when all of the following are true:

- `just ci` passes locally.
- `forgeclaw-ipc` compiles cleanly under the workspace lint regime.
- All documented message types in `docs/IPC_PROTOCOL.md` are represented in Rust types or a deliberate, documented omission exists with strong justification.
- The framing layer is demonstrably length-prefixed JSON with enforced max size.
- Tests prove codec roundtrip correctness and socket lifecycle correctness.
- The implementation is narrow, composable, and obviously reusable by future `container` and `agent-runner` work.

## Out of Scope

- Container runtime integration
- Warm pool behavior
- Provider proxy integration
- Agent execution or tool running
- Channel routing
- Tanren dispatches

Keep the crate foundational and protocol-centric.
