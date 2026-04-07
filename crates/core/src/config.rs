//! Configuration model deserialized from TOML.
//!
//! The top-level [`ForgeclawConfig`] struct maps to a `forgeclaw.toml` file.
//! See `examples/forgeclaw.example.toml` for a complete sample.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::ConfigError;

/// Top-level Forgeclaw configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForgeclawConfig {
    /// Runtime settings (data directory, concurrency limits, etc.).
    pub runtime: RuntimeConfig,
    /// Database / persistence backend.
    pub store: StoreConfig,
    /// Named provider configurations (keyed by provider name).
    pub providers: BTreeMap<String, ProviderConfig>,
    /// Shared budget pools (keyed by pool name).
    #[serde(default)]
    pub budget_pools: BTreeMap<String, BudgetPoolConfig>,
    /// Named group configurations (keyed by group name).
    pub groups: BTreeMap<String, GroupConfig>,
    /// Default container settings.
    pub container: ContainerDefaults,
    /// Optional Tanren integration settings.
    pub tanren: Option<TanrenConfig>,
}

impl ForgeclawConfig {
    /// Load and parse a configuration file from the given path.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Io`] if the file cannot be read, or
    /// [`ConfigError::Parse`] if the TOML is invalid or doesn't match
    /// the expected schema.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let contents = std::fs::read_to_string(path)?;
        let config: Self = toml::from_str(&contents)?;
        Ok(config)
    }
}

/// Runtime configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

fn default_log_level() -> String {
    "info".to_owned()
}

/// Database / persistence backend configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
pub struct ProviderConfig {
    /// Provider type identifier (e.g. `"anthropic"`, `"openai_compat"`).
    #[serde(rename = "type")]
    pub kind: String,
    /// Optional custom base URL (for self-hosted providers like Ollama).
    pub base_url: Option<String>,
}

/// Shared token budget pool configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
pub struct GroupConfig {
    /// Primary provider name (must match a key in `providers`).
    pub provider: String,
    /// Model identifier to use with the provider.
    pub model: String,
    /// Ordered fallback chain when the primary provider fails.
    #[serde(default)]
    pub fallback: Vec<FallbackEntry>,
    /// Channel to bind this group to.
    pub channel: String,
    /// Whether this is the main (privileged) group.
    #[serde(default)]
    pub is_main: bool,
    /// Optional per-group token budget.
    pub token_budget: Option<TokenBudget>,
    /// Optional shared budget pool name.
    pub budget_pool: Option<String>,
    /// Compose services this group is allowed to access.
    #[serde(default)]
    pub allowed_compose_services: Vec<String>,
}

/// A fallback provider+model pair in a group's fallback chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FallbackEntry {
    /// Provider name.
    pub provider: String,
    /// Model identifier.
    pub model: String,
}

/// Per-group token budget limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenBudget {
    /// Maximum tokens per day.
    pub daily: u64,
    /// Maximum tokens per month.
    pub monthly: u64,
}

/// Default settings for containers.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
pub struct TanrenConfig {
    /// Tanren API base URL.
    pub api_url: String,
    /// How often to poll for dispatch status (e.g. `"10s"`).
    pub poll_interval: Option<String>,
    /// Maximum concurrent dispatches.
    pub max_concurrent_dispatches: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example_toml_path() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/forgeclaw.example.toml")
    }

    #[test]
    fn load_example_toml() {
        let config = ForgeclawConfig::load(&example_toml_path())
            .expect("example TOML should parse successfully");
        assert_eq!(config.runtime.log_level, "info");
        assert!(config.providers.contains_key("anthropic"));
        assert!(config.groups.contains_key("main"));
    }

    #[test]
    fn snapshot_example_config() {
        let config = ForgeclawConfig::load(&example_toml_path())
            .expect("example TOML should parse successfully");
        insta::assert_yaml_snapshot!(config);
    }

    #[test]
    fn default_log_level_applied() {
        let toml = r#"
[runtime]
data_dir = "/tmp"
max_concurrent_containers = 1
warm_pool_size = 0

[store]
backend = "sqlite"
url = "sqlite:///tmp/test.db"

[container]
image = "test:latest"
timeout = "10m"
idle_ttl = "1m"
memory_limit = "1g"
cpu_limit = 1

[providers]

[groups]
"#;
        let config: ForgeclawConfig = toml::from_str(toml).expect("should parse");
        assert_eq!(config.runtime.log_level, "info");
    }

    #[test]
    fn missing_required_field_produces_error() {
        let toml = r#"
[runtime]
data_dir = "/tmp"
"#;
        let result: Result<ForgeclawConfig, _> = toml::from_str(toml);
        assert!(result.is_err());
        let err = result.expect_err("should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("missing"),
            "error should mention missing field: {msg}"
        );
    }

    #[test]
    fn invalid_store_backend_produces_error() {
        let toml = r#"
[runtime]
data_dir = "/tmp"
max_concurrent_containers = 1
warm_pool_size = 0

[store]
backend = "mysql"
url = "mysql://localhost"

[container]
image = "test:latest"
timeout = "10m"
idle_ttl = "1m"
memory_limit = "1g"
cpu_limit = 1

[providers]

[groups]
"#;
        let result: Result<ForgeclawConfig, _> = toml::from_str(toml);
        assert!(result.is_err());
    }

    #[test]
    fn is_main_defaults_to_false() {
        let toml = r#"
[runtime]
data_dir = "/tmp"
max_concurrent_containers = 1
warm_pool_size = 0

[store]
backend = "sqlite"
url = "sqlite:///tmp/test.db"

[container]
image = "test:latest"
timeout = "10m"
idle_ttl = "1m"
memory_limit = "1g"
cpu_limit = 1

[providers.ollama]
type = "openai_compat"
base_url = "http://localhost:11434/v1"

[groups.test]
provider = "ollama"
model = "llama3"
channel = "discord"
"#;
        let config: ForgeclawConfig = toml::from_str(toml).expect("should parse");
        let group = config.groups.get("test").expect("should have test group");
        assert!(!group.is_main);
    }

    #[test]
    fn config_error_io_display() {
        let err = ConfigError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "file not found",
        ));
        insta::assert_snapshot!(err.to_string(), @"failed to read config file: file not found");
    }

    #[test]
    fn load_nonexistent_file_returns_io_error() {
        let result = ForgeclawConfig::load(Path::new("/nonexistent/forgeclaw.toml"));
        assert!(result.is_err());
        assert!(matches!(
            result.expect_err("should fail"),
            ConfigError::Io(_)
        ));
    }
}
