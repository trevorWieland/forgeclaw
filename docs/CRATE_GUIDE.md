# Crate Guide

## Overview

Forgeclaw is a Cargo workspace with 14 crates (12 libraries + 2 binaries). All crates live under `crates/`. Each has a single responsibility, a typed public interface, and its own test suite. Dependency cycles are compile-time errors.

```
crates/
├── forgeclaw/      ── main binary (composition root)
├── agent-runner/   ── container-side binary
├── core/           ── shared types, event bus, config
├── providers/      ── model abstraction
├── channels/       ── channel trait + implementations
├── container/      ── lifecycle + Docker runtime
│    └── (depends on) ipc
├── ipc/            ── host-container protocol
├── auth/           ── credential proxy
│    └── (depends on) providers
├── store/          ── database layer
├── scheduler/      ── task scheduling
│    └── (depends on) store, queue
├── router/         ── message pipeline
│    └── (depends on) store, channels, queue
├── queue/          ── concurrency control
│    └── (depends on) container
├── tanren/         ── Tanren client
│    └── (depends on) store
└── health/         ── monitoring
     └── (depends on) providers, container, tanren
```

All crates depend on `core` (implicitly). `forgeclaw` (main binary) depends on everything. `agent-runner` depends on `ipc`.

---

## `core`

**Responsibility**: Shared types, event bus, error taxonomy, configuration model. The foundation everything else builds on.

**Depends on**: nothing

**Key types**:

```rust
// Identity types (newtypes prevent mixing up strings)
pub struct GroupId(pub String);
pub struct ContainerId(pub String);
pub struct ProviderId(pub String);
pub struct ChannelId(pub String);
pub struct JobId(pub String);
pub struct TaskId(pub String);
pub struct DispatchId(pub String);

// Event bus
pub enum Event {
    Message(MessageEvent),
    Container(ContainerEvent),
    Provider(ProviderEvent),
    Tanren(TanrenEvent),
    Task(TaskEvent),
    Health(HealthEvent),
    Ipc(IpcEvent),
    Config(ConfigEvent),
}

pub struct EventBus {
    tx: broadcast::Sender<Event>,
}

impl EventBus {
    pub fn emit(&self, event: Event);
    pub fn subscribe(&self) -> broadcast::Receiver<Event>;
}

// Error taxonomy
pub enum ErrorClass {
    Transient { retry_after: Duration },
    Auth { provider: ProviderId, reason: String },
    Config { key: String, reason: String },
    Container { id: Option<ContainerId>, reason: String },
    Fatal { reason: String },
}

// Configuration (deserialized from TOML)
// Note: the implementation uses BTreeMap for deterministic serialization order.
pub struct ForgeclawConfig {
    pub runtime: RuntimeConfig,
    pub store: StoreConfig,
    pub providers: HashMap<String, ProviderConfig>,
    pub budget_pools: HashMap<String, BudgetPoolConfig>,
    pub groups: HashMap<String, GroupConfig>,
    pub container: ContainerDefaults,
    pub tanren: Option<TanrenConfig>,
    pub channels: HashMap<String, ChannelConfig>,
}
```

**Test surface**: Event bus emit/subscribe, config deserialization from TOML, error classification.

---

## `providers`

**Responsibility**: Multi-model provider abstraction. Provider trait, built-in implementations, token budget management, fallback chain logic.

**Depends on**: `core`

**Key types**:

```rust
#[async_trait]
pub trait Provider: Send + Sync + 'static {
    fn id(&self) -> &ProviderId;
    fn name(&self) -> &str;
    fn models(&self) -> &[ModelSpec];
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionStream>;
    fn estimate_tokens(&self, messages: &[Message], tools: &[ToolSpec]) -> TokenEstimate;
    async fn health(&self) -> ProviderHealth;
}

pub struct ProviderRegistry { ... }
pub struct ProviderChain { ... }
pub struct TokenBudget { ... }

// Built-in implementations
pub struct AnthropicProvider { ... }
pub struct OpenAiCompatProvider { ... }
```

**Test surface**: Provider registration, chain resolution, fallback logic, budget tracking, budget exhaustion actions.

---

## `channels`

**Responsibility**: Channel trait, channel registry, message type normalization. Concrete implementations (Discord, Telegram, etc.) behind feature flags.

**Depends on**: `core`

**Key types**:

```rust
#[async_trait]
pub trait Channel: Send + Sync + 'static {
    fn id(&self) -> &ChannelId;
    fn name(&self) -> &str;
    async fn connect(&mut self) -> Result<()>;
    async fn disconnect(&mut self) -> Result<()>;
    async fn send_message(&self, group: &GroupId, text: &str) -> Result<()>;
    async fn send_embed(&self, group: &GroupId, embed: &Embed) -> Result<()>;
    async fn health(&self) -> ChannelHealth;
}

pub struct ChannelRegistry {
    channels: HashMap<ChannelId, Box<dyn Channel>>,
    group_to_channel: HashMap<GroupId, ChannelId>,
}

// Platform-agnostic message
pub struct Message {
    pub id: String,
    pub group: GroupId,
    pub sender: String,
    pub text: String,
    pub timestamp: DateTime<Utc>,
    pub channel: ChannelId,
    pub is_bot: bool,
}
```

**Feature flags**: `discord`, `telegram`, `slack` (each adds a channel implementation).

**Test surface**: Registry operations, message normalization, channel health reporting.

---

## `container`

**Responsibility**: Container lifecycle state machine, runtime abstraction (Docker via bollard), container spec builder, mount security, warm pool management.

**Depends on**: `core`, `ipc`

**Key types**:

```rust
pub enum ContainerState { Provisioning, Starting, Ready, Processing, Streaming, Idle, Draining, Exited, Failed }
pub struct ContainerManager { ... }
pub struct WarmPool { ... }
pub struct ContainerSpec { ... }
pub struct MountSecurity { ... }

#[async_trait]
pub trait ContainerRuntime: Send + Sync + 'static {
    async fn create(&self, spec: &ContainerSpec) -> Result<ContainerId>;
    async fn start(&self, id: &ContainerId) -> Result<()>;
    async fn stop(&self, id: &ContainerId, timeout: Duration) -> Result<()>;
    async fn remove(&self, id: &ContainerId) -> Result<()>;
    async fn list(&self) -> Result<Vec<ContainerInfo>>;
    async fn health(&self) -> Result<RuntimeHealth>;
    async fn cleanup_orphans(&self, label: &str) -> Result<usize>;
}

pub struct DockerRuntime { ... }  // via bollard
```

**Test surface**: State transitions (valid and invalid), timeout enforcement, mount validation, warm pool assignment/replenishment/eviction, orphan cleanup.

---

## `ipc`

**Responsibility**: The IPC protocol between host and containers. Frame codec, message types, Unix socket server (host) and client (agent-runner).

**Depends on**: `core`

**Key types**:

```rust
// Message enums (see IPC_PROTOCOL.md for full spec)
pub enum HostToContainer { Init, Messages, Shutdown }
pub enum ContainerToHost { Ready, OutputDelta, OutputComplete, Progress, Command, Error, Heartbeat }

// Frame codec
pub struct FrameCodec;
impl Encoder<&[u8]> for FrameCodec { ... }
impl Decoder for FrameCodec { ... }

// Server (host side)
pub struct IpcServer { ... }
impl IpcServer {
    pub fn bind(path: &Path) -> Result<Self>;
    pub fn bind_with_options(path: &Path, options: IpcServerOptions) -> Result<Self>;
    pub async fn accept(&self, group: GroupInfo) -> Result<PendingConnection>;
}

pub struct IpcServerOptions {
    pub unauthorized_limit: UnauthorizedCommandLimitConfig,
    pub unknown_traffic_limit: UnknownTrafficLimitConfig,
    pub peer_credential_policy: PeerCredentialPolicy,
    pub write_timeout: Duration,
}

pub enum PeerCredentialPolicy {
    CaptureOnly,
    RequireExact { uid: Option<u32>, gid: Option<u32> },
    Custom(...),
}

pub struct UnauthorizedCommandLimitConfig {
    pub burst_capacity: u32,
    pub refill_per_second: u32,
    pub backoff: Duration,
    pub disconnect_after_strikes: u32,
}

pub struct UnknownTrafficLimitConfig {
    pub lifetime_message_limit: usize,      // 0 disables this control
    pub lifetime_byte_limit: usize,         // 0 disables this control
    pub rate_limit_burst_capacity: u32,     // 0 disables rate limiting
    pub rate_limit_refill_per_second: u32,  // 0 disables rate limiting
}

pub struct PendingConnection { ... }
impl PendingConnection {
    pub async fn handshake(
        self,
        init: InitPayload,
        timeout: Duration,
    ) -> Result<(IpcConnection, ReadyPayload)>;
}

pub enum UnknownTypePolicy { SkipBounded, Strict }

pub enum IpcInboundEvent {
    Command(AuthorizedCommand),
    Unauthorized(UnauthorizedCommandRejection), // non-fatal: event loops stay alive
    Message(ContainerToHost),
}

pub struct IpcConnection { ... } // typed send/recv, recv_command auth matrix, into_split
impl IpcConnection {
    pub fn negotiated_protocol_version(&self) -> NegotiatedProtocolVersion;
    pub async fn recv_event(&mut self) -> Result<IpcInboundEvent>; // command + non-command demux
    pub async fn recv_event_with_policy(
        &mut self,
        policy: UnknownTypePolicy,
    ) -> Result<IpcInboundEvent>;
    pub async fn recv_command(&mut self) -> Result<AuthorizedCommand>; // returns NotCommand for legal interleaving
    pub async fn recv_command_strict(&mut self) -> Result<AuthorizedCommand>; // strict non-command rejection
}
pub struct IpcConnectionWriter { ... }
pub struct IpcConnectionReader { ... }

// Client (container side)
pub struct IpcClientOptions {
    pub write_timeout: Duration,
}

pub struct IpcClient { ... }
impl IpcClient {
    pub async fn connect(path: &Path) -> Result<PendingClient>;
    pub async fn connect_with_options(path: &Path, options: IpcClientOptions) -> Result<PendingClient>;
    pub async fn recv(&mut self) -> Result<HostToContainer>; // default SkipBounded
    pub async fn recv_with_policy(
        &mut self,
        policy: UnknownTypePolicy,
    ) -> Result<HostToContainer>;
}

pub struct PendingClient { ... }
impl PendingClient {
    pub async fn handshake(
        self,
        ready: ReadyPayload,
        timeout: Duration,
    ) -> Result<(IpcClient, InitPayload)>;
}
```

Safe receive API source of truth:
- [`IpcConnection::recv_event`](../crates/ipc/src/server/mod.rs)
- [`IpcConnection::recv_event_with_policy`](../crates/ipc/src/server/mod.rs)
- [`IpcConnection::recv_command`](../crates/ipc/src/server/mod.rs)
- [`IpcConnection::recv_command_strict`](../crates/ipc/src/server/mod.rs)

`recv_container_unchecked*` are crate-private internals and are not part of the public API contract.

**Test surface**: Frame codec roundtrip (all message types), max frame size enforcement, command/non-command interleaving behavior, connection lifecycle (connect, handshake, exchange, close), malformed frame handling, outbound pre-serialization constraint validation, schema/runtime parity checks.

---

## `auth`

**Responsibility**: Multi-provider credential proxy, per-container token management, rate limiting, circuit breaker.

**Depends on**: `core`, `providers`

**Key types**:

```rust
pub struct CredentialProxy {
    listener: TcpListener,
    tokens: Arc<RwLock<HashMap<String, TokenInfo>>>,
    providers: Arc<ProviderRegistry>,
    rate_limiter: RateLimiter,
    circuit_breakers: HashMap<ProviderId, CircuitBreaker>,
}

pub struct TokenInfo {
    pub group: GroupId,
    pub container: ContainerId,
    pub chain: ProviderChain,
}

pub struct CircuitBreaker {
    pub state: CircuitState,
    pub failure_count: u32,
    pub last_failure: Option<Instant>,
    pub cooldown: Duration,
}
```

**Test surface**: Token registration/deregistration, request routing, credential injection, rate limit enforcement, circuit breaker state transitions, format translation.

---

## `store`

**Responsibility**: Database abstraction with compile-time typed queries, a single Rust source of truth for schema, and error classification for the recovery machinery in `core`. Supports SQLite (local dev, tests, small deployments) and PostgreSQL (production) behind one API via SeaORM (built on sqlx). Also exposes an audit-log `events` table used by `router`, `scheduler`, and `health` for debugging and metrics.

**Depends on**: `core`

**Key types**:

```rust
pub struct Store {
    db: sea_orm::DatabaseConnection,
}

impl Store {
    // Construction & migrations
    pub async fn connect(url: &str) -> Result<Self, StoreError>;
    pub async fn connect_sqlite_memory() -> Result<Self, StoreError>;
    pub async fn migrate(&self) -> Result<(), StoreError>;

    // Messages
    pub async fn store_message(&self, msg: &NewMessage) -> Result<(), StoreError>;
    pub async fn get_messages_since(
        &self,
        group: &GroupId,
        cursor: &Cursor,
        limit: i64,
    ) -> Result<Vec<StoredMessage>, StoreError>;

    // Groups
    pub async fn get_group(&self, id: &GroupId) -> Result<Option<RegisteredGroup>, StoreError>;
    pub async fn upsert_group(&self, group: &RegisteredGroup) -> Result<(), StoreError>;

    // Tasks
    pub async fn create_task(&self, task: &NewTask) -> Result<TaskId, StoreError>;
    pub async fn get_due_tasks(
        &self,
        now: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<StoredTask>, StoreError>;
    pub async fn update_task_after_run(
        &self,
        id: &TaskId,
        result: &TaskRunResult,
    ) -> Result<(), StoreError>;

    // State (generic key/value)
    pub async fn get_state(&self, key: &str) -> Result<Option<String>, StoreError>;
    pub async fn set_state(&self, key: &str, value: &str) -> Result<(), StoreError>;

    // Sessions (group → agent session id)
    pub async fn get_session(&self, group: &GroupId) -> Result<Option<String>, StoreError>;
    pub async fn set_session(&self, group: &GroupId, session_id: &str) -> Result<(), StoreError>;

    // Events (audit log)
    pub async fn record_event(&self, event: &NewEvent) -> Result<(), StoreError>;
    pub async fn list_events(
        &self,
        filter: &EventFilter,
        limit: i64,
    ) -> Result<Vec<StoredEvent>, StoreError>;
}

pub struct Cursor {
    /// Exclusive lower bound on the store-owned monotonic `seq`.
    pub seq: i64,
}
```

**Notes**

- Queries go through SeaORM's typed `Entity` + `Column` API. Wrong column names, wrong operand types, and structurally-invalid filters fail compilation — not at runtime. A schema-drift test additionally compares every entity against the live migrated schema on both backends.
- Schema is defined once in Rust via SeaQuery's `SchemaManager`; the same migration code emits correct DDL for both backends. All index creations use `IF NOT EXISTS` so a crash mid-migration followed by a restart is safe.
- **Message pagination uses a store-owned `seq` cursor with commit-visible ordering on both backends.** On insert, the database assigns a monotonically-increasing `BIGINT` / `INTEGER PRIMARY KEY AUTOINCREMENT` ordinal, and `get_messages_since` pages strictly on that `seq`. No caller can produce a cursor key smaller than one already delivered (closes caller backdating). On PostgreSQL, `store_message` wraps each insert in a transaction-scoped `pg_advisory_xact_lock` keyed on `group_id`, so concurrent writers to the same group serialize (their `seq` allocation order equals commit order for that stream) while writers to *different* groups do not contend — one busy chat cannot head-of-line block any other group's persistence. Since `get_messages_since` always filters by `group_id`, a reader that observes `seq = N` in group `G` is guaranteed every earlier `seq` in group `G` is also visible. On SQLite the database-level writer lock gives the same guarantee without extra work. The caller-generated UUIDv7 `id` is kept as a unique correlation key, not a sort key.
- **Bounded reads.** Every query method clamps its caller-supplied `limit` against `MAX_PAGE_SIZE` (default 10,000). `get_messages_since`, `get_due_tasks`, and `list_events` all accept an explicit `limit: i64`. `get_due_tasks` also takes an explicit `now: DateTime<Utc>` so the method is pure with respect to system time and dialect-neutral.
- `StoredTask` is the flat row type the store returns; the `scheduler` crate maps it to its richer `ScheduledTask` domain enum.
- **Credential redaction.** `StoreError::Database` does **not** retain the raw `sea_orm::DbErr`. The full error chain is sanitized once at the `From<DbErr>` boundary and stored as plain strings; `Display`, `Debug`, and `source()` all see only the redacted form. `StoreError::classify()` maps every failure into a `core::ErrorClass` via an internal `DatabaseCategory` hint so callers decide retry vs circuit-break vs halt without parsing messages.

**Test surface**: CRUD operations for all entities, cursor-based pagination edge cases (including a regression that proves backdated inserts are not skipped), migration idempotency AND migration restart safety after partial apply, schema-drift detection, and credential-redaction checks against `Display`/`Debug`/`source()`. The SQLite in-memory suite covers the full API surface and runs on any machine with no external services. PostgreSQL parity tests are gated behind a `postgres-tests` Cargo feature and a `FORGECLAW_TEST_POSTGRES_URL` env var; when the feature is enabled, a missing env var is a hard test failure (not a silent skip), and a dedicated `postgres-parity` CI job provisions a `postgres:18` service container so parity is a real gate.

---

## `scheduler`

**Responsibility**: Task scheduling with cron, interval, and once schedules. Task lifecycle management.

**Depends on**: `core`, `store`, `queue`

**Key types**:

```rust
pub struct Scheduler {
    store: Arc<Store>,
    event_bus: EventBus,
    poll_interval: Duration,
}

pub enum ScheduleType {
    Cron(String),            // cron expression
    Interval(Duration),      // fixed interval, anchored to prevent drift
    Once(DateTime<Utc>),     // single execution
}

pub struct ScheduledTask {
    pub id: TaskId,
    pub group: GroupId,
    pub prompt: String,
    pub schedule: ScheduleType,
    pub status: TaskStatus,
    pub next_run: Option<DateTime<Utc>>,
    pub last_result: Option<String>,
}

pub enum TaskStatus { Active, Paused, Completed, Failed }
```

**Test surface**: Cron next-run computation, interval drift prevention, task lifecycle transitions, auto-pause on auth failure.

---

## `router`

**Responsibility**: Message pipeline from channel ingest to container dispatch.

**Depends on**: `core`, `store`, `channels`, `queue`

**Key types**:

```rust
pub struct Router {
    store: Arc<Store>,
    event_bus: EventBus,
    trigger_patterns: HashMap<GroupId, Regex>,
    sender_allowlists: HashMap<GroupId, HashSet<String>>,
}

impl Router {
    /// Process a new message event
    pub async fn handle_message(&self, event: &MessageEvent) -> Result<()>;

    /// Build context window for a group
    pub async fn build_context(&self, group: &GroupId) -> Result<ContextWindow>;
}

pub struct ContextWindow {
    pub messages: Vec<Message>,
    pub group: GroupInfo,
    pub timezone: String,
    pub omitted_count: usize,
}
```

**Pipeline stages**:
1. **Ingest**: Receive `MessageEvent` from event bus
2. **Filter**: Is this a registered group? Does it match the trigger pattern? Is the sender authorized?
3. **Context**: Fetch messages since last agent cursor. Cap at configurable limit. Note omitted count.
4. **Dispatch**: Emit `Event::AgentRequested { group, context }` to event bus

**Test surface**: Trigger pattern matching, sender authorization, context window building, cursor advancement, deduplication.

---

## `queue`

**Responsibility**: Group concurrency control, backpressure management, warm pool coordination.

**Depends on**: `core`, `container`

**Key types**:

```rust
pub struct GroupQueue {
    groups: HashMap<GroupId, GroupState>,
    max_concurrent: usize,
    active_count: AtomicUsize,
    waiting: Mutex<VecDeque<GroupId>>,
}
```

**Test surface**: Concurrency limit enforcement, FIFO ordering, backpressure (queue fills, oldest dropped), warm pool integration, error tracking and backoff.

---

## `tanren`

**Responsibility**: Tanren API client, dispatch management, self-improvement orchestration.

**Depends on**: `core`, `store`

**Key types**:

```rust
pub struct TanrenClient { ... }
pub struct TanrenIntegration {
    client: TanrenClient,
    store: Arc<Store>,
    event_bus: EventBus,
    active_dispatches: RwLock<HashMap<DispatchId, DispatchInfo>>,
}
```

**Test surface**: API client (mock HTTP server), dispatch lifecycle, self-improvement validation, result routing.

---

## `health`

**Responsibility**: Aggregated health monitoring, Prometheus metrics endpoint, status API.

**Depends on**: `core`, `providers`, `container`, `tanren`

**Key types**:

```rust
pub struct HealthMonitor {
    sources: Vec<Box<dyn HealthSource>>,
    metrics: PrometheusMetrics,
}

#[async_trait]
pub trait HealthSource: Send + Sync {
    fn name(&self) -> &str;
    async fn check(&self) -> HealthStatus;
}

pub enum HealthStatus {
    Healthy,
    Degraded(String),
    Unhealthy(String),
}
```

**Built-in sources**: Docker runtime, each provider, Tanren, store, each channel.

**Endpoints**:
- `GET /health` → aggregated health JSON
- `GET /metrics` → Prometheus exposition format
- `GET /status` → detailed status for dashboards

**Test surface**: Health aggregation (all healthy, one degraded, one unhealthy), metric recording, endpoint response format.

---

## `forgeclaw` (main binary)

**Path**: `crates/forgeclaw/`

**Responsibility**: Composition root. Thin — no business logic. Wires crates together, starts services, handles signals.

**Depends on**: everything

**Startup sequence**:
1. Load config (TOML + env overrides)
2. Initialize store (run migrations)
3. Create event bus
4. Initialize provider registry
5. Start credential proxy
6. Initialize container manager + warm pool
7. Register channels, connect
8. Start router (subscribes to message events)
9. Start scheduler (polls for due tasks)
10. Start Tanren integration (if configured)
11. Start health monitor + metrics/status servers
12. Enter event loop

**Shutdown sequence** (on SIGTERM/SIGINT):
1. Stop accepting new work
2. Drain queue (wait for active containers, bounded timeout)
3. Disconnect channels
4. Stop scheduler
5. Stop credential proxy
6. Clean up remaining containers
7. Close store
8. Exit

**Test surface**: None (integration tested via Phase 1-3 integration tests). The composition root is deliberately untestable in isolation — its job is to wire things together.
