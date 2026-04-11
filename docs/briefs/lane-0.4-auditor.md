# Lane 0.4 Audit Brief — `forgeclaw-ipc`

## Why This Lane Matters

Lane 0.4 is the protocol boundary between the Forgeclaw host and its agent containers. If this crate is weak, every later lane inherits that weakness: container orchestration, agent-runner integration, JS adapters, and security boundaries all sit on top of it.

This is not a style review. Audit it as foundational infrastructure whose mistakes become expensive later.

Read these before auditing:

- `ROADMAP.md`
- `docs/IPC_PROTOCOL.md`
- `docs/CRATE_GUIDE.md`
- `docs/DESIGN_PRINCIPLES.md`
- `HLD.md`
- `CLAUDE.md`

Use the docs as the contract. Do not grade the implementation against personal preference.

## Audit Goal

Determine whether Lane 0.4 actually delivers a production-grade Phase 0 IPC foundation:

- typed protocol messages aligned with the spec
- correct length-prefixed JSON framing
- safe and composable Unix socket host/client plumbing
- credible tests for normal and adversarial cases

## Required Evaluation Dimensions

Evaluate across all of these dimensions:

- completion
- correctness
- security
- performance and efficiency
- elegance of API design
- extensibility for later lanes
- long-term stability and maintenance risk
- test quality

## What To Check

### 1. Spec conformance

Verify the Rust types and wire behavior actually match `docs/IPC_PROTOCOL.md`.

Focus on:

- message discriminators and field names
- required vs optional fields
- nested payload structure
- handshake semantics
- max frame size
- lifecycle assumptions around `ready`, `init`, `messages`, `shutdown`, and terminal behavior

Flag any undocumented divergence. If divergence exists, explain whether it is benign, risky, or a spec violation.

### 2. Framing correctness

Inspect the codec carefully.

Look for:

- exact 4-byte big-endian length handling
- off-by-one or truncation bugs
- malformed-frame behavior
- oversized frame rejection
- partial read/write handling
- clear treatment of non-UTF-8 and invalid JSON payloads

Do not assume helper crates got this right. Verify the effective behavior.

### 3. Connection lifecycle quality

Review the host/client socket abstraction and handshake flow.

Look for:

- clear ownership of bind/connect/accept lifecycle
- correct cleanup behavior
- predictable shutdown semantics
- no hidden deadlock or indefinite-wait paths
- no API shape that will force later lanes into awkward rewrites

### 4. Security posture

This crate handles untrusted bytes from a container boundary. Audit accordingly.

Look for:

- payload-size abuse
- logging of raw untrusted payloads or oversized content
- path handling mistakes around Unix sockets
- trusting message content for identity or authorization decisions
- error messages that could leak more than intended

### 5. Extensibility

Judge whether the implementation preserves the "protocol-first polyglot" principle.

Look for:

- protocol types that are too Rust-specific to serve as a stable contract
- tight coupling to future container-manager behavior
- abstractions that make JS/other adapters harder later
- brittle assumptions that block additive protocol evolution

### 6. Tests

Audit the tests as seriously as the implementation.

Look for:

- full message coverage, not just one or two examples
- malformed/truncated/oversized frame coverage
- handshake and disconnect lifecycle coverage
- deterministic behavior
- tests that would actually catch regressions rather than simply replay the implementation

## Findings Format

Every issue you raise must include:

- severity
- file and line reference
- concrete evidence from the implementation
- the failure mode or long-term risk
- a specific fix direction

Do not give vague impressions. Give actionable findings.

If you think something is unusual but ultimately acceptable, say so explicitly and explain why it still passes.

## Verdict

End with exactly one of:

- `PASS`
- `PASS WITH ISSUES`
- `NEEDS WORK`
- `REJECT`

Interpretation:

- `PASS`: ready to merge as-is
- `PASS WITH ISSUES`: acceptable to merge, but improvements should be tracked
- `NEEDS WORK`: materially incomplete or risky; should be remediated before merge
- `REJECT`: fundamentally misaligned with the spec or design principles

## High-Risk Failure Modes To Keep In Mind

- framing bugs that only show up under partial I/O
- silent acceptance of malformed messages
- API choices that force a future breaking rewrite in `container` or `agent-runner`
- versioning choices that undermine the protocol-first principle
- tests that miss the exact cases likely to break cross-language adapters
- error or log output that reflects arbitrary container-supplied payloads unsafely

Be demanding. This crate is Phase 0 infrastructure, not a disposable prototype.
