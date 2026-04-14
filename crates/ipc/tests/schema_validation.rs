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
