//! Configuration sub-types for each Forgeclaw subsystem.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Runtime configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    /// Path to the data directory (e.g. for `SQLite` databases).
    pub data_dir: String,
    /// Tracing log level filter (default: `"info"`).
    #[serde(default = "default_log_level")]
    pub log_level: String,
    /// Maximum number of containers running concurrently.
    pub max_concurrent_containers: usize,
    /// Number of warm (pre-provisioned) containers to keep in the pool.
    pub warm_pool_size: usize,
    /// Event bus channel capacity (default: 256).
    ///
    /// Controls how many events can be buffered before slow receivers
    /// start lagging. Higher values trade memory for tolerance of
    /// subscriber backpressure.
    #[serde(default = "default_event_bus_capacity")]
    pub event_bus_capacity: usize,
}

fn default_event_bus_capacity() -> usize {
    256
}

fn default_log_level() -> String {
    "info".to_owned()
}

/// Database / persistence backend configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreConfig {
    /// Which database backend to use.
    pub backend: StoreBackend,
    /// Connection URL for the database.
    pub url: String,
}

/// Supported database backends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StoreBackend {
    /// `SQLite` (file-based).
    Sqlite,
    /// `PostgreSQL`.
    Postgres,
}

/// Configuration for a single LLM provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    /// Provider type — must be one of the known [`ProviderKind`] variants.
    #[serde(rename = "type")]
    pub kind: ProviderKind,
    /// Optional custom base URL (for self-hosted providers like Ollama).
    pub base_url: Option<String>,
}

/// Known LLM provider types.
///
/// Using a typed enum instead of a raw `String` catches typos at
/// deserialization time (compile-time correctness, design principle 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// Anthropic (Claude models).
    Anthropic,
    /// Any OpenAI-compatible API (Ollama, vLLM, `LiteLLM`, etc.).
    OpenaiCompat,
}

/// Shared token budget pool configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetPoolConfig {
    /// Maximum tokens allowed per day.
    pub daily_limit: u64,
    /// Maximum tokens allowed per month.
    pub monthly_limit: u64,
    /// What to do when the budget is exhausted.
    pub action_on_exhaust: ExhaustAction,
    /// Percentage thresholds at which to send alerts (e.g. `[0.8, 0.95]`).
    #[serde(default)]
    pub alert_thresholds: Vec<f64>,
}

/// Action to take when a budget pool is exhausted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExhaustAction {
    /// Fall back to a cheaper provider.
    Fallback,
    /// Pause processing until the budget resets.
    Pause,
    /// Notify the channel but continue processing.
    Notify,
}

/// Configuration for a single group (logical agent context).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupConfig {
    /// Primary provider name (must match a key in `providers`).
    pub provider: String,
    /// Model identifier to use with the provider.
    pub model: String,
    /// Ordered fallback chain when the primary provider fails.
    #[serde(default)]
    pub fallback: Vec<FallbackEntry>,
    /// Channel to bind this group to (must match a key in `channels`).
    pub channel: String,
    /// Whether this is the main (privileged) group.
    #[serde(default)]
    pub is_main: bool,
    /// Optional per-group token budget.
    pub token_budget: Option<TokenBudget>,
    /// Optional shared budget pool name (must match a key in `budget_pools`).
    pub budget_pool: Option<String>,
    /// Compose services this group is allowed to access.
    #[serde(default)]
    pub allowed_compose_services: Vec<String>,
}

/// A fallback provider+model pair in a group's fallback chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FallbackEntry {
    /// Provider name.
    pub provider: String,
    /// Model identifier.
    pub model: String,
}

/// Per-group token budget limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenBudget {
    /// Maximum tokens per day.
    pub daily: u64,
    /// Maximum tokens per month.
    pub monthly: u64,
}

/// Default settings for containers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerDefaults {
    /// Container image reference.
    pub image: String,
    /// Hard wall-clock timeout (e.g. `"30m"`).
    pub timeout: String,
    /// How long to keep an idle container before cleanup (e.g. `"5m"`).
    pub idle_ttl: String,
    /// Memory limit (e.g. `"4g"`).
    pub memory_limit: String,
    /// CPU core limit.
    pub cpu_limit: u32,
}

/// Tanren integration configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TanrenConfig {
    /// Tanren API base URL.
    pub api_url: String,
    /// How often to poll for dispatch status (e.g. `"10s"`).
    pub poll_interval: Option<String>,
    /// Maximum concurrent dispatches.
    pub max_concurrent_dispatches: Option<usize>,
}

/// Configuration for a single channel (e.g. a Discord bot, Telegram bot).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelConfig {
    /// Channel platform type — must be one of the known [`ChannelKind`] variants.
    #[serde(rename = "type")]
    pub kind: ChannelKind,
    /// Optional display name for this channel instance.
    pub name: Option<String>,
    /// Platform-specific settings (varies by channel type).
    #[serde(default)]
    pub settings: BTreeMap<String, toml::Value>,
}

/// Known channel platform types.
///
/// Using a typed enum instead of a raw `String` catches typos at
/// deserialization time (compile-time correctness, design principle 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    /// Discord bot.
    Discord,
    /// Telegram bot.
    Telegram,
    /// Slack bot.
    Slack,
    /// Generic webhook endpoint.
    Webhook,
}
