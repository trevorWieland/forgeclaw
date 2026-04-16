//! Strict runtime-vs-schema parity for the format-constrained domain
//! types (`ProtocolVersionText`, `BranchName`).
//!
//! These are the only two bounded-text wire types that layer a format
//! validator on top of the maxLength bound, and the published JSON
//! Schema now carries that same constraint (regex `pattern` for
//! `ProtocolVersionText`; `allOf` of positive `pattern` plus
//! `not`-pattern clauses for `BranchName`).
//!
//! The contract is **strict parity**: for every candidate value, the
//! runtime constructor and the published JSON Schema agree on
//! accept/reject. Polyglot adapters generated from the schema can
//! never construct values the runtime then rejects, and the runtime
//! never accepts values the schema would reject.
//!
//! These tests are the canonical regression bar for that contract.

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
fn protocol_version_text_schema_matches_runtime_strictly() {
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

    // Format-invalid samples — runtime AND schema must both reject.
    // (Previously the schema accepted these because it only carried
    // maxLength; the new schema carries the same regex pattern as the
    // runtime validator.)
    for bad in ["", "1", "1.", "1.foo", "v1.0", "1.0.3", ".1"] {
        assert!(
            ProtocolVersionText::new(bad).is_err(),
            "runtime must reject invalid `{bad}`"
        );
        assert!(
            !compiled.is_valid(&json!(bad)),
            "schema must reject invalid `{bad}`"
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
fn branch_name_schema_matches_runtime_strictly() {
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

    // Format-invalid samples — runtime AND schema must both reject.
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
            "runtime must reject advisory-invalid `{bad:?}`"
        );
        assert!(
            !compiled.is_valid(&json!(bad)),
            "schema must reject advisory-invalid `{bad:?}`"
        );
    }

    // Length cap applies to both.
    let overlong = "x".repeat(129);
    assert!(BranchName::new(&overlong).is_err());
    assert!(!compiled.is_valid(&json!(overlong)));
}
