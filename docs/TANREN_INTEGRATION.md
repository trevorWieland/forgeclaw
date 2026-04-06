# Tanren Integration

## Overview

Tanren (鍛錬, "forge/temper") is an opinionated orchestration engine for agentic software development. Forgeclaw integrates with Tanren as a first-class execution backend for code creation, validation, auditing, and self-improvement.

Forgeclaw manages two types of containers:
1. **Interactive agent containers** — managed directly by Forgeclaw's container system. Message-driven, IPC-connected, warm-pooled.
2. **Tanren dispatch containers** — managed by Tanren. Forgeclaw submits work and monitors results. Tanren handles provisioning, execution, gating, and teardown.

This separation is deliberate. Interactive agents and code work have different lifecycle requirements — conflating them would compromise both.

## Integration Architecture

```
┌───────────────────────────────────────────────────┐
│                  FORGECLAW HOST                     │
│                                                     │
│  ┌─────────────┐     ┌──────────────────┐          │
│  │  Interactive │     │  Tanren Client   │          │
│  │  Agent       │────►│                  │          │
│  │  (via IPC    │     │  • dispatch()    │          │
│  │   command)   │     │  • poll_status() │          │
│  └─────────────┘     │  • query_events()│          │
│                       │  • query_metrics()          │
│                       └────────┬─────────┘          │
│                                │ HTTP API            │
└────────────────────────────────┼─────────────────────┘
                                 │
                                 ▼
                       ┌──────────────────┐
                       │   TANREN SERVICE  │
                       │  (compose peer)   │
                       │                   │
                       │  Dispatch →       │
                       │  Provision →      │
                       │  Execute →        │
                       │  Gate →           │
                       │  Teardown         │
                       └────────┬──────────┘
                                │ DooD or SSH
                       ┌────────┴──────────┐
                       │  Tanren Container  │
                       │  or Remote VM      │
                       │                    │
                       │  Agent executes:   │
                       │  do-task / gate /  │
                       │  audit-task / etc  │
                       └───────────────────┘
```

## Tanren Client

```rust
pub struct TanrenClient {
    base_url: Url,
    api_key: String,
    http: reqwest::Client,
}

impl TanrenClient {
    // === Dispatch (fire-and-forget or step-by-step) ===

    /// Submit a full dispatch (provision → execute → teardown auto-chained)
    pub async fn dispatch_full(&self, req: &DispatchRequest) -> Result<DispatchId>;

    /// Submit a manual dispatch (caller controls each step)
    pub async fn dispatch_manual(&self, req: &DispatchRequest) -> Result<DispatchId>;

    /// Get dispatch status
    pub async fn dispatch_status(&self, id: &DispatchId) -> Result<DispatchStatus>;

    /// Cancel a dispatch
    pub async fn dispatch_cancel(&self, id: &DispatchId) -> Result<()>;

    // === Step-by-step control (for manual mode) ===

    pub async fn provision(&self, req: &ProvisionRequest) -> Result<EnvId>;
    pub async fn execute(&self, env_id: &EnvId, req: &ExecuteRequest) -> Result<()>;
    pub async fn teardown(&self, env_id: &EnvId) -> Result<()>;
    pub async fn run_status(&self, env_id: &EnvId) -> Result<RunStatus>;

    // === Observability ===

    pub async fn query_events(&self, query: &EventQuery) -> Result<Vec<TanrenEvent>>;
    pub async fn metrics_summary(&self) -> Result<MetricsSummary>;
    pub async fn metrics_costs(&self) -> Result<CostSummary>;
    pub async fn metrics_vms(&self) -> Result<VmMetrics>;

    // === VM management ===

    pub async fn list_vms(&self) -> Result<Vec<VmInfo>>;
    pub async fn release_vm(&self, id: &str) -> Result<()>;

    // === Health ===

    pub async fn health(&self) -> Result<TanrenHealth>;
    pub async fn readiness(&self) -> Result<bool>;
}
```

## Dispatch Types

### Code Work Dispatch

An interactive agent (or the scheduler) dispatches code work to Tanren:

```rust
pub struct CodeWorkDispatch {
    /// Target repository
    pub project: String,

    /// Git branch for the work
    pub branch: String,

    /// Tanren phase
    pub phase: TanrenPhase,

    /// Prompt / task description
    pub prompt: String,

    /// Environment profile (local, remote, docker)
    pub environment_profile: String,

    /// Which CLI/agent to use (auto-resolved from roles.yml if None)
    pub cli: Option<String>,

    /// Timeout for the entire dispatch
    pub timeout: Duration,
}

pub enum TanrenPhase {
    DoTask,       // Implement changes
    Gate,         // Run tests/lint/build
    AuditTask,    // Code review
    RunDemo,      // Verify behavior
    AuditSpec,    // Full spec audit
    Investigate,  // Root-cause analysis
}
```

### Self-Improvement Dispatch

A specialized dispatch targeting Forgeclaw's own codebase:

```rust
pub struct SelfImprovementDispatch {
    /// What to change
    pub objective: String,

    /// Which files/modules to touch (scope guard)
    pub scope: Vec<String>,

    /// Commands that must pass for the change to be accepted
    pub acceptance_tests: Vec<String>,

    /// Branch naming policy
    pub branch_policy: BranchPolicy,

    /// Whether to auto-create a PR on success
    pub create_pr: bool,
}

pub enum BranchPolicy {
    /// Create a new feature branch: feature/<slug>
    FeatureBranch { slug: String },
    /// Work on an existing branch
    ExistingBranch { name: String },
}
```

**Validation rules for self-improvement:**
- Must target the Forgeclaw repository (hardcoded project reference)
- Must include acceptance tests (`cargo test` at minimum)
- Scope must be specified (no unrestricted modifications)
- Only main group can dispatch self-improvement
- Result is always a branch + optional PR — never auto-merged

## IPC Commands

Interactive agents dispatch Tanren work through IPC commands:

```json
// Code work dispatch
{
  "type": "command",
  "command": "dispatch_tanren",
  "payload": {
    "project": "my-project",
    "branch": "feature/new-api",
    "phase": "do_task",
    "prompt": "Implement the /users endpoint per the spec in docs/api.md",
    "environment_profile": "local",
    "timeout_seconds": 3600
  }
}

// Self-improvement dispatch
{
  "type": "command",
  "command": "dispatch_self_improvement",
  "payload": {
    "objective": "Add PDF text extraction tool to the agent-runner",
    "scope": ["agent-runner/src/tools/"],
    "acceptance_tests": ["cargo test --workspace", "cargo clippy -- -D warnings"],
    "branch_policy": { "type": "feature_branch", "slug": "pdf-tool" },
    "create_pr": true
  }
}
```

## Authorization

| Command | Who can dispatch | Validation |
|---------|-----------------|------------|
| `dispatch_tanren` | Groups with `tanren_dispatch = true` in config | Project must exist, branch must be valid |
| `dispatch_self_improvement` | Main group only | Must target Forgeclaw repo, must include acceptance tests, scope required |

## Result Routing

When a Tanren dispatch completes, the result is routed back to the originating context:

1. **Completion polling**: Forgeclaw polls `dispatch_status()` periodically (every 30s) for active dispatches
2. **Result classification**: Map Tanren outcome to Forgeclaw event:
   - `success` → `TanrenEvent::DispatchCompleted { outcome: Success }`
   - `fail` → `TanrenEvent::DispatchCompleted { outcome: Failed }`
   - `timeout` → `TanrenEvent::DispatchCompleted { outcome: TimedOut }`
   - `error` → `TanrenEvent::DispatchCompleted { outcome: Error }`
3. **Channel notification**: Post result summary to the originating channel/group
4. **Self-improvement handling**: If the dispatch was self-improvement and succeeded:
   - Post PR link to the main group's channel
   - (Optional, configurable) Trigger agent image rebuild

## Result Message Format

```
✅ Tanren dispatch completed: feature/new-api
Phase: do-task → gate → audit-task
Outcome: success
Duration: 12m 34s
Tokens: 45,230 input / 8,102 output
Branch pushed: feature/new-api
PR: https://github.com/you/my-project/pull/42
```

Or on failure:

```
❌ Tanren dispatch failed: feature/new-api
Phase: gate (failed at this phase)
Outcome: fail
Signal: test_failure
Duration: 3m 12s
Tail output:
  FAILED tests/api_test.rs::test_users_endpoint
  expected 200, got 404
```

## Tanren Health Source

Tanren connectivity is monitored as a health source:

```rust
pub struct TanrenHealthSource {
    client: TanrenClient,
    poll_interval: Duration,
}

#[async_trait]
impl HealthSource for TanrenHealthSource {
    fn name(&self) -> &str { "tanren" }

    async fn check(&self) -> HealthStatus {
        match self.client.readiness().await {
            Ok(true) => HealthStatus::Healthy,
            Ok(false) => HealthStatus::Degraded("not ready".into()),
            Err(e) => HealthStatus::Unhealthy(e.to_string()),
        }
    }
}
```

## Metrics

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `forgeclaw_tanren_dispatches_total` | counter | phase, outcome | Dispatches submitted |
| `forgeclaw_tanren_dispatch_duration_seconds` | histogram | phase | Dispatch wall-clock time |
| `forgeclaw_tanren_tokens_used` | counter | direction | Tokens consumed by Tanren dispatches |
| `forgeclaw_tanren_self_improvement_total` | counter | outcome | Self-improvement dispatches |
| `forgeclaw_tanren_active_dispatches` | gauge | — | Currently running dispatches |

## Configuration

```toml
[tanren]
api_url = "http://tanren:8000"
# API key from env: FORGECLAW_TANREN_API_KEY
poll_interval = "30s"
max_concurrent_dispatches = 3

[tanren.self_improvement]
enabled = true
project = "forgeclaw"
repo_url = "https://github.com/you/forgeclaw.git"
default_acceptance = ["cargo test --workspace", "cargo clippy -- -D warnings", "cargo fmt --check"]
auto_pr = true
auto_rebuild = false  # safety: require manual approval for self-modifications
```

## Self-Improvement Loop (Detailed)

```
Step 1: Agent identifies need
  Interactive agent (main group) realizes it lacks a capability.
  Example: "I can't read PDFs. I should be able to."

Step 2: Agent dispatches self-improvement
  → IPC command: dispatch_self_improvement
  → Forgeclaw validates: main group, Forgeclaw repo, has acceptance tests

Step 3: Forgeclaw submits to Tanren
  → POST /api/v1/dispatch (AUTO mode)
  → project: forgeclaw, branch: feature/pdf-tool, phase: do-task
  → Tanren provisions local DooD container (or remote VM for heavy work)

Step 4: Tanren executes
  → Provision: create worktree, checkout branch
  → Do-task: agent implements PDF tool in agent-runner/src/tools/
  → Gate: cargo test --workspace, cargo clippy, cargo fmt --check
  → Audit-task: code review agent checks quality, security, scope
  → Teardown: cleanup worktree

Step 5: Result flows back
  → Forgeclaw polls dispatch_status → completed, outcome: success
  → Branch pushed: feature/pdf-tool
  → PR created (if auto_pr = true)
  → Main group channel notified: "PR #42: Add PDF tool — gates passed, audit passed"

Step 6: (Optional) Rebuild
  → If auto_rebuild = true: Forgeclaw triggers docker build for agent image
  → New containers use updated image
  → If auto_rebuild = false: human reviews PR, merges, triggers rebuild manually
```

The loop is intentionally conservative. Self-modifications always land on a branch, always run gates, always get audited, and never auto-merge. The human remains in the loop for the merge decision (unless explicitly configured otherwise).
