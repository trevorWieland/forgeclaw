//! Environment variable override typing and secret-filtering tests.

use super::*;

#[test]
fn parse_env_value_integer() {
    let v = parse_env_value("42");
    assert_eq!(v.as_integer(), Some(42));
}

#[test]
fn parse_env_value_boolean() {
    let v = parse_env_value("true");
    assert_eq!(v.as_bool(), Some(true));
}

#[test]
fn parse_env_value_float() {
    let v = parse_env_value("3.14");
    assert!(v.as_float().is_some());
}

#[test]
fn parse_env_value_plain_string_fallback() {
    let v = parse_env_value("hello world");
    assert_eq!(v.as_str(), Some("hello world"));
}

#[test]
fn parse_env_value_quoted_string() {
    let v = parse_env_value("\"quoted\"");
    assert_eq!(v.as_str(), Some("quoted"));
}

#[test]
fn env_override_integer_field_deserializes() {
    let base_str = r#"
[runtime]
data_dir = "/tmp"
log_level = "info"
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
    let mut base: toml::Value = toml::from_str(base_str).expect("parse base");
    let typed = parse_env_value("10");
    set_nested(&mut base, &["runtime", "max_concurrent_containers"], typed);
    let config: ForgeclawConfig = base.try_into().expect("should deserialize");
    assert_eq!(config.runtime.max_concurrent_containers, 10);
}

#[test]
fn secret_env_vars_are_ignored() {
    // Flat FORGECLAW_* vars (no __ separator) are secrets and must not be
    // injected into config.
    let base = toml::Value::Table(toml::map::Map::new());
    let env_vars = vec![(
        "FORGECLAW_ANTHROPIC_API_KEY".to_owned(),
        "sk-secret-123".to_owned(),
    )];
    let result = apply_env_overrides(base, env_vars.into_iter());

    let table = result.as_table().expect("should be table");
    assert!(
        !table.contains_key("anthropic_api_key"),
        "secret env var should not be injected into config"
    );
}

#[test]
fn path_env_vars_are_applied() {
    // Vars with __ separator ARE config overrides and should be applied.
    let base_str = r#"
[runtime]
data_dir = "/original"
log_level = "info"
max_concurrent_containers = 1
warm_pool_size = 0
"#;
    let base: toml::Value = toml::from_str(base_str).expect("parse base");
    let env_vars = vec![(
        "FORGECLAW_RUNTIME__DATA_DIR".to_owned(),
        "\"/overridden\"".to_owned(),
    )];
    let result = apply_env_overrides(base, env_vars.into_iter());

    let data_dir = result
        .get("runtime")
        .and_then(|r| r.get("data_dir"))
        .and_then(toml::Value::as_str);
    assert_eq!(
        data_dir,
        Some("/overridden"),
        "__ env var should override config value"
    );
}

#[test]
fn mixed_secret_and_config_vars() {
    // Verify that in a mix of secret and config vars, only config vars
    // (with __) are applied.
    let base_str = "[runtime]\ndata_dir = \"/tmp\"\nlog_level = \"info\"\nmax_concurrent_containers = 1\nwarm_pool_size = 0\n";
    let base: toml::Value = toml::from_str(base_str).expect("parse base");
    let env_vars = vec![
        (
            "FORGECLAW_DISCORD_TOKEN".to_owned(),
            "secret-token".to_owned(),
        ),
        (
            "FORGECLAW_RUNTIME__LOG_LEVEL".to_owned(),
            "\"debug\"".to_owned(),
        ),
        (
            "FORGECLAW_OPENAI_API_KEY".to_owned(),
            "sk-other-secret".to_owned(),
        ),
    ];
    let result = apply_env_overrides(base, env_vars.into_iter());

    // Config override should be applied.
    let log_level = result
        .get("runtime")
        .and_then(|r| r.get("log_level"))
        .and_then(toml::Value::as_str);
    assert_eq!(log_level, Some("debug"));

    // Secrets should NOT appear.
    let table = result.as_table().expect("should be table");
    assert!(!table.contains_key("discord_token"));
    assert!(!table.contains_key("openai_api_key"));
}

#[test]
fn env_override_preserves_map_key_casing() {
    // Map-key segments (provider/group/channel names) preserve their
    // original casing while section and field names are lowercased.
    let base_str = r#"
[providers.MyProvider]
type = "anthropic"
"#;
    let base: toml::Value = toml::from_str(base_str).expect("parse base");
    let env_vars = vec![(
        "FORGECLAW_PROVIDERS__MyProvider__BASE_URL".to_owned(),
        "\"http://custom:8080\"".to_owned(),
    )];
    let result = apply_env_overrides(base, env_vars.into_iter());

    // The map key "MyProvider" should be preserved, not lowercased.
    let base_url = result
        .get("providers")
        .and_then(|p| p.get("MyProvider"))
        .and_then(|p| p.get("base_url"))
        .and_then(toml::Value::as_str);
    assert_eq!(
        base_url,
        Some("http://custom:8080"),
        "map key casing should be preserved"
    );

    // Make sure it didn't also create a lowercased key.
    let lowered = result.get("providers").and_then(|p| p.get("myprovider"));
    assert!(lowered.is_none(), "lowercased map key should not exist");
}
