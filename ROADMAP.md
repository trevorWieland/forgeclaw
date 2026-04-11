# Forgeclaw: Roadmap

## Build Philosophy

Each phase produces a working system. Phase N+1 adds capability on top of Phase N — it doesn't replace it. Phases contain independent lanes that can be developed in parallel and integrated at phase boundaries.

Exit criteria are concrete and testable. A phase is done when its criteria pass, not when all possible polish is applied.

## Phase 0 — Foundation

**Goal**: Compiling workspace with core types, event bus, database schema, and IPC protocol types. No runtime behavior.

### Lanes

| Lane | Crate | Deliverable | Status |
|------|-------|------------|--------|
| 0.1 | workspace | Cargo workspace with 14 crate stubs under `crates/`. Workspace manifest with shared lints, deps, and metadata. CI pipeline (9 GitHub Actions jobs). `justfile` with bootstrap and all dev recipes. License files, CONTRIBUTING.md, CLAUDE.md. | **Done** |
| 0.2 | `core` | Event enum (`MessageEvent`, `ContainerEvent`, `TanrenEvent`, `TaskEvent`, `HealthEvent`, `IpcEvent`). Event bus (tokio broadcast channel with typed wrapper). Error taxonomy (`ErrorClass`: Transient, Auth, Config, Container, Fatal). Config types (deserialized from TOML via serde). `GroupId`, `ContainerId`, `ProviderId`, `ChannelId` newtypes. | **Done** |
| 0.3 | `store` | sqlx setup with SQLite and Postgres support. Schema migrations (sqlx-migrate). Tables: `messages`, `chat_metadata`, `tasks`, `task_run_logs`, `router_state`, `sessions`, `registered_groups`, `events`. Compile-time checked queries for all CRUD operations. DataStore trait with sqlx implementation. | **Done** |
| 0.4 | `ipc` | Protocol message types: `HostToContainer` and `ContainerToHost` enums. Frame codec: 4-byte length prefix + JSON payload. Unix socket server (host side) and client (container side) with tokio. Handshake: container connects, sends `Ready`, host sends `Init`. Unit tests for codec roundtrip, connection lifecycle. | **In Progress** |

### Exit Criteria

- `just ci` passes locally (fmt, clippy, deny, check-lines, check-suppression, nextest, doc, machete)
- GitHub Actions CI green on all 9 jobs
- Database migrations run against both SQLite and Postgres
- IPC codec roundtrip tests pass for all message types (including proptest fuzzing)
- Event bus emit/subscribe tests pass
- Config deserialization roundtrip tests pass for a sample `forgeclaw.toml`

## Phase 1 — Container Lifecycle

**Goal**: Spawn a real Docker container, establish IPC, exchange messages, clean shutdown. No AI models, no channels — just the raw container engine.

### Lanes

| Lane | Crate | Deliverable |
|------|-------|------------|
| 1.1 | `container` | Container state machine: `Provisioning → Starting → Ready → Processing → Streaming → Idle → Draining → Exited` (plus `Failed`). Per-state timeouts. Transition validation (invalid transitions return errors). Docker runtime via bollard: create, start, stop, remove, list, health. `ContainerSpec` builder: image, mounts, network, env vars, resource limits (memory, CPU). Mount security: allowlist validation, symlink resolution, blocked patterns. |
| 1.2 | `auth` | Multi-provider credential proxy: hyper HTTP server. Per-container token registration/deregistration. Request routing: extract container token → resolve group → resolve provider → inject credentials → forward. Rate limiting: sliding window per container. Circuit breaker: track failures per provider, auto-pause on auth errors. Health endpoint for proxy itself. |
| 1.3 | `queue` | Group queue: per-group state tracking (active, idle, pending). Global concurrency limit enforcement. Enqueue/dequeue with FIFO ordering. Warm pool: maintain N containers in `Ready` state, configurable per-group or global. Pool replenishment in background. Backpressure: when at capacity, queue messages with bounded buffer. Error tracking per group (retry count, error count, last error, backoff). |
| 1.4 | `agent-runner` | Container-side Rust binary. IPC client: connect to Unix socket, send `Ready`, receive `Init`. Echo adapter: receives context, sends it back as `OutputComplete` (test harness). Agent adapter trait: `onInit`, `onMessages`, `onShutdown`. Dockerfile for agent image: Rust build + runtime, workspace directories, non-root user. |
| 1.5 | integration | End-to-end test: host spawns Docker container via bollard → IPC handshake → send context → receive echoed output → verify clean shutdown. Warm pool test: pre-warm 2 containers, send message, verify reuse (no cold start). Timeout test: container exceeds phase timeout, verify forced shutdown and cleanup. |

### Exit Criteria

- Integration test spawns a real Docker container, exchanges IPC messages, and tears down cleanly
- Warm pool reuses idle containers (measurable: warm start < 500ms vs cold start)
- Phase timeouts trigger container shutdown within 1 second of deadline
- Circuit breaker trips after N failures, recovers after cooldown
- All containers cleaned up on host shutdown (no orphans)

### Dependencies

- Lane 1.1 (container) depends on 0.4 (IPC types)
- Lane 1.2 (auth) depends on 0.2 (core types)
- Lane 1.3 (queue) depends on 1.1 (container state machine)
- Lane 1.4 (agent-runner) depends on 0.4 (IPC client)
- Lane 1.5 (integration) depends on all of 1.1–1.4

## Phase 2 — Providers & Routing

**Goal**: Messages flow from ingest to model provider to response. Multi-model routing works. Tasks can be scheduled.

### Lanes

| Lane | Crate | Deliverable |
|------|-------|------------|
| 2.1 | `providers` | Provider trait: `name()`, `models()`, `complete()` (streaming), `estimate_tokens()`, `health()`. Anthropic implementation: Messages API with streaming SSE, tool use support. OpenAI-compatible implementation: covers OpenAI, Ollama, vLLM, any OpenAI-format API. Provider registry: register providers, assign to groups, resolve chains. Token budget pools: daily/monthly limits, atomic usage tracking, action on exhaust (fallback/pause/notify). Fallback logic: try primary → on failure/budget exhaust → try next in chain. |
| 2.2 | `router` | Message pipeline: ingest (from channel event) → trigger detection (pattern matching per group) → sender authorization (allowlist check) → context window building (fetch messages since last agent cursor, cap at configurable limit) → dispatch (emit `AgentRequested` event). Message formatting: platform-agnostic XML context format with timezone. Cursor management: per-group composite cursor (timestamp + message ID), persisted to store. Deduplication: filter bot messages, detect echoes. |
| 2.3 | `scheduler` | Task types: cron (cron expression), interval (fixed duration, anchored to prevent drift), once (single execution at time). Task lifecycle: active, paused, completed, failed. Task persistence in store. Scheduler loop: poll for due tasks at configurable interval, emit `TaskDue` event. Integration with queue: tasks compete for container capacity alongside messages. Auto-pause on repeated auth failures. Task run logging: outcome, duration, error details. |
| 2.4 | agent-runner | Real provider integration: agent-runner calls credential proxy with group token, receives AI responses. Conversation loop: receive Init → call provider → execute tool calls → stream output via IPC. Tool framework: Bash, ReadFile, WriteFile, EditFile, GlobSearch, GrepSearch. Tool permission model: per-tool permission level, enforced by agent-runner. Session management: save/resume session ID for context continuity. |

### Exit Criteria

- End-to-end test: synthetic message → routed to group → container spawned → Anthropic API called → response returned
- Multi-model test: two groups configured with different providers, both produce valid responses
- Token budget test: exhaust a group's daily budget, verify fallback provider is used
- Scheduler test: create a cron task, verify it fires within 1 second of schedule
- Tool execution test: agent calls Bash tool, result incorporated into conversation

### Dependencies

- Lane 2.1 (providers) depends on 0.2 (core types)
- Lane 2.2 (router) depends on 0.3 (store), 0.2 (core events)
- Lane 2.3 (scheduler) depends on 0.3 (store), 1.3 (queue)
- Lane 2.4 (agent-runner) depends on 1.2 (auth proxy), 1.4 (agent-runner base)

## Phase 3 — Channels & Tanren

**Goal**: Real-world I/O. Messages from Discord (or any channel). Code work dispatches to Tanren. Health monitoring operational.

### Lanes

| Lane | Crate | Deliverable |
|------|-------|------------|
| 3.1 | `channels` | Channel trait: `connect()`, `disconnect()`, `send_message()`, `send_embed()`. Channel registry: register channels at startup, resolve channel for group. Discord implementation: gateway connection, message handling, mention-to-trigger translation, admin commands. Message type normalization: platform-specific messages → platform-agnostic `Message` type. Channel health: connection status, reconnection logic. |
| 3.2 | `tanren` | Tanren API client: dispatch (full + step-by-step), status polling, cancel, VM management, event query, metrics. Dispatch builder: resolve environment profile, project, branch, phase, CLI from request. IPC integration: handle `DispatchTanren` and `DispatchSelfImprovement` commands from containers. Authorization: per-group Tanren dispatch permissions. Result routing: on dispatch completion, send result to originating channel. Metrics integration: token usage, cost tracking, success rates from Tanren. |
| 3.3 | `health` | Health source trait: `name()`, `check()` → `HealthStatus`. Built-in sources: container runtime, provider health (per provider), Tanren connectivity, store connectivity, channel connection status. Prometheus metrics endpoint (`/metrics`): container_spawn_duration, message_processing_duration, tokens_used (by provider/model/group), queue_depth, warm_pool_size, tanren_dispatch_count. Status server: JSON API for external dashboards. Docker healthcheck endpoint. |
| 3.4 | `bin` | Composition root: load config → init store → init event bus → start credential proxy → register providers → register channels → connect channels → start scheduler → start health monitor → enter event loop. Signal handling: SIGTERM/SIGINT → graceful shutdown (drain queue, wait for active containers, disconnect channels, close store). Startup validation: check Docker socket, check Tanren connectivity (if configured), verify provider credentials. Logging: structured JSON logs via tracing crate. |

### Exit Criteria

- Discord message mentioning the bot → response in Discord (full pipeline)
- Tanren dispatch from interactive agent → Tanren executes → result posted to channel
- Health endpoint returns meaningful status for all subsystems
- Prometheus metrics endpoint returns parseable metrics
- Graceful shutdown completes within 30 seconds under load
- Channel reconnects automatically after transient disconnect

### Dependencies

- Lane 3.1 (channels) depends on 0.2 (core events), 0.3 (store)
- Lane 3.2 (tanren) depends on 0.2 (core types), 0.3 (store)
- Lane 3.3 (health) depends on all other crates (reads health from each)
- Lane 3.4 (bin) depends on everything

## Phase 4 — Compose-Native & Advanced Features

**Goal**: The system is a proper compose citizen with service discovery, network isolation, self-improvement, and operational polish.

### Lanes

| Lane | Crate | Deliverable |
|------|-------|------------|
| 4.1 | compose | Service discovery: query Docker API for containers in same compose project, extract service metadata. Network policy: create per-group Docker networks that connect agent containers to only their allowed compose services. Service env injection: resolve compose service connection info (host, port) and inject into agent container env vars. Compose project detection: read `COMPOSE_PROJECT_NAME`, discover own service identity. |
| 4.2 | self-improvement | Self-improvement dispatch: `SelfImprovementSpec` type (objective, scope, acceptance tests, branch policy). Validation: restrict to own repo, require acceptance tests. Tanren dispatch with `project = "forgeclaw"`. Result handling: on green gate, notify channel with PR link. (Optional) image rebuild trigger: shell out to `docker build` or dispatch another Tanren job. |
| 4.3 | hot-reload | Config file watcher: detect changes to `forgeclaw.toml`. Reloadable config sections: group assignments, provider chains, token budgets, scheduler tasks. Non-reloadable (require restart): store backend, bind addresses. Emit `Event::ConfigReloaded` with diff of what changed. Channels respond to group config changes (add/remove groups). |
| 4.4 | warm-pool | Configurable pool size per group (override global default). Pre-warming on startup: spawn N containers per group on boot. Pool health: periodically verify warm containers are still responsive (IPC heartbeat). Automatic replenishment: when a warm container is consumed, queue a replacement. Metrics: warm_pool_hit_rate, cold_start_count. |
| 4.5 | provider-fallback | Automatic failover: on provider error (5xx, timeout, rate limit), try next in chain without user-visible delay. Budget-triggered fallback: when daily/monthly budget exhausted, switch to fallback provider. Usage alerting: emit events at 80%/95%/100% budget thresholds. Per-provider health tracking: if a provider is consistently failing, deprioritize it. |

### Exit Criteria

- Agent container for group with `allowed_compose_services = ["postgres"]` can reach postgres but not redis
- Self-improvement dispatch creates a branch, runs gates, reports result to channel
- Config change to `forgeclaw.toml` takes effect without restart (group added, budget changed)
- Warm pool hit rate > 90% under steady-state load
- Provider fallback triggers within 1 second of primary failure

## Phase 5 — Hardening & Migration

**Goal**: Production-ready. Migration from NanoClaw. Security audit. Documentation.

### Lanes

| Lane | Deliverable |
|------|------------|
| 5.1 | **Migration tool**: Read NanoClaw SQLite database, import groups, messages, tasks, state into Forgeclaw schema. Map NanoClaw group folders to Forgeclaw group config. Preserve message history and task schedules. Validate imported data integrity. |
| 5.2 | **Security audit**: Mount security (allowlist enforcement, symlink resolution, blocked patterns). IPC authorization (group identity unforgeable from socket connection). Credential proxy hardening (path traversal defense, request validation, body size limits). Container escape prevention (non-root, no privileged, limited capabilities). Network policy enforcement (verify agent can't reach disallowed services). Rate limiting validation (verify limits enforced under load). |
| 5.3 | **Load testing**: Concurrent group processing (10+ groups simultaneously). Warm pool behavior under burst load. Backpressure: verify messages queued correctly when at capacity. Provider failover under load. Memory usage: no leaks over 24-hour run. Container cleanup: no orphans after stress test. |
| 5.4 | **Documentation**: ARCHITECTURE.md (how the system works). Deployment guide (compose setup, config reference). Channel development guide (how to add a new channel). Provider development guide (how to add a new model provider). Agent adapter guide (how to wrap a new agent SDK). IPC protocol specification (formal spec for polyglot adapters). |
| 5.5 | **Additional channels**: Telegram channel implementation. Slack channel implementation. Channel development pattern validated across 3+ implementations. |

### Exit Criteria

- NanoClaw migration tool imports a real NanoClaw database with zero data loss
- Security audit finds no critical or high severity issues
- 24-hour load test with 10 groups: zero orphan containers, zero memory leaks, <1% error rate
- All documentation reviewed and complete
- At least 2 channel implementations beyond Discord

## Summary Timeline

| Phase | Name | Lanes | Key Milestone | Demo |
|-------|------|-------|--------------|------|
| 0 | Foundation | 4 | Workspace compiles, DB schema runs, IPC codec tested | `just ci` green, 50+ tests passing |
| 1 | Container Lifecycle | 5 | Real Docker container spawned and communicated with via IPC | Terminal recording: spawn container, IPC handshake, echo message, clean shutdown |
| 2 | Providers & Routing | 4 | Multi-model AI responses through full message pipeline | CLI tool: pipe a prompt in, get AI response back through the full container pipeline |
| 3 | Channels & Tanren | 4 | Discord messages processed, Tanren dispatches working | @mention bot in Discord, get response. Dispatch code work via Tanren. |
| 4 | Compose-Native | 5 | Service discovery, network isolation, self-improvement | Agent identifies missing capability, dispatches self-improvement, PR created |
| 5 | Hardening | 5 | Migration tool, security audit, load tested, documented | 24-hour load test results, security audit report |

Each phase's lanes can be developed in parallel within the phase. Phases are sequential — Phase N must pass exit criteria before Phase N+1 begins.
