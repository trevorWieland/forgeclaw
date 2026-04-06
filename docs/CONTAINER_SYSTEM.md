# Container System

## Overview

The container system manages the full lifecycle of interactive agent containers — from provisioning through processing to cleanup. It uses typed state machines for lifecycle management, bollard for Docker Engine API communication, a warm pool for reducing cold-start latency, and the IPC protocol for structured host-container communication.

## Container Lifecycle State Machine

```
                    ┌────────────┐
                    │Provisioning│
                    └─────┬──────┘
                          │ container created + started
                          ▼
                    ┌────────────┐
             ┌──────│  Starting  │
             │      └─────┬──────┘
             │            │ IPC handshake (ready received)
             │            ▼
             │      ┌────────────┐◄──────────────────────────┐
             │      │   Ready    │                            │
             │      └─────┬──────┘                            │
             │            │ init sent                         │
             │            ▼                                   │
             │      ┌────────────┐                            │
             │      │ Processing │──── follow-up messages ───►│
             │      └─────┬──────┘◄──── (stays Processing) ──┘
             │            │ output streaming                   │
             │            ▼                        output_complete
             │      ┌────────────┐                   (reuse)  │
             │      │ Streaming  │───────────────────────────►│
             │      └─────┬──────┘                            │
             │            │ output_complete (no reuse)        │
             │            ▼                                   │
             │      ┌────────────┐   idle TTL expired         │
             │      │    Idle    │────────────────┐           │
             │      └─────┬──────┘                │           │
             │            │ new work              │           │
             │            ▼                       │           │
             │        (→ Ready)                   │           │
             │                                    ▼           │
             │                              ┌──────────┐      │
             │                              │ Draining │      │
             │                              └────┬─────┘      │
             │                                   │            │
             ▼                                   ▼            │
       ┌──────────┐                        ┌──────────┐      │
       │  Failed  │                        │  Exited  │      │
       └──────────┘                        └──────────┘      │
```

### State Definitions

| State | Data | Timeout | Description |
|-------|------|---------|-------------|
| `Provisioning` | `ContainerSpec`, `started_at` | 60s | Image pulled, container being created |
| `Starting` | `ContainerId`, `started_at` | 30s | Container running, waiting for IPC `ready` |
| `Ready` | `ContainerId`, `IpcChannel`, `since` | — | IPC connected, idle, available for work |
| `Processing` | `ContainerId`, `IpcChannel`, `JobId`, `since` | group timeout (default 30m) | Actively processing a job |
| `Streaming` | `ContainerId`, `IpcChannel`, `JobId`, `since` | 5m hard cap | Model output being streamed back |
| `Idle` | `ContainerId`, `IpcChannel`, `since` | idle TTL (default 5m) | Work complete, available for reuse |
| `Draining` | `ContainerId`, `deadline` | drain timeout (10s) | Graceful shutdown requested |
| `Exited` | `ContainerId`, `ExitStatus`, `at` | — | Terminal: container stopped |
| `Failed` | `ContainerId` (optional), `ErrorClass`, `at` | — | Terminal: error during any phase |

### Transition Rules

Transitions are enforced at compile time via the type system. Invalid transitions (e.g., `Exited → Processing`) don't exist as methods.

**Key constraints:**
- `Streaming` has a **hard wall-clock cap** (default 5 minutes). Activity does not reset this timer. This prevents the "slow stream never times out" bug from NanoClaw.
- `Processing` timeout is the group's configured timeout (default 30 minutes). This is the total time for the agent to produce a result, including all tool calls.
- `Idle → Ready` is the warm pool reuse path. The container stays alive and IPC-connected.
- `Idle` timeout triggers `Idle → Draining → Exited`. The warm pool decides whether to keep or evict idle containers.
- Any state can transition to `Failed` on error. Failed containers are cleaned up immediately.

## Container Runtime Trait

```rust
#[async_trait]
pub trait ContainerRuntime: Send + Sync + 'static {
    /// Create a container from a spec (does not start it)
    async fn create(&self, spec: &ContainerSpec) -> Result<ContainerId>;

    /// Start a created container
    async fn start(&self, id: &ContainerId) -> Result<()>;

    /// Stop a running container (graceful with timeout, then force)
    async fn stop(&self, id: &ContainerId, timeout: Duration) -> Result<()>;

    /// Remove a stopped container
    async fn remove(&self, id: &ContainerId) -> Result<()>;

    /// List containers managed by this runtime (filtered by label)
    async fn list(&self) -> Result<Vec<ContainerInfo>>;

    /// Check runtime health (Docker daemon reachable, disk space, etc.)
    async fn health(&self) -> Result<RuntimeHealth>;

    /// Clean up orphaned containers from previous runs
    async fn cleanup_orphans(&self, label_prefix: &str) -> Result<usize>;
}
```

### Docker Runtime (Primary)

Uses the `bollard` crate to talk to the Docker Engine API over the Unix socket (`/var/run/docker.sock`). No `docker` CLI subprocess spawning.

**Advantages over CLI:**
- Structured JSON responses (no stdout parsing)
- Connection pooling (reuses HTTP connection)
- Native streaming for logs and events
- No `docker` binary needed in the host container

**Container labels**: Every container created by Forgeclaw is labeled:
```
forgeclaw.managed = "true"
forgeclaw.group = "<group_id>"
forgeclaw.instance = "<instance_id>"
forgeclaw.created = "<ISO 8601 timestamp>"
```

Labels enable orphan cleanup: on startup, find containers with `forgeclaw.managed=true` and `forgeclaw.instance` not matching the current instance → stop and remove.

### Future Runtimes

The trait enables alternative implementations:
- **Podman**: API-compatible with Docker (bollard works with minor configuration)
- **Apple Container**: macOS-native containers (CLI-based, separate implementation)
- **Kubernetes**: Create pods instead of containers (future, for k8s deployment)

## Container Spec

```rust
pub struct ContainerSpec {
    /// Image to use
    pub image: String,

    /// Environment variables
    pub env: HashMap<String, String>,

    /// Volume mounts
    pub mounts: Vec<Mount>,

    /// Network to attach to
    pub network: Option<String>,

    /// Resource limits
    pub resources: ResourceLimits,

    /// Labels for management
    pub labels: HashMap<String, String>,

    /// User to run as (UID:GID)
    pub user: Option<String>,

    /// Working directory
    pub workdir: Option<String>,

    /// Entrypoint override
    pub entrypoint: Option<Vec<String>>,
}

pub struct Mount {
    pub source: PathBuf,
    pub target: String,
    pub read_only: bool,
}

pub struct ResourceLimits {
    pub memory: Option<String>,      // e.g., "4g"
    pub cpu: Option<f64>,            // e.g., 2.0
    pub pids_limit: Option<i64>,     // e.g., 256
}
```

### Mount Categories

| Mount | Target | Mode | Purpose |
|-------|--------|------|---------|
| Group workspace | `/workspace/group` | rw | Group's persistent storage (memory, files) |
| Global workspace | `/workspace/global` | ro | Shared read-only resources (non-main only) |
| Project meta | `/workspace/project` | ro | System docs, config (main only) |
| IPC socket | `/workspace/ipc.sock` | rw | Host-container communication |
| Session state | `/home/agent/.sessions` | rw | Agent SDK session persistence |
| UV cache | `/home/agent/.cache/uv` | rw | Python package cache (persistent) |
| Additional mounts | varies | varies | User-configured via mount allowlist |

### Mount Security

Mounts are validated against an external allowlist file that is never mounted into the container:

```rust
pub struct MountSecurity {
    /// Allowed mount source paths (loaded from external file)
    allowlist: Vec<AllowedPath>,

    /// Patterns that are always blocked (e.g., /etc/shadow, .env)
    blocked_patterns: Vec<String>,
}

impl MountSecurity {
    /// Validate a requested mount against the allowlist
    pub fn validate(&self, mount: &Mount) -> Result<()> {
        // 1. Resolve symlinks to real path
        // 2. Check against blocked patterns
        // 3. Check against allowlist
        // 4. Verify path doesn't escape allowed directories
    }
}
```

## Warm Pool

The warm pool maintains pre-provisioned containers in the `Ready` state, available for immediate assignment.

```rust
pub struct WarmPool {
    /// Target pool size (global or per-group)
    target_size: usize,

    /// Containers currently in Ready state, available for assignment
    ready: Vec<ContainerHandle>,

    /// Containers being provisioned to replenish the pool
    provisioning: usize,

    /// TTL for idle containers before eviction
    idle_ttl: Duration,
}
```

### Pool Behavior

**Startup**: Spawn `target_size` containers. Each provisions, starts, and completes IPC handshake. Pool is "warm" when all reach `Ready`.

**Assignment**: When a group needs a container:
1. Check pool for a ready container → assign immediately (warm hit)
2. If pool empty → provision new container (cold start) + queue pool replenishment

**Replenishment**: When a container is consumed from the pool, a background task provisions a replacement. The pool self-heals to `target_size`.

**Eviction**: Containers idle beyond `idle_ttl` are drained and removed. This prevents resource waste when the system is quiet.

**Per-group pools** (optional): If configured, each group maintains its own pool. This ensures hot groups always have a warm container, even if other groups are consuming pool capacity.

### Metrics

| Metric | Type | Description |
|--------|------|-------------|
| `forgeclaw_warm_pool_size` | gauge | Current ready containers in pool |
| `forgeclaw_warm_pool_hits_total` | counter | Requests served from warm pool |
| `forgeclaw_warm_pool_misses_total` | counter | Requests that required cold start |
| `forgeclaw_container_cold_start_seconds` | histogram | Cold start provisioning duration |
| `forgeclaw_container_warm_start_seconds` | histogram | Warm start assignment duration |

## Concurrency Control

```rust
pub struct GroupQueue {
    /// Per-group state
    groups: HashMap<GroupId, GroupState>,

    /// Global concurrency limit
    max_concurrent: usize,

    /// Currently active containers (across all groups)
    active_count: AtomicUsize,

    /// Groups waiting for capacity (FIFO)
    waiting: VecDeque<GroupId>,
}

pub struct GroupState {
    /// Current container lifecycle state
    container: Option<ContainerState>,

    /// Pending messages waiting for container capacity
    pending_messages: VecDeque<PendingJob>,

    /// Pending tasks waiting for container capacity
    pending_tasks: VecDeque<PendingJob>,

    /// Error tracking
    errors: ErrorTracker,
}
```

**Concurrency enforcement**: The `active_count` is an `AtomicUsize` — incremented atomically when a container transitions to `Processing`, decremented when it transitions to `Idle` or `Exited`. No race condition between concurrent enqueue calls.

**Backpressure**: When at `max_concurrent`, new requests are queued per-group. When capacity frees up, the first waiting group is dequeued (FIFO). Queued messages have a bounded buffer — if the buffer fills, the oldest message is dropped with a notification to the channel.

## Network Policy

Agent containers get group-specific network access:

```rust
pub struct NetworkPolicy {
    /// Compose services this group can reach
    pub allowed_services: Vec<String>,

    /// External network access
    pub external_access: ExternalAccess,
}

pub enum ExternalAccess {
    /// Full internet access
    Full,
    /// DNS-only (can resolve but not connect)
    DnsOnly,
    /// No external access
    None,
}
```

**Implementation**: The host creates a Docker network per group (or per policy fingerprint, to share identical policies). The agent container and allowed compose services are attached to this network. Services not on the network are unreachable.

## Cleanup Guarantees

**On normal exit**: Container transitions through `Draining → Exited`. Host removes the container and deregisters the credential proxy token.

**On error**: Container transitions to `Failed`. Host stops the container (with timeout), removes it, and deregisters the token.

**On host crash**: On next startup, the orphan cleanup routine finds containers labeled `forgeclaw.managed=true` with a stale instance ID. These are stopped and removed.

**On host shutdown (SIGTERM)**: Graceful shutdown drains the queue (no new work accepted), sends `shutdown` to all active containers, waits for drain timeout, then force-stops any remaining containers.

No container is ever left running without a managing host process.
