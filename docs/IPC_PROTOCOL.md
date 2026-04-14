# IPC Protocol Specification

Version: 1.0-draft

## Overview

The IPC protocol defines bidirectional communication between the Forgeclaw host process and agent containers. It is the most important interface in the system — the stable contract that enables polyglot agent adapters.

## Transport

**Unix Domain Socket**: The host creates a Unix socket at a deterministic path inside each container's workspace. The container connects to this socket on startup.

**Socket Path**: `/workspace/ipc.sock` (mounted as a volume by the host)

**Connection Model**: One socket per container. The host listens; the container connects. The connection lifetime equals the container session lifetime.

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
| `job_id` | string | yes | Correlates to the job from `init` or `messages` |

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
| `job_id` | string | yes | Correlates to the originating job |
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
| `context` | object | yes | Message history, group info, timezone |
| `config` | object | yes | Provider proxy URL/token, model, limits |

#### `messages`

Follow-up messages that arrived while the container was processing. Allows the agent to incorporate new context without restarting.

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

## Versioning

The `protocol_version` field in `ready` allows the host to detect adapter capability:

- **1.0**: Base protocol (this document)
- Future versions add new message types or fields. Existing fields are never removed or retyped (additive-only changes within a major version).

The host rejects connections from adapters with an unsupported major version.

## Error Handling

- **Malformed frame** (invalid length, non-UTF8, non-JSON): Host logs the error and closes the socket. Container is transitioned to `Failed`.
- **Unknown message type**: Ignored with a warning log, up to 32 consecutive unknown frames per connection. If a peer exceeds this limit without sending a recognized message, the connection is closed. This allows forward-compatible adapters to send experimental messages while bounding the resource cost of a misbehaving or malicious peer.
- **Missing required field**: Treated as malformed. Socket closed.
- **Socket disconnect without `output_complete`**: Host treats the in-progress job as failed. Container transitioned to `Exited` with error status.
- **Heartbeat timeout** (no heartbeat for 60 seconds during `Processing`): Host sends `shutdown` with a short deadline, then kills the container if it doesn't respond.

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
