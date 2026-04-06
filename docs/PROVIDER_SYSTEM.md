# Provider System

## Overview

The provider system abstracts model access behind a unified interface. Every group has a provider chain (primary + fallbacks), token budgets are tracked per-pool, and the credential proxy routes container requests to the correct provider transparently.

## Provider Trait

```rust
#[async_trait]
pub trait Provider: Send + Sync + 'static {
    /// Unique identifier for this provider instance
    fn id(&self) -> &ProviderId;

    /// Human-readable name
    fn name(&self) -> &str;

    /// Models available through this provider
    fn models(&self) -> &[ModelSpec];

    /// Stream a completion request. Returns structured events.
    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderEvent>> + Send>>>;

    /// Estimate token count for a set of messages.
    /// Used for budget tracking and context window management.
    fn estimate_tokens(&self, messages: &[Message], tools: &[ToolSpec]) -> TokenEstimate;

    /// Health check for this provider
    async fn health(&self) -> ProviderHealth;
}
```

## Built-In Providers

### Anthropic

Talks to the Anthropic Messages API (`/v1/messages`). Supports:
- Streaming SSE responses
- Tool use (function calling)
- System prompts
- Token counting via Anthropic's tokenizer
- Extended thinking

**Auth**: API key (`x-api-key` header) or OAuth token (`Authorization: Bearer`).

### OpenAI-Compatible

Talks to any API that implements the OpenAI Chat Completions format (`/v1/chat/completions`). Covers:
- OpenAI (GPT-4, o3, etc.)
- Ollama (local models)
- vLLM (self-hosted inference)
- Together AI, Groq, Fireworks, etc.

**Auth**: Bearer token (`Authorization: Bearer`) or none (for local Ollama).

**Tool format translation**: OpenAI-format tool calls are translated to/from the normalized internal format. The agent runner never sees provider-specific tool schemas.

## Provider Registry

```rust
pub struct ProviderRegistry {
    /// All registered providers
    providers: HashMap<ProviderId, Arc<dyn Provider>>,

    /// Per-group provider chain (primary + fallbacks)
    chains: HashMap<GroupId, ProviderChain>,

    /// Token budget pools (groups can share pools)
    budgets: HashMap<PoolId, Arc<TokenBudget>>,

    /// Provider health state
    health: HashMap<ProviderId, ProviderHealthState>,
}
```

### Provider Chain

Each group has an ordered list of providers to try:

```rust
pub struct ProviderChain {
    /// Primary provider + model
    pub primary: ProviderModelPair,

    /// Ordered fallback list
    pub fallbacks: Vec<ProviderModelPair>,

    /// Budget pool this chain draws from
    pub budget_pool: PoolId,
}

pub struct ProviderModelPair {
    pub provider: ProviderId,
    pub model: String,
}
```

**Resolution order:**
1. Try primary provider + model
2. If primary fails (error, rate limit, timeout): try first fallback
3. If budget exhausted: try first fallback (different budget action per pool)
4. Continue through fallback list until one succeeds or all fail

### Fallback Triggers

| Trigger | Action |
|---------|--------|
| HTTP 429 (rate limited) | Immediate fallback, no retry |
| HTTP 5xx (server error) | Retry once with 1s backoff, then fallback |
| Connection timeout | Immediate fallback |
| Budget exhausted | Fallback if `budget_action = Fallback`; pause if `Pause`; notify if `Notify` |
| Provider health degraded | Deprioritize in chain (move to end of fallback list) |

Fallback is transparent to the agent. The provider proxy handles it — the container doesn't know its request was rerouted.

## Token Budget Pools

```rust
pub struct TokenBudget {
    /// Unique pool identifier
    pub id: PoolId,

    /// Daily token limit (resets at midnight in configured timezone)
    pub daily_limit: Option<u64>,

    /// Monthly token limit (resets on the 1st)
    pub monthly_limit: Option<u64>,

    /// What to do when budget is exhausted
    pub action_on_exhaust: BudgetAction,

    /// Alert thresholds (emit events at these percentages)
    pub alert_thresholds: Vec<f64>,  // e.g., [0.80, 0.95, 1.0]

    // Runtime state
    used_today: AtomicU64,
    used_this_month: AtomicU64,
    reset_date: AtomicI64,
}

pub enum BudgetAction {
    /// Switch to fallback provider
    Fallback,
    /// Pause the group until budget resets
    Pause,
    /// Notify the channel but continue (soft limit)
    Notify,
}
```

**Shared pools**: Multiple groups can share a budget pool. Example: all "guest" groups share a 100K daily token pool. The main group has its own 1M daily pool.

**Tracking**: Token usage is recorded per-request via the credential proxy. The proxy increments the budget counter before forwarding the response. If the response would exceed the budget, the proxy can preemptively fail the request (for `Pause` action) or let it through and trigger fallback for subsequent requests.

## Credential Proxy

The credential proxy is an HTTP server running on the host. Agent containers talk to it instead of talking directly to model providers.

### Request Flow

```
Agent Container
  │
  │  POST http://host:9090/v1/messages
  │  Headers:
  │    X-Container-Token: <per-container random token>
  │    Content-Type: application/json
  │  Body: { model: "...", messages: [...], tools: [...] }
  │
  ▼
Credential Proxy
  │
  ├── Extract container token
  ├── Resolve: token → group → provider chain
  ├── Check budget: enough tokens remaining?
  │   ├── Yes → continue
  │   └── No → apply budget action (fallback/pause/notify)
  ├── Check rate limit: within per-container window?
  │   ├── Yes → continue
  │   └── No → 429 response
  ├── Select provider (primary or fallback)
  ├── Translate request format (if needed: normalize → provider-native)
  ├── Inject credentials (API key header or OAuth bearer)
  ├── Forward to provider
  ├── Stream response back to container
  ├── Translate response format (if needed: provider-native → normalize)
  ├── Record token usage to budget pool
  └── Record metrics (latency, tokens, provider, model)
```

### Token Management

- **Registration**: Before spawning a container, the host generates a 32-byte random token (hex-encoded, 64 chars) and registers it in the proxy's token map.
- **Resolution**: Token → `{ group_id, container_id, provider_chain }`.
- **Deregistration**: When a container exits (any state), the host deregisters the token. Subsequent requests with that token are rejected.
- **No reuse**: Tokens are single-use per container lifecycle. A new container gets a new token.

### Request Validation

- **Path allowlist**: Only `/v1/messages`, `/v1/chat/completions`, and their streaming variants
- **Content-Type**: Must be `application/json`
- **Body size**: Maximum 10 MiB
- **Path traversal**: URL-decoded segments checked for `..`
- **Invalid token**: 401 response (no retry, container should report fatal error)

### Format Translation

The proxy normalizes between provider API formats:

| From (container sends) | To (provider expects) |
|------------------------|----------------------|
| Anthropic Messages format | Anthropic Messages API (passthrough) |
| Anthropic Messages format | OpenAI Chat Completions (translate) |
| OpenAI Chat Completions format | OpenAI API (passthrough) |
| OpenAI Chat Completions format | Anthropic Messages API (translate) |

The container's adapter determines which format it sends. The proxy handles the rest. This means a Claude Code adapter (which speaks Anthropic format) can be backed by an OpenAI provider if that's what the group's chain specifies.

### Circuit Breaker

Per-provider circuit breaker:

```
Closed (healthy)
  │ N failures in window
  ▼
Open (tripped)
  │ cooldown elapsed
  ▼
Half-Open (probing)
  │ success → Closed
  │ failure → Open
```

- **Failure threshold**: 5 failures in 60 seconds
- **Cooldown**: 30 seconds
- **Half-open probe**: Single request allowed through; if it succeeds, circuit closes

When a circuit is open, requests to that provider fail immediately with a structured error. The provider chain's fallback logic kicks in.

## Configuration

```toml
[providers.anthropic]
type = "anthropic"
# Key from env: FORGECLAW_PROVIDER_ANTHROPIC_API_KEY

[providers.openai]
type = "openai_compat"
base_url = "https://api.openai.com/v1"
# Key from env: FORGECLAW_PROVIDER_OPENAI_API_KEY

[providers.ollama]
type = "openai_compat"
base_url = "http://ollama:11434/v1"
# No auth needed for local Ollama

[budget_pools.main]
daily_limit = 2_000_000
monthly_limit = 50_000_000
action_on_exhaust = "fallback"
alert_thresholds = [0.80, 0.95]

[budget_pools.guests]
daily_limit = 100_000
monthly_limit = 2_000_000
action_on_exhaust = "pause"
alert_thresholds = [0.90]

[groups.main]
provider = "anthropic"
model = "claude-sonnet-4-20250514"
fallback = [
  { provider = "openai", model = "gpt-4o" },
  { provider = "ollama", model = "llama3" },
]
budget_pool = "main"

[groups.guest-room]
provider = "ollama"
model = "llama3"
fallback = []
budget_pool = "guests"
```

## Metrics

The provider system emits the following Prometheus metrics:

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `forgeclaw_provider_requests_total` | counter | provider, model, group, status | Total requests per provider |
| `forgeclaw_provider_latency_seconds` | histogram | provider, model | Request latency distribution |
| `forgeclaw_provider_tokens_used` | counter | provider, model, group, direction | Tokens consumed (input/output) |
| `forgeclaw_budget_usage_ratio` | gauge | pool | Current usage as fraction of limit |
| `forgeclaw_budget_exhausted_total` | counter | pool, action | Budget exhaustion events |
| `forgeclaw_provider_fallback_total` | counter | from_provider, to_provider, reason | Fallback events |
| `forgeclaw_provider_circuit_state` | gauge | provider | Circuit breaker state (0=closed, 1=open, 2=half-open) |
