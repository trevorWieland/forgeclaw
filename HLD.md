# Forgeclaw: High-Level Design

## System Identity

Forgeclaw is a compose-native AI runtime — a Docker service that orchestrates intelligent agent containers, routes messages across communication channels, manages multiple model providers, and integrates deeply with Tanren for code creation and self-improvement. It runs as part of a Docker Compose stack, discovering and interacting with sibling services.

## Architecture Overview

### The Three Execution Modes

Forgeclaw orchestrates three distinct types of work:

**Interactive Agents** — Message-driven agent containers managed directly by Forgeclaw. Messages arrive via channels, are routed to a group, an agent container spins up (or is pulled from the warm pool), processes the conversation via the configured model provider, and responds. These containers have bidirectional IPC with the host and can send messages, schedule tasks, and dispatch Tanren jobs.

**Tanren Dispatches** — Code creation, validation, and auditing jobs dispatched to Tanren. Forgeclaw submits work via the Tanren API. Tanren provisions the execution environment (local DooD container, remote VM), runs the agent through its phase lifecycle (do-task → gate → audit), and reports results back. Forgeclaw monitors dispatch status and routes results to channels.

**Self-Improvement** — A feedback loop where interactive agents identify capability gaps and dispatch Tanren jobs targeting Forgeclaw's own codebase. Tanren provisions an environment, the agent makes changes, gates run (cargo test, clippy, fmt), and if green, the result is available for review or auto-deploy.

### System Diagram

```
┌─────────────────────────────────────────────────────────────┐
│                      Docker Compose Stack                    │
│                                                              │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌────────────┐  │
│  │ postgres │  │  tanren  │  │ your apps│  │  others    │  │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘  └─────┬──────┘  │
│       │              │             │               │         │
│  ─────┴──────────────┴─────────────┴───────────────┴─────── │
│                     compose network                          │
│  ───────────────────────┬────────────────────────────────── │
│                         │                                    │
│  ┌──────────────────────┴───────────────────────────────┐   │
│  │               FORGECLAW (host container)              │   │
│  │                                                       │   │
│  │  ┌──────────────────────────────────────────────┐    │   │
│  │  │              Event Bus (tokio)                │    │   │
│  │  └──┬───────┬───────┬───────┬───────┬───────┬──┘    │   │
│  │     │       │       │       │       │       │        │   │
│  │  ┌──┴──┐ ┌──┴──┐ ┌──┴──┐ ┌──┴──┐ ┌──┴──┐ ┌──┴──┐  │   │
│  │  │chan-│ │rout-│ │prov-│ │cont-│ │tanr-│ │heal-│  │   │
│  │  │nels│ │ er  │ │ider│ │ainer│ │ en  │ │ th  │  │   │
│  │  └─────┘ └─────┘ └─────┘ └──┬──┘ └─────┘ └─────┘  │   │
│  │                              │                       │   │
│  │              Docker Socket (/var/run/docker.sock)     │   │
│  └──────────────────────┬───────────────────────────────┘   │
│                         │                                    │
│           ┌─────────────┼─────────────┐                     │
│           │             │             │                      │
│     ┌─────┴─────┐ ┌────┴────┐ ┌─────┴─────┐               │
│     │  Agent    │ │  Agent  │ │  Tanren   │               │
│     │Container A│ │Container│ │ Container │               │
│     │(Discord  )│ │(Telegram│ │(code work)│               │
│     │ group-1  )│ │ group-2)│ │           │               │
│     └───────────┘ └─────────┘ └───────────┘               │
│                                                              │
└──────────────────────────────────────────────────────────────┘
```

## Crate Structure

Forgeclaw is a Cargo workspace with 12 crates plus the agent-runner:

| Crate | Responsibility |
|-------|---------------|
| `core` | Shared types, event bus, error taxonomy, configuration loading |
| `providers` | Multi-model abstraction: provider trait, Anthropic/OpenAI/Ollama implementations, token budgets, fallback chains |
| `channels` | Channel trait, channel registry, message types. Implementations (Discord, etc.) as feature flags |
| `container` | Container lifecycle state machine, runtime trait (Docker/Podman/Apple Container), mount builder, network policy |
| `ipc` | Structured IPC protocol over Unix domain sockets. Frame codec, message types, server/client |
| `auth` | Multi-provider credential proxy (hyper server), circuit breaker, rate limiting, token management |
| `store` | sqlx database layer with compile-time checked queries. SQLite + Postgres. Messages, tasks, state, events |
| `scheduler` | Task scheduling: cron, interval, once. Task lifecycle management |
| `router` | Message pipeline: ingest → trigger detection → context window building → dispatch |
| `queue` | Group concurrency control, warm container pool, backpressure, retry with classified errors |
| `tanren` | Tanren API client, dispatch builder, event polling, self-improvement dispatch |
| `health` | Health source trait, Prometheus metrics, status server, compose healthcheck |

Plus:

| Component | Responsibility |
|-----------|---------------|
| `bin/` | Main binary — thin composition root. Startup orchestration, signal handling, graceful shutdown |
| `agent-runner/` | Container-side IPC client + agent adapter framework. Separate build target for agent container image |
| `ipc-adapter-js/` | TypeScript/JS IPC adapter library for wrapping JS-based agent SDKs (Claude Code, OpenAI Agents) |

### Dependency Graph

```
bin
 ├── core
 ├── providers
 ├── channels
 ├── container ── ipc
 ├── auth ── providers
 ├── store ── core
 ├── scheduler ── store, queue
 ├── router ── store, channels, queue
 ├── queue ── container, core
 ├── tanren ── core, store
 └── health ── core, providers, container, tanren
```

No circular dependencies. `core` depends on nothing. Every other crate depends on `core` (implicitly or explicitly). `bin` depends on everything.

## Key Data Flows

### Message Processing

```
1. Channel receives message (Discord mention, Telegram command, etc.)
2. Channel emits Event::Message { channel, group, message }
3. Router subscribes, evaluates:
   - Is this a registered group?
   - Does the message match the trigger pattern?
   - Is the sender authorized?
   - Build context window (messages since last agent response)
4. Router emits Event::AgentRequested { group, context }
5. Queue subscribes, evaluates concurrency:
   - Is there a warm container for this group? → reuse
   - Under concurrency limit? → provision new
   - At limit? → enqueue, wait for capacity
6. Container manager provisions/reuses container:
   - Build spec (mounts, network, env, provider proxy URL)
   - Create via bollard Docker API
   - Establish IPC (Unix socket handshake)
   - Send Init { context, config }
7. Agent runner (in container) processes:
   - Connects to provider proxy with group token
   - Streams conversation through configured model
   - Executes tool calls
   - Sends OutputDelta events back via IPC
8. Container manager receives output:
   - Streams to router
   - Router formats and sends to channel
   - Updates cursor in store
9. Container transitions to Idle (available for reuse) or Exited
```

### Tanren Dispatch

```
1. Interactive agent (via IPC) or scheduler triggers a Tanren dispatch
2. Tanren crate validates:
   - Is the requesting group authorized for Tanren dispatches?
   - Is the dispatch within budget?
3. Tanren crate submits dispatch to Tanren API:
   - Project, branch, phase, environment profile
   - Tanren provisions environment (local DooD or remote VM)
   - Tanren executes agent through phase lifecycle
4. Forgeclaw polls dispatch status (or receives webhook)
5. On completion:
   - Result routed to originating channel
   - Metrics updated (tokens used, duration, outcome)
   - If self-improvement: trigger image rebuild flow
```

### Self-Improvement

```
1. Interactive agent identifies capability gap
   ("I need PDF parsing but I don't have that tool")
2. Agent sends IPC command: DispatchSelfImprovement {
     objective: "Add PDF text extraction tool",
     scope: ["agent-runner/src/tools/"],
     acceptance: ["cargo test", "cargo clippy"],
   }
3. Forgeclaw validates, dispatches to Tanren with:
   - project: "forgeclaw"
   - branch: "feature/pdf-tool"
   - phase: do-task → gate → audit-task
4. Tanren provisions environment, agent implements feature
5. Gates pass (cargo test, clippy, fmt)
6. Audit agent reviews changes
7. Result: branch pushed, PR created
8. Forgeclaw notifies originating channel: "PR #42 ready"
9. (Optional) Auto-rebuild agent image, restart containers
```

## Security Model

### Trust Hierarchy

| Entity | Trust Level | Capabilities |
|--------|------------|-------------|
| Host process | Full | All operations, all groups, all providers |
| Main group containers | High | Send to any group, register groups, dispatch Tanren |
| Non-main group containers | Scoped | Send to own group only, limited IPC commands |
| Tanren dispatches | Scoped | Access to specified project only, gated by Tanren |

### Isolation Boundaries

**Container isolation**: Each group gets its own container with its own filesystem, network policy, and IPC channel. Containers run as non-root. Mounts are validated against an allowlist.

**Credential isolation**: Containers never see API keys or OAuth tokens. The provider proxy injects credentials per-request based on a per-container token. Tokens are random, single-use per container lifecycle.

**Network isolation**: Agent containers are attached to group-specific Docker networks. Each group's config declares which compose services the group can reach. A guest group might only reach Forgeclaw itself; the main group might reach postgres and tanren.

**IPC authorization**: Every IPC command carries the source group identity (derived from the socket connection, not from the message content — unforgeable). Main groups can send cross-group messages and dispatch Tanren jobs. Non-main groups are scoped to their own group.

## Configuration Model

### Layered TOML

```
~/.config/forgeclaw/config.toml    # User-level defaults
./forgeclaw.toml                    # Project/deployment config
Environment variables               # Overrides (FORGECLAW_* prefix)
```

Merge precedence: env > project > user.

### Top-Level Config Structure

```toml
[runtime]
data_dir = "/data"
log_level = "info"
max_concurrent_containers = 5
warm_pool_size = 2

[store]
backend = "sqlite"  # or "postgres"
url = "sqlite:///data/forgeclaw.db"  # or postgres://...

[providers.anthropic]
type = "anthropic"
# Credentials loaded from environment or secret store, never from config file

[providers.ollama]
type = "openai_compat"
base_url = "http://ollama:11434/v1"

[channels.discord]
type = "discord"
# Credentials loaded from environment: FORGECLAW_DISCORD_TOKEN

[groups.main]
provider = "anthropic"
model = "claude-sonnet-4-20250514"
fallback = [{ provider = "ollama", model = "llama3" }]
channel = "discord"
is_main = true
token_budget = { daily = 1_000_000, monthly = 20_000_000 }
allowed_compose_services = ["postgres", "tanren", "redis"]

[groups.guest]
provider = "ollama"
model = "llama3"
channel = "discord"
is_main = false
allowed_compose_services = []

[tanren]
api_url = "http://tanren:8000"
# API key from environment: FORGECLAW_TANREN_API_KEY

[container]
image = "ghcr.io/you/forgeclaw-agent:latest"
timeout = "30m"
idle_ttl = "5m"
memory_limit = "4g"
cpu_limit = 2
```

### Secrets

Credentials are never stored in config files. They come from:
1. Environment variables (`FORGECLAW_ANTHROPIC_API_KEY`, `FORGECLAW_OPENAI_API_KEY`, etc.)
2. Docker secrets (mounted files)
3. A future secret provider adapter (Vault, cloud secret managers)

## Deployment Model

### Docker Compose (Primary)

```yaml
services:
  forgeclaw:
    image: ghcr.io/you/forgeclaw:latest
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock
      - forgeclaw-data:/data
      - ./forgeclaw.toml:/config/forgeclaw.toml:ro
    environment:
      - FORGECLAW_ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY}
      - FORGECLAW_TANREN_API_KEY=${TANREN_API_KEY}
      - FORGECLAW_DISCORD_TOKEN=${DISCORD_TOKEN}
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:9090/health"]
      interval: 30s
    depends_on:
      - tanren
      - postgres

  tanren:
    image: ghcr.io/you/tanren:latest
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock
      - tanren-data:/data

  postgres:
    image: postgres:17
    volumes:
      - pg-data:/var/lib/postgresql/data
```

Forgeclaw and Tanren are peers in the stack. Both access the Docker socket for DooD. Agent containers are sibling containers on the compose network.

### Kubernetes (Future)

The architecture doesn't preclude k8s deployment. The container runtime trait can be implemented for the Kubernetes API (create pods instead of Docker containers). Network policies map to k8s NetworkPolicy resources. But Docker Compose is the primary target.
