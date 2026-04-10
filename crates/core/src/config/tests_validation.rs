//! Cross-reference validation tests.

use crate::error::ConfigError;

use super::*;

fn example_toml_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/forgeclaw.example.toml")
}

#[test]
fn validate_catches_broken_provider_ref() {
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

[channels.discord]
type = "discord"

[groups.main]
provider = "nonexistent"
model = "test"
channel = "discord"
"#;
    let config: ForgeclawConfig = toml::from_str(toml_str).expect("should parse");
    let err = config.validate().expect_err("should fail validation");
    let msg = err.to_string();
    assert!(
        msg.contains("nonexistent"),
        "should mention broken provider ref: {msg}"
    );
}

#[test]
fn validate_catches_broken_channel_ref() {
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

[providers.test]
type = "anthropic"

[channels.slack]
type = "slack"

[groups.main]
provider = "test"
model = "test"
channel = "nonexistent"
"#;
    let config: ForgeclawConfig = toml::from_str(toml_str).expect("should parse");
    let err = config.validate().expect_err("should fail validation");
    let msg = err.to_string();
    assert!(
        msg.contains("nonexistent"),
        "should mention broken channel ref: {msg}"
    );
}

#[test]
fn validate_catches_channel_ref_with_empty_channels() {
    // Even when channels section is empty, refs are validated.
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

[providers.test]
type = "anthropic"

[channels]

[groups.main]
provider = "test"
model = "test"
channel = "discord"
"#;
    let config: ForgeclawConfig = toml::from_str(toml_str).expect("should parse");
    let err = config.validate().expect_err("should fail validation");
    let msg = err.to_string();
    assert!(
        msg.contains("discord"),
        "should mention broken channel ref: {msg}"
    );
}

#[test]
fn validate_catches_empty_channels_with_groups() {
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

[providers.test]
type = "anthropic"

[channels]

[groups.main]
provider = "test"
model = "test"
channel = "discord"
"#;
    let config: ForgeclawConfig = toml::from_str(toml_str).expect("should parse");
    let err = config.validate().expect_err("should fail validation");
    let msg = err.to_string();
    assert!(
        msg.contains("at least one channel"),
        "should mention empty channels: {msg}"
    );
}

#[test]
fn validate_catches_broken_budget_pool_ref() {
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

[providers.test]
type = "anthropic"

[channels.discord]
type = "discord"

[groups.main]
provider = "test"
model = "test"
channel = "discord"
budget_pool = "nonexistent"
"#;
    let config: ForgeclawConfig = toml::from_str(toml_str).expect("should parse");
    let err = config.validate().expect_err("should fail validation");
    let msg = err.to_string();
    assert!(
        msg.contains("nonexistent"),
        "should mention broken budget_pool ref: {msg}"
    );
}

#[test]
fn validate_catches_broken_fallback_provider_ref() {
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

[providers.test]
type = "anthropic"

[channels.discord]
type = "discord"

[groups.main]
provider = "test"
model = "test"
channel = "discord"
fallback = [{ provider = "ghost", model = "m" }]
"#;
    let config: ForgeclawConfig = toml::from_str(toml_str).expect("should parse");
    let err = config.validate().expect_err("should fail validation");
    let msg = err.to_string();
    assert!(
        msg.contains("ghost"),
        "should mention broken fallback provider ref: {msg}"
    );
}

#[test]
fn invalid_provider_kind_rejected() {
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

[providers.bad]
type = "anthorpic"

[channels.discord]
type = "discord"

[groups]
"#;
    let result: Result<ForgeclawConfig, _> = toml::from_str(toml_str);
    assert!(result.is_err(), "typo 'anthorpic' should be rejected");
    let msg = result.expect_err("should fail").to_string();
    assert!(
        msg.contains("unknown variant"),
        "error should mention unknown variant: {msg}"
    );
}

#[test]
fn invalid_channel_kind_rejected() {
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

[channels.bad]
type = "discrod"

[groups]
"#;
    let result: Result<ForgeclawConfig, _> = toml::from_str(toml_str);
    assert!(result.is_err(), "typo 'discrod' should be rejected");
    let msg = result.expect_err("should fail").to_string();
    assert!(
        msg.contains("unknown variant"),
        "error should mention unknown variant: {msg}"
    );
}

#[test]
fn validate_passes_valid_config() {
    let config = ForgeclawConfig::load(&example_toml_path()).expect("example TOML should parse");
    config.validate().expect("example config should be valid");
}

#[test]
fn load_runs_validation_automatically() {
    let dir = std::env::temp_dir().join("forgeclaw-test-load-validates");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("bad.toml");
    std::fs::write(
        &path,
        r#"
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

[channels.discord]
type = "discord"

[groups.main]
provider = "nonexistent"
model = "test"
channel = "discord"
"#,
    )
    .expect("write temp file");

    let err = ForgeclawConfig::load(&path).expect_err("load should fail validation");
    assert!(
        matches!(err, ConfigError::Validation { .. }),
        "expected Validation error, got: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
