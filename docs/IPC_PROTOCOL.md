# IPC Protocol Specification

Version: 1.0

## Overview

The IPC protocol defines bidirectional communication between the Forgeclaw host process and agent containers. It is the most important interface in the system — the stable contract that enables polyglot agent adapters.

## Transport

**Unix Domain Socket**: The host creates a Unix socket at a deterministic path inside each container's workspace. The container connects to this socket on startup.

**Socket Path**: `/workspace/ipc.sock` (mounted as a volume by the host)

**Connection Model**: One socket per container. The host listens; the container connects. The connection lifetime equals the container session lifetime.

**Host Bind Policy** (Unix hardening):
- The immediate socket parent directory must be mode `0700`.
- The socket file is bound with mode `0600`.
- Socket paths must be absolute and fail-closed normalized; `.` and `..`
  traversal components are rejected.
- Ancestor symlinks are rejected except vetted system aliases:
  - macOS: `/var -> /private/var`, `/tmp -> /private/tmp`
  - Linux: `/var/run -> /run`, `/var/lock -> /run/lock`
- Post-bind attestation is required: parent fingerprint must remain
  stable and listener/path inode fingerprints must match. Any mismatch
  is a fail-closed bind-race error.
- Guaranteed-supported deployment pattern: use a dedicated private host-owned directory and place the socket there.
- Trusted path input is required: callers should treat the bind path as
  host-authored configuration, not untrusted user/container input.

## Framing

All messages are length-prefixed JSON frames:

```
┌──────────────┬──────────────────────────┐
│ Length (4B)   │ JSON Payload (N bytes)   │
│ big-endian u32│                          │
└──────────────┴──────────────────────────┘
```

- **Length**: 4-byte big-endian unsigned integer. Value is the byte length of the JSON payload (not including the length prefix itself).
- **Payload**: UTF-8 encoded JSON object.
- **Maximum frame size**: 10 MiB (10,485,760 bytes). Frames exceeding this limit are rejected with a protocol error.

## Wire Constraints

These constraints are normative for protocol `1.x`. Implementations must enforce them consistently in runtime validation and schema validation.
Runtime enforcement applies on both:
- inbound decode/receive paths, and
- outbound send paths before serialization (invalid payloads are rejected and never written to the wire).

| Field / Structure | Constraint |
|---|---|
| `init.context.messages` | max 256 entries |
| `messages.messages` | max 256 entries |
| `dispatch_self_improvement.payload.scopes` | max 64 entries |
| `dispatch_self_improvement.payload.acceptance_tests` | max 64 entries |
| Core ID fields (`group`, `target_group`, `task_id`, `job_id`, etc.) | non-empty, maxLength 128 |
| `adapter` (AdapterName) | maxLength 128 |
| `adapter_version` (AdapterVersion) | maxLength 128 |
| `protocol_version` (ProtocolVersionText) | `^[0-9]+\.[0-9]+$` numeric major.minor, maxLength 128 |
| `stage` (StageName) | maxLength 128 |
| `sender` (SenderName) | maxLength 128 |
| `name` in `GroupInfo` / `register_group` (GroupName) | maxLength 128 |
| `project` (ProjectName) | maxLength 128 |
| `branch` (BranchName) | maxLength 128; advisory git-ref shape: non-empty, no whitespace, no control chars, no `..`, no leading/trailing `/`, no consecutive `/` |
| `context_mode` (ContextModeText) | maxLength 128 |
| `environment_profile` (EnvironmentProfileText) | maxLength 128 |
| `model` | maxLength 256 |
| token-like fields (`provider_proxy_token`) | maxLength 2048 |
| short freeform fields (`error.message`, `progress.detail`) | maxLength 1024 |
| `schedule_value` | maxLength 512 |
| message text (`historical.text`, `send_message.text`) | maxLength 32768 |
| prompt/objective text (`schedule_task.prompt`, `dispatch_* .prompt`, `dispatch_self_improvement.objective`) | maxLength 32768 |
| `output_delta.text` | maxLength 65536 |
| `output_complete.result` | maxLength 262144 |
| `session_id` | maxLength 128 |
| self-improvement list item text (`scopes[]`, `acceptance_tests[]`) | maxLength 1024 |
| `provider_proxy_url` | absolute `http(s)` URL, maxLength 2048 |
| `register_group.extensions.version` | non-empty (contains `\\S`), maxLength 128 |
| `register_group.extensions` | top-level maxProperties 32 (including `version`) |
| `register_group.extensions` keys (all nesting levels) | maxLength 128 |
| `register_group.extensions` JSON nesting | max depth 8 |
| `register_group.extensions` encoded object size | max encoded bytes 65536 |

For additive `1.x` changes, new fields may be introduced, but documented constraints on existing fields are never loosened silently.

## Message Types

### Container → Host

Every message from the container to the host is a JSON object with a `type` field:

#### `ready`

Sent immediately after the container connects. Signals that the agent adapter has initialized and is ready to receive work.

```json
{
  "type": "ready",
  "adapter": "claude-code",
  "adapter_version": "1.0.0",
  "protocol_version": "1.0"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `type` | `"ready"` | yes | Message discriminator |
| `adapter` | string | yes | Name of the agent adapter |
| `adapter_version` | string | yes | Adapter version |
| `protocol_version` | string | yes | IPC protocol version supported |

#### `output_delta`

Streamed text output from the agent. Sent incrementally as the model generates text.

```json
{
  "type": "output_delta",
  "text": "Here's what I found...",
  "job_id": "job-abc123"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `type` | `"output_delta"` | yes | Message discriminator |
| `text` | string | yes | Incremental text chunk |
| `job_id` | string | yes | Must match the active job from `init` or latest idle-time `messages` rebind |

#### `output_complete`

Signals the agent has finished processing the current job. Contains the final result.

```json
{
  "type": "output_complete",
  "job_id": "job-abc123",
  "result": "Here's the full response...",
  "session_id": "sess-xyz789",
  "token_usage": {
    "input_tokens": 1500,
    "output_tokens": 800
  },
  "stop_reason": "end_turn"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `type` | `"output_complete"` | yes | Message discriminator |
| `job_id` | string | yes | Must match the active originating job |
| `result` | string \| null | yes | Final text output (null if tools-only turn) |
| `session_id` | string \| null | no | Session ID for resume (adapter-specific) |
| `token_usage` | object \| null | no | Token counts for this turn |
| `stop_reason` | string | yes | Why the model stopped: `end_turn`, `max_tokens`, `tool_use` |

#### `progress`

Optional progress update for long-running operations. Not all adapters emit this.

```json
{
  "type": "progress",
  "job_id": "job-abc123",
  "stage": "tool_execution",
  "detail": "Running cargo test...",
  "percent": 60
}
```

#### `command`

IPC commands from the agent to the host system.

```json
{
  "type": "command",
  "command": "send_message",
  "payload": {
    "target_group": "group-main",
    "text": "Task completed successfully."
  }
}
```

**Available commands:**

| Command | Payload | Authorization | IPC Enforcement |
|---------|---------|--------------|-----------------|
| `send_message` | `{ target_group, text }` | Main: any group. Non-main: own group only. | Enforced at IPC boundary |
| `schedule_task` | `{ group, schedule_type, schedule_value, prompt, context_mode? }` | Main: any group. Non-main: own group only. | Enforced at IPC boundary |
| `pause_task` | `{ task_id }` | Main: any. Non-main: own tasks only. | Ownership verified by caller via `OwnershipPending` |
| `cancel_task` | `{ task_id }` | Main: any. Non-main: own tasks only. | Ownership verified by caller via `OwnershipPending` |
| `register_group` | `{ name, extensions? }` | Main only. | Enforced at IPC boundary |
| `dispatch_tanren` | `{ project, branch, phase, prompt, environment_profile? }` | Groups with `tanren` capability only. | Enforced at IPC boundary |
| `dispatch_self_improvement` | `{ objective, scopes, acceptance_tests, branch_policy }` | Main only. | Enforced at IPC boundary |

**Group capabilities**: The `group` object in `init.context` carries a `capabilities` field:

```json
{
  "id": "group-tanren",
  "name": "Tanren Group",
  "is_main": false,
  "capabilities": {
    "tanren": true
  }
}
```

The `capabilities` object is host-authoritative and determines which command families the IPC layer will accept. If omitted, capabilities default to all-false.

##### `register_group` payload

`register_group.payload` is defined as:

```json
{
  "name": "New Group",
  "extensions": {
    "version": "1",
    "trigger": "@bot"
  }
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | yes | Human-readable group name |
| `extensions` | object \| null | no | Optional typed envelope for router-owned extension data |
| `extensions.version` | string | yes when `extensions` is present | Schema version for extension compatibility |

`extensions` must be a JSON object when provided. Arrays/scalars are invalid.
`extensions.version` is a required wire-contract field whenever `extensions` is present.
`extensions.version` must be non-empty (contains at least one non-whitespace character) and maxLength 128.
The `extensions` envelope is bounded for abuse resistance:
- maxProperties 32 at the top level (including `version`),
- key maxLength 128 (at all nesting levels),
- max depth 8 for nested JSON values,
- max encoded bytes 65536 for the full `extensions` object.
JSON Schema publishes this encoded-size contract as extension metadata:
`x-maxEncodedBytes: 65536`.
All supported runtime versions enforce this requirement.

#### `error`

Reports an error from the agent adapter.

```json
{
  "type": "error",
  "code": "provider_error",
  "message": "Model returned 429: rate limited",
  "fatal": false,
  "job_id": "job-abc123"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `code` | string | yes | Error classification |
| `message` | string | yes | Human-readable description |
| `fatal` | boolean | yes | If true, container should be shut down |
| `job_id` | string \| null | no | Which job this error relates to |

**Error codes:**
- `provider_error` — Model provider returned an error
- `tool_error` — Tool execution failed
- `adapter_error` — Agent adapter internal error
- `protocol_error` — Malformed message from host
- `timeout` — Operation exceeded adapter-side timeout

#### `heartbeat`

Periodic liveness signal. The host expects heartbeats at least every 30 seconds during processing.
`timestamp` must be an RFC3339 string.

```json
{
  "type": "heartbeat",
  "timestamp": "2026-04-03T10:30:00Z"
}
```

### Host → Container

#### `init`

Sent after receiving `ready`. Provides the initial context and configuration for processing.

```json
{
  "type": "init",
  "job_id": "job-abc123",
  "context": {
    "messages": [
      { "sender": "Alice", "text": "Hey @bot, what's the weather?", "timestamp": "2026-04-03T10:00:00Z" },
      { "sender": "Bob", "text": "Also curious!", "timestamp": "2026-04-03T10:01:00Z" }
    ],
    "group": {
      "id": "group-main",
      "name": "Main Group",
      "is_main": true
    },
    "timezone": "America/New_York"
  },
  "config": {
    "provider_proxy_url": "http://host.docker.internal:9090/v1",
    "provider_proxy_token": "abc123def456",
    "model": "claude-sonnet-4-20250514",
    "max_tokens": 32000,
    "session_id": "sess-xyz789",
    "tools_enabled": true,
    "timeout_seconds": 1800
  }
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `type` | `"init"` | yes | Message discriminator |
| `job_id` | string | yes | Unique job identifier for correlation |
| `context` | object | yes | Message history, group info, timezone (IANA name validated at IPC boundary) |
| `config` | object | yes | Provider proxy URL/token, model, limits |

#### `messages`

Follow-up messages that arrived while the container was processing. Allows the agent to incorporate new context without restarting.
While `Processing`, `messages.job_id` must match the active job. While `Idle`, it may rebind the active job.

```json
{
  "type": "messages",
  "job_id": "job-abc123",
  "messages": [
    { "sender": "Alice", "text": "Also check the logs please", "timestamp": "2026-04-03T10:05:00Z" }
  ]
}
```

#### `shutdown`

Request graceful shutdown. The container should finish current work (if possible within deadline), send `output_complete`, and exit.

```json
{
  "type": "shutdown",
  "reason": "idle_timeout",
  "deadline_ms": 10000
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `reason` | string | yes | Why: `idle_timeout`, `host_shutdown`, `eviction`, `error_recovery` |
| `deadline_ms` | integer | yes | Milliseconds to finish. After deadline, host kills the container. |

## Lifecycle

```
Host                              Container
  │                                   │
  │◄──── [connect to socket] ─────────│
  │                                   │
  │◄──── ready ───────────────────────│
  │                                   │
  │───── init ───────────────────────►│
  │                                   │
  │◄──── output_delta ────────────────│  (repeated)
  │◄──── output_delta ────────────────│
  │◄──── command ─────────────────────│  (optional, interleaved)
  │◄──── heartbeat ───────────────────│  (periodic during processing)
  │◄──── output_complete ─────────────│
  │                                   │
  │  (container now idle, may receive │
  │   more work or shutdown)          │
  │                                   │
  │───── messages ───────────────────►│  (optional: follow-up context)
  │                                   │
  │◄──── output_delta ────────────────│
  │◄──── output_complete ─────────────│
  │                                   │
  │───── shutdown ───────────────────►│
  │                                   │
  │◄──── output_complete (if pending)─│
  │                                   │
  │◄──── [socket close] ─────────────│
  │                                   │
```

The host enforces post-handshake phases at the IPC boundary:

- **Processing**: default immediately after `init`; container may emit streaming/progress/commands/heartbeat/error and must eventually emit `output_complete`.
- **Idle**: entered after `output_complete`; container must not continue streaming or sending commands until the host sends new `messages`.
- **Draining**: entered after host `shutdown`; host rejects new `messages`, and container may only finish/heartbeat/error before close.
- On the first `output_complete` observed while draining, IPC transitions
  to terminal closed state and closes the transport immediately.

Out-of-phase traffic is treated as a protocol error and the connection is closed.

## Versioning

The `protocol_version` field in `ready` allows the host to detect adapter capability:

- **1.0**: Baseline protocol. `register_group.extensions.version` is
  required whenever `extensions` is present.
- Future versions add new message types or fields. Existing fields are never removed or retyped (additive-only changes within a major version).

The host rejects connections from adapters with an unsupported major version.
After handshake, the host persists a negotiated runtime version
(`major` unchanged, `minor = min(local, peer)`) per connection so
minor-aware behavior gates can remain explicit and centralized.

## Error Handling

- **Malformed frame** (invalid length, non-UTF8, non-JSON): Host emits a structured error log at the protocol boundary (including peer/group identity and error class, never raw payload bytes), then closes the socket. Container is transitioned to `Failed`.
- **Unknown message type**: Ignored with a warning log for forward compatibility, up to both:
  - 32 consecutive unknown frames, and
  - 1 MiB cumulative bytes across the current consecutive-unknown streak.
  On either limit breach, the host closes the connection. Any recognized message resets both unknown counters.
  In addition, host/client implementations enforce independent non-consecutive abuse controls by default:
  - total unknown-frame cap per connection lifetime,
  - total unknown-byte cap per connection lifetime, and
  - unknown-frame token-bucket rate limiting.
  These controls are configurable and can be explicitly disabled for testing.
- **Missing required field**: Treated as malformed. Socket closed.
- **Job ID mismatch** (job-scoped message whose `job_id` does not match the active job): Treated as protocol violation. Socket closed.
- **Socket disconnect without `output_complete`**: Host treats the in-progress job as failed. Container transitioned to `Exited` with error status.
- **Heartbeat timeout** (no heartbeat for 60 seconds during `Processing`): Host sends `shutdown` with a short deadline, then kills the container if it doesn't respond.
- **Idle read timeout** (no complete inbound frame for 60 seconds during `Idle`): Host closes the connection with an `idle_read_timeout` protocol error. This guard also applies to trickled partial frames that never complete.
- **Blocked post-handshake write** (peer not reading): sender applies a
  per-connection write deadline (default 5 seconds). Timeout is fatal:
  connection is poisoned and closed.
- Host outbound `messages` traffic does not reset the heartbeat timer;
  only inbound container liveness signals (`heartbeat`) refresh the
  processing heartbeat deadline.
- **Lifecycle violation** (valid message at wrong phase): Host logs the violation and closes the connection.
- **Unauthorized command abuse**: Single unauthorized commands are rejected and logged. Every unauthorized rejection is recorded at structured audit/debug level; warn/error channels may still be sampled for operator-noise control. Repeated unauthorized commands are connection-bounded with per-connection token-bucket controls (default: burst 8, refill 2/sec, 200ms backoff, disconnect after 5 exhausted-budget strikes).
- **Peer credential policy rejection**: Host may fail closed at accept
  time if captured OS peer credentials do not satisfy configured policy
  (for example strict UID/GID in hardened mode).

## Implementing an Adapter

An adapter is a program that:

1. Connects to the Unix socket at `/workspace/ipc.sock`
2. Sends a `ready` message
3. Receives `init` with context and config
4. Runs an agent loop (call the model, execute tools, generate output)
5. Streams `output_delta` messages as output is generated
6. Sends `output_complete` when done
7. Optionally handles `messages` for follow-up context
8. Handles `shutdown` gracefully

The adapter surface is intentionally minimal. The complexity lives in the agent runtime (Claude Code, OpenAI Agents SDK, custom harness), not in the adapter.

Reference implementations:
- **Rust**: `crates/ipc/` (host + client, with full auth matrix)
- **TypeScript**: `ipc-adapter-js/src/index.ts` (planned — JSON Schemas at `crates/ipc/schemas/` enable type-safe generation)
