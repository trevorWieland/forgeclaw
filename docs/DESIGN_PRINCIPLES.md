# Design Principles

These principles guide every design decision in Forgeclaw. When two approaches seem equally valid, the one that better aligns with these principles wins.

## 1. Event-First, Not Callback-First

Subsystems communicate through a typed event bus, not through direct function calls or closure-captured callbacks.

**What this means in practice:**
- The router doesn't call `containerManager.spawn()`. It emits `Event::AgentRequested`. The container manager subscribes and acts.
- Adding metrics doesn't require modifying every subsystem. A metrics subscriber listens to all events and records what it cares about.
- Testing a subsystem means injecting events and asserting on emitted events. No mocking of upstream/downstream dependencies.

**What this prevents:**
- God Objects (NanoClaw's `index.ts` owned all state because it was the only place where callbacks could close over shared state)
- Fragile coupling (changing the container manager's API doesn't break the router)
- Cross-cutting concern explosion (adding tracing, audit logging, or metrics to NanoClaw required touching every call site)

**The event bus is not an ESB.** It's a tokio broadcast channel with typed wrappers. Events are fire-and-observe, not request-response. For request-response patterns (e.g., "spawn a container and tell me when it's ready"), use oneshot channels carried inside the event payload.

## 2. Protocol-First Polyglot

The IPC protocol between host and container is the most important interface in the system. It's more important than any Rust API, because it's the contract that enables polyglot agent adapters.

**What this means in practice:**
- The IPC protocol is specified as a versioned document with JSON Schema definitions, not just as Rust types
- Breaking changes to the protocol require a version bump and migration path
- Reference implementations exist in Rust and TypeScript — these are first-class deliverables, not afterthoughts
- A new agent SDK dropping tomorrow means writing a thin adapter (~30 lines) against the protocol spec, not restructuring the system

**What this prevents:**
- Vendor lock-in to a single agent SDK (NanoClaw was Claude Code-only)
- Rebuilding the agent image every time the host changes (the protocol is the stable boundary)
- Fragile stdout parsing (NanoClaw used sentinel markers in stdout; Forgeclaw uses length-prefixed JSON frames over Unix sockets)

## 3. Typed State Machines

Container lifecycle, task lifecycle, provider health, and connection state are all modeled as explicit state machines with typed transitions.

**What this means in practice:**
- Invalid state transitions don't compile. You can't go from `Exited` to `Processing` — the type system prevents it.
- Each state carries its own data. `Processing` holds the job ID and start time. `Idle` holds the time it became idle. `Failed` holds the error class.
- Each state has its own timeout. Provisioning timeout ≠ processing timeout ≠ idle TTL.
- State transitions emit events. The event bus broadcasts `ContainerEvent::Transitioned { from, to }`.

**What this prevents:**
- The "slow stream never times out" bug (NanoClaw reset a single global timer on every stream chunk; Forgeclaw has per-state timeouts with a hard wall-clock cap on `Streaming`)
- Container leaks (if a container is in `Starting` for longer than its timeout, it transitions to `Failed` and gets cleaned up — no manual intervention)
- Race conditions in lifecycle management (transitions are atomic; two concurrent calls to transition produce one success and one error)

## 4. Compile-Time Correctness

Prefer catching errors at compile time over runtime.

**What this means in practice:**
- Database queries are checked against the schema at compile time (sqlx `query_as!` macro). Wrong column name? Doesn't compile.
- Configuration is deserialized into typed structs (serde). Missing required field? Doesn't compile. Wrong type? Doesn't compile.
- IPC messages are serde-typed enums. Malformed JSON fails deserialization with a clear error, not a runtime panic.
- Newtype wrappers for IDs (`GroupId`, `ContainerId`, `ProviderId`) prevent mixing up string parameters.

**What this prevents:**
- Cursor corruption from untyped JSON (NanoClaw's `last_agent_timestamp` was parsed from a JSON string with runtime format detection)
- SQL injection (sqlx uses parameterized queries exclusively)
- ID confusion (passing a group ID where a container ID was expected)

## 5. Compose-Native, Not Compose-Compatible

Forgeclaw doesn't "happen to run in Docker." It is infrastructure that joins the compose stack as a peer service.

**What this means in practice:**
- Forgeclaw discovers sibling services in the compose stack via the Docker API
- Agent containers get network policies based on their group config — they can reach allowed services and nothing else
- Connection info for compose services is resolved and injected into agent containers as env vars
- Forgeclaw exposes a Docker healthcheck, Prometheus metrics, and a status API that other compose services can query
- The Dockerfile is a first-class deliverable, not an afterthought

**What this prevents:**
- Agent containers with unrestricted network access (a guest agent shouldn't be able to reach your production database)
- Manual configuration of service URLs (Forgeclaw resolves them from the compose network)
- Treating the container as a black box (health and metrics are observable from the compose stack)

## 6. Multi-Model by Default

The system assumes multiple model providers from day one. Single-provider is a special case, not the default.

**What this means in practice:**
- Every group has a provider chain: primary + ordered fallbacks
- Token budgets are tracked per-pool (groups can share pools or have independent ones)
- The credential proxy resolves the correct provider for each request based on the group token
- Provider health is monitored; unhealthy providers are deprioritized in fallback chains
- The agent runner doesn't know which provider it's talking to — it speaks a normalized API format to the credential proxy

**What this prevents:**
- Hardcoded Anthropic assumptions throughout the codebase (NanoClaw's `ANTHROPIC_BASE_URL`, `CLAUDE.md`, Anthropic-style auth)
- Single point of failure (if Anthropic is down, groups with fallbacks continue working)
- Budget overruns (daily/monthly limits with configurable actions: fallback, pause, or notify)

## 7. Two Container Types, One Orchestrator

Interactive agent containers and Tanren dispatch containers serve different purposes and have different lifecycle management, but they're orchestrated by the same system.

**What this means in practice:**
- Interactive containers are managed by Forgeclaw's container crate: spawned on demand, IPC-connected, warm-pooled, reusable
- Tanren containers are managed by Tanren: provisioned, executed, gated, torn down through Tanren's API
- Forgeclaw dispatches to Tanren and monitors results — it doesn't manage Tanren's containers
- Both container types can be DooD (sibling containers on the same Docker socket) or remote (Tanren supports VM provisioning)
- The self-improvement loop bridges both: an interactive agent requests a capability, Forgeclaw dispatches to Tanren, Tanren builds it

**What this prevents:**
- Conflating interactive and batch workloads (they have different timeout, concurrency, and lifecycle requirements)
- Rebuilding Tanren's orchestration inside Forgeclaw (Tanren already handles provisioning, gating, auditing, and cleanup)
- Orphaned resources (Tanren guarantees VM/container cleanup; Forgeclaw guarantees interactive container cleanup)

## 8. Security Through Isolation, Not Trust

Every boundary is enforced, not assumed.

**What this means in practice:**
- Container identity is derived from the IPC socket connection (which socket connected), not from message content (what the container claims to be). Unforgeable.
- Credentials are injected by the proxy at request time. Containers never see API keys.
- Mount paths are validated against an external allowlist that is never mounted into the container. Tamper-proof.
- Network policies are enforced by Docker networking, not by application-level checks.
- Non-main groups have a strictly smaller set of IPC commands available. The type system distinguishes `MainGroupCommand` from `GroupCommand`.

**What this prevents:**
- Container impersonation (a rogue container claiming to be the main group)
- Credential theft (containers can't extract keys from env vars or config files)
- Mount escape (containers can't access paths outside the allowlist)
- Network escape (containers can't reach services they're not authorized for)

## 9. Fail Explicitly, Recover Automatically

Errors are classified, not swallowed. Recovery is codified, not manual.

**What this means in practice:**
- Every error carries a classification: `Transient` (retry), `Auth` (circuit break), `Config` (report), `Container` (restart), `Fatal` (shutdown)
- Transient errors trigger automatic retry with backoff. The retry count and backoff duration are per-error-class, not global.
- Auth errors trip the circuit breaker: pause the affected group, notify the channel, wait for cooldown
- Container failures trigger cleanup and re-provisioning from the warm pool
- Fatal errors trigger graceful shutdown with queue drain

**What this prevents:**
- Silent data loss (NanoClaw's output callback errors were caught, logged, then resolved as "success")
- Retry storms (circuit breaker prevents hammering a down provider)
- Manual intervention for recoverable failures (warm pool replenishment, provider fallback, and task auto-pause are all automatic)

## 10. Observable by Default

Every subsystem emits structured metrics, health status, and logs. Observability is not an add-on.

**What this means in practice:**
- Prometheus metrics endpoint exposes: container spawn duration, message processing time, token usage by provider/model/group, queue depth, warm pool size, Tanren dispatch count, error rates
- Health endpoint aggregates: Docker runtime, each provider, Tanren, database, each channel
- Structured JSON logs (via `tracing` crate) with correlation IDs linking a message through the full pipeline
- Event bus enables external subscribers for custom monitoring

**What this prevents:**
- Flying blind in production (NanoClaw had `pino` logging but no metrics endpoint)
- Debugging by reading container logs (structured events and metrics tell you what's happening without digging through stdout)
- Alert fatigue (health aggregation means one endpoint to check, not N)
