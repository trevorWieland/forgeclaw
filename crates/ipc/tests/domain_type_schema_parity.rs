//! Runtime-vs-schema parity checks for the Phase A domain types with
//! layered format validators.
//!
//! `ProtocolVersionText` and `BranchName` both extend the plain
//! maxLength-128 envelope with format constraints. The exported JSON
//! Schema for those types currently only expresses the length bound
//! (all bounded-text types emit `{"type":"string","maxLength":128}`
//! inline), so the schema is intentionally *more permissive* than the
//! runtime.
//!
//! This test makes that relationship explicit: any value that passes
//! the runtime validator MUST also pass the emitted JSON Schema.
//! Format-invalid values must be rejected at runtime even when the
//! schema accepts them. That way the Rust runtime is always at least
//! as strict as the schema, and downstream tooling relying on the
//! schema for sanity-check can never drift ahead of the runtime.

use forgeclaw_ipc::{BranchName, ProtocolVersionText};
use jsonschema::Validator;
use serde_json::json;

fn json_schema_for<T: schemars::JsonSchema>() -> serde_json::Value {
    let schema = schemars::schema_for!(T);
    serde_json::to_value(&schema).expect("serialize schema to value")
}

fn compile(schema: &serde_json::Value) -> Validator {
    jsonschema::validator_for(schema).expect("compile schema")
}

#[test]
fn protocol_version_text_runtime_matches_or_exceeds_schema() {
    let schema = json_schema_for::<ProtocolVersionText>();
    let compiled = compile(&schema);

    // Format-valid samples — both runtime and schema must accept.
    for good in ["1.0", "42.17", "0.0", "9.99"] {
        assert!(
            ProtocolVersionText::new(good).is_ok(),
            "runtime must accept valid `{good}`"
        );
        assert!(
            compiled.is_valid(&json!(good)),
            "schema must accept valid `{good}`"
        );
    }

    // Format-invalid samples — runtime must reject (the schema only
    // bounds length, so it accepts these; that's the intended
    // "runtime is stricter than schema" relationship).
    for bad in ["", "1", "1.", "1.foo", "v1.0", "1.0.3"] {
        assert!(
            ProtocolVersionText::new(bad).is_err(),
            "runtime must reject invalid `{bad}`"
        );
    }

    // Length-invalid: runtime and schema must both reject.
    let overlong = "1.".to_owned() + &"9".repeat(200);
    assert!(
        ProtocolVersionText::new(&overlong).is_err(),
        "runtime must reject overlong protocol version"
    );
    assert!(
        !compiled.is_valid(&json!(overlong)),
        "schema must reject overlong protocol version (maxLength)"
    );
}

#[test]
fn branch_name_runtime_matches_or_exceeds_schema() {
    let schema = json_schema_for::<BranchName>();
    let compiled = compile(&schema);

    for good in ["main", "feat/x", "release/1.x", "lane-0.4", "a/b/c"] {
        assert!(
            BranchName::new(good).is_ok(),
            "runtime must accept valid `{good}`"
        );
        assert!(
            compiled.is_valid(&json!(good)),
            "schema must accept valid `{good}`"
        );
    }

    // Runtime-only rejections (length-bounded schema still accepts).
    for bad in [
        "",
        "/main",
        "main/",
        "a//b",
        "a..b",
        "with space",
        "with\ttab",
        "control\x01char",
    ] {
        assert!(
            BranchName::new(bad).is_err(),
            "runtime must reject advisory-invalid `{bad}`"
        );
    }

    // Length cap applies to both.
    let overlong = "x".repeat(129);
    assert!(BranchName::new(&overlong).is_err());
    assert!(!compiled.is_valid(&json!(overlong)));
}
