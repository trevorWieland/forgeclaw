use std::path::Path;

use crate::error::ConfigError;

use super::*;

fn example_toml_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/forgeclaw.example.toml")
}

// ---------------------------------------------------------------------------
// Basic loading
// ---------------------------------------------------------------------------

#[test]
fn load_example_toml() {
    let config = ForgeclawConfig::load(&example_toml_path())
        .expect("example TOML should parse successfully");
    assert_eq!(config.runtime.log_level, "info");
    assert!(config.providers.contains_key("anthropic"));
    assert!(config.groups.contains_key("main"));
    assert!(config.channels.contains_key("discord"));
}

#[test]
fn snapshot_example_config() {
    let config = ForgeclawConfig::load(&example_toml_path())
        .expect("example TOML should parse successfully");
    insta::assert_yaml_snapshot!(config);
}

#[test]
fn default_log_level_applied() {
    let toml_str = r#"
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
[channels]
"#;
    let config: ForgeclawConfig = toml::from_str(toml_str).expect("should parse");
    assert_eq!(config.runtime.log_level, "info");
}

#[test]
fn missing_required_field_produces_error() {
    let toml_str = r#"
[runtime]
data_dir = "/tmp"
"#;
    let result: Result<ForgeclawConfig, _> = toml::from_str(toml_str);
    assert!(result.is_err());
    let err = result.expect_err("should fail");
    let msg = err.to_string();
    assert!(
        msg.contains("missing"),
        "error should mention missing field: {msg}"
    );
}

#[test]
fn missing_channels_produces_error() {
    let toml_str = r#"
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
    let result: Result<ForgeclawConfig, _> = toml::from_str(toml_str);
    assert!(result.is_err());
    let msg = result.expect_err("should fail").to_string();
    assert!(
        msg.contains("channels"),
        "error should mention missing channels: {msg}"
    );
}

#[test]
fn invalid_store_backend_produces_error() {
    let toml_str = r#"
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
[channels]
"#;
    let result: Result<ForgeclawConfig, _> = toml::from_str(toml_str);
    assert!(result.is_err());
}

#[test]
fn is_main_defaults_to_false() {
    let toml_str = r#"
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

[channels.discord]
type = "discord"

[groups.test]
provider = "ollama"
model = "llama3"
channel = "discord"
"#;
    let config: ForgeclawConfig = toml::from_str(toml_str).expect("should parse");
    let group = config.groups.get("test").expect("should have test group");
    assert!(!group.is_main);
}

// ---------------------------------------------------------------------------
// Error display
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// deny_unknown_fields
// ---------------------------------------------------------------------------

#[test]
fn unknown_fields_rejected() {
    let toml_str = r#"
[runtime]
data_dir = "/tmp"
max_concurrent_containers = 1
warm_pool_size = 0
this_key_does_not_exist = true

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
[channels]
"#;
    let result: Result<ForgeclawConfig, _> = toml::from_str(toml_str);
    assert!(result.is_err());
    let msg = result.expect_err("should fail").to_string();
    assert!(
        msg.contains("unknown"),
        "error should mention unknown field: {msg}"
    );
}

#[test]
fn unknown_top_level_field_rejected() {
    let toml_str = r#"
bogus_section = "oops"

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
[channels]
"#;
    let result: Result<ForgeclawConfig, _> = toml::from_str(toml_str);
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// Channels
// ---------------------------------------------------------------------------

#[test]
fn channel_config_deserializes() {
    let toml_str = r#"
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

[channels.discord]
type = "discord"
name = "My Discord Bot"

[channels.discord.settings]
guild_id = "123456"

[channels.webhook]
type = "webhook"
"#;
    let config: ForgeclawConfig = toml::from_str(toml_str).expect("should parse");
    assert_eq!(config.channels.len(), 2);
    let discord = config.channels.get("discord").expect("should have discord");
    assert_eq!(discord.kind, ChannelKind::Discord);
    assert_eq!(discord.name.as_deref(), Some("My Discord Bot"));
    assert!(discord.settings.contains_key("guild_id"));
}

// ---------------------------------------------------------------------------
// Layered loading
// ---------------------------------------------------------------------------

#[test]
fn layered_load_from_single_project_file() {
    let config = ForgeclawConfig::load_layered(Some(&example_toml_path()))
        .expect("layered load should succeed");
    assert_eq!(config.runtime.log_level, "info");
    assert!(config.providers.contains_key("anthropic"));
}

#[test]
fn layered_precedence_env_over_project_over_user() {
    // Simulate the three-layer merge: user < project < env.
    // User sets log_level = "warn", warm_pool_size = 10, data_dir = "/user"
    let user_toml: toml::Value = toml::from_str(
        r#"
[runtime]
data_dir = "/user"
log_level = "warn"
max_concurrent_containers = 3
warm_pool_size = 10

[store]
backend = "sqlite"
url = "sqlite:///user/forgeclaw.db"

[container]
image = "user-image:latest"
timeout = "30m"
idle_ttl = "5m"
memory_limit = "4g"
cpu_limit = 2

[providers.anthropic]
type = "anthropic"

[channels.discord]
type = "discord"

[groups.main]
provider = "anthropic"
model = "claude-sonnet-4-20250514"
channel = "discord"
"#,
    )
    .expect("parse user config");

    // Project overrides log_level and data_dir, but warm_pool_size comes from user.
    let project_toml: toml::Value = toml::from_str(
        r#"
[runtime]
data_dir = "/project"
log_level = "debug"
"#,
    )
    .expect("parse project config");

    // Env overrides data_dir only.
    let env_vars = vec![(
        "FORGECLAW_RUNTIME__DATA_DIR".to_owned(),
        "\"/env\"".to_owned(),
    )];

    let merged = merge::deep_merge(user_toml, project_toml);
    let merged = apply_env_overrides(merged, env_vars.into_iter());
    let config: ForgeclawConfig = merged.try_into().expect("deserialize merged config");
    config.validate().expect("validation should pass");

    // env > project > user
    assert_eq!(
        config.runtime.data_dir, "/env",
        "env should win for data_dir"
    );
    assert_eq!(
        config.runtime.log_level, "debug",
        "project should win over user for log_level"
    );
    assert_eq!(
        config.runtime.warm_pool_size, 10,
        "user value should persist when not overridden"
    );
}
