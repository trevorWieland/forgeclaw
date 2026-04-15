//! Validate all fixture JSON files against the committed schemas.
//!
//! This test ensures that the fixtures in `fixtures/` conform to
//! the JSON Schemas committed in `schemas/`. Catches drift
//! between fixture examples and the stable protocol contract.

use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn schemas_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("schemas")
}

fn load_schema(filename: &str) -> serde_json::Value {
    let path = schemas_dir().join(filename);
    let text = std::fs::read_to_string(&path)
        .expect("could not read committed schema — run `just schemas` to generate it");
    serde_json::from_str(&text).expect("parse schema JSON")
}

fn validate_fixture(schema_value: &serde_json::Value, fixture_path: &std::path::Path) {
    let fixture_text = std::fs::read_to_string(fixture_path).expect("read fixture");
    let fixture_value: serde_json::Value =
        serde_json::from_str(&fixture_text).expect("parse fixture JSON");
    let validator = jsonschema::validator_for(schema_value).expect("compile schema");
    let result = validator.validate(&fixture_value);
    assert!(
        result.is_ok(),
        "fixture {} failed schema validation: {}",
        fixture_path.display(),
        result.err().map_or_else(String::new, |e| e.to_string()),
    );
}

#[test]
fn container_to_host_fixtures_conform_to_schema() {
    let schema_value = load_schema("container_to_host.schema.json");

    let c2h_fixtures = [
        "ready.json",
        "output_delta.json",
        "output_complete.json",
        "output_complete_null.json",
        "progress.json",
        "command.json",
        "command_register_group.json",
        "command_schedule_task.json",
        "command_pause_task.json",
        "command_cancel_task.json",
        "command_dispatch_tanren.json",
        "command_dispatch_self_improvement.json",
        "error.json",
        "heartbeat.json",
    ];

    let dir = fixtures_dir();
    for name in &c2h_fixtures {
        let path = dir.join(name);
        assert!(path.exists(), "fixture missing: {name}");
        validate_fixture(&schema_value, &path);
    }
}

#[test]
fn container_to_host_schema_rejects_output_complete_missing_result() {
    let schema_value = load_schema("container_to_host.schema.json");
    let validator = jsonschema::validator_for(&schema_value).expect("compile schema");
    let missing_result = serde_json::json!({
        "type": "output_complete",
        "job_id": "job-1",
        "stop_reason": "end_turn"
    });
    let err = validator
        .validate(&missing_result)
        .expect_err("missing required result should fail schema");
    assert!(
        !err.to_string().is_empty(),
        "schema validation should report a concrete error"
    );
}

#[test]
fn container_to_host_schema_rejects_register_group_extensions_missing_version() {
    let schema_value = load_schema("container_to_host.schema.json");
    let validator = jsonschema::validator_for(&schema_value).expect("compile schema");
    let missing_version = serde_json::json!({
        "type": "command",
        "command": "register_group",
        "payload": {
            "name": "new-group",
            "extensions": {
                "trigger": "@bot"
            }
        }
    });
    let err = validator
        .validate(&missing_version)
        .expect_err("extensions without version should fail schema");
    assert!(
        !err.to_string().is_empty(),
        "schema validation should report a concrete error"
    );
}

#[test]
fn container_to_host_schema_rejects_register_group_extensions_version_over_128() {
    let schema_value = load_schema("container_to_host.schema.json");
    let validator = jsonschema::validator_for(&schema_value).expect("compile schema");
    let too_long_version = serde_json::json!({
        "type": "command",
        "command": "register_group",
        "payload": {
            "name": "new-group",
            "extensions": {
                "version": "x".repeat(129)
            }
        }
    });
    let err = validator
        .validate(&too_long_version)
        .expect_err("extensions.version >128 should fail schema");
    assert!(
        !err.to_string().is_empty(),
        "schema validation should report a concrete error"
    );
}

#[test]
fn container_to_host_schema_rejects_register_group_extensions_over_32_properties() {
    let schema_value = load_schema("container_to_host.schema.json");
    let validator = jsonschema::validator_for(&schema_value).expect("compile schema");
    let mut extensions = serde_json::Map::new();
    extensions.insert(
        "version".to_owned(),
        serde_json::Value::String("1".to_owned()),
    );
    for i in 0..32 {
        extensions.insert(format!("k{i}"), serde_json::Value::from(i));
    }
    let payload = serde_json::json!({
        "type": "command",
        "command": "register_group",
        "payload": {
            "name": "new-group",
            "extensions": extensions
        }
    });
    let err = validator
        .validate(&payload)
        .expect_err("extensions maxProperties should fail schema");
    assert!(
        !err.to_string().is_empty(),
        "schema validation should report a concrete error"
    );
}

#[test]
fn container_to_host_schema_rejects_invalid_heartbeat_timestamp() {
    let schema_value = load_schema("container_to_host.schema.json");
    let validator = jsonschema::validator_for(&schema_value).expect("compile schema");
    let invalid_timestamp = serde_json::json!({
        "type": "heartbeat",
        "timestamp": "2026-04-03 10:30:00"
    });
    let err = validator
        .validate(&invalid_timestamp)
        .expect_err("invalid heartbeat timestamp should fail schema");
    assert!(
        !err.to_string().is_empty(),
        "schema validation should report a concrete error"
    );
}

#[test]
fn host_to_container_fixtures_conform_to_schema() {
    let schema_value = load_schema("host_to_container.schema.json");

    let h2c_fixtures = ["init.json", "messages.json", "shutdown.json"];

    let dir = fixtures_dir();
    for name in &h2c_fixtures {
        let path = dir.join(name);
        assert!(path.exists(), "fixture missing: {name}");
        validate_fixture(&schema_value, &path);
    }
}

#[test]
fn host_to_container_schema_rejects_invalid_timezone_with_valid_timestamp() {
    let schema_value = load_schema("host_to_container.schema.json");
    let validator = jsonschema::validator_for(&schema_value).expect("compile schema");
    let invalid_timezone = serde_json::json!({
        "type": "init",
        "job_id": "job-1",
        "context": {
            "messages": [
                { "sender": "A", "text": "x", "timestamp": "2026-04-03T10:00:00Z" }
            ],
            "group": { "id": "group-main", "name": "Main", "is_main": true },
            "timezone": "Mars/Olympus"
        },
        "config": {
            "provider_proxy_url": "http://proxy.local",
            "provider_proxy_token": "token",
            "model": "model",
            "max_tokens": 1000,
            "tools_enabled": true,
            "timeout_seconds": 600
        }
    });
    let err = validator
        .validate(&invalid_timezone)
        .expect_err("invalid timezone should fail schema");
    assert!(
        !err.to_string().is_empty(),
        "schema validation should report a concrete error"
    );
}

#[test]
fn host_to_container_schema_rejects_invalid_timestamp_with_valid_timezone() {
    let schema_value = load_schema("host_to_container.schema.json");
    let validator = jsonschema::validator_for(&schema_value).expect("compile schema");
    let invalid_timestamp = serde_json::json!({
        "type": "init",
        "job_id": "job-1",
        "context": {
            "messages": [
                { "sender": "A", "text": "x", "timestamp": "not-a-timestamp" }
            ],
            "group": { "id": "group-main", "name": "Main", "is_main": true },
            "timezone": "UTC"
        },
        "config": {
            "provider_proxy_url": "http://proxy.local",
            "provider_proxy_token": "token",
            "model": "model",
            "max_tokens": 1000,
            "tools_enabled": true,
            "timeout_seconds": 600
        }
    });
    let err = validator
        .validate(&invalid_timestamp)
        .expect_err("invalid timestamp should fail schema");
    assert!(
        !err.to_string().is_empty(),
        "schema validation should report a concrete error"
    );
}

#[test]
fn container_to_host_schema_publishes_extensions_encoded_size_metadata() {
    let schema_value = load_schema("container_to_host.schema.json");
    let extensions = &schema_value["$defs"]["RegisterGroupPayload"]["properties"]["extensions"];
    assert_eq!(extensions["x-maxEncodedBytes"], 65536);
}
