//! Contract checks that keep runtime limits, schemas, and docs aligned.

use std::path::PathBuf;

use forgeclaw_core::{GroupId, TaskId};
use forgeclaw_ipc::{
    CommandPayload, DispatchSelfImprovementPayload, HistoricalMessage, MessagesPayload,
};

fn docs_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .parent()
        .expect("workspace root")
        .join("docs/IPC_PROTOCOL.md")
}

fn schema_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("schemas")
        .join(name)
}

#[test]
fn runtime_rejects_messages_over_256() {
    let msg = HistoricalMessage {
        sender: "A".parse().expect("valid sender"),
        text: "x".parse().expect("valid text"),
        timestamp: "2026-04-03T10:00:00Z".parse().expect("valid timestamp"),
    };
    let payload = serde_json::json!({
        "job_id": "job-1",
        "messages": std::iter::repeat_n(serde_json::to_value(&msg).expect("serialize"), 257)
            .collect::<Vec<_>>()
    });
    let err =
        serde_json::from_value::<MessagesPayload>(payload).expect_err("messages >256 should fail");
    assert!(err.to_string().contains("256"));
}

#[test]
fn runtime_rejects_self_improvement_lists_over_64() {
    let payload = serde_json::json!({
        "command": "dispatch_self_improvement",
        "payload": {
            "objective": "o",
            "scopes": std::iter::repeat_n("scope", 65).collect::<Vec<_>>(),
            "acceptance_tests": ["cargo test"],
            "branch_policy": "create"
        }
    });
    let err =
        serde_json::from_value::<CommandPayload>(payload).expect_err("scopes >64 should fail");
    assert!(err.to_string().contains("64"));
}

#[test]
fn committed_schemas_encode_list_limits() {
    fn collect_max_items(value: &serde_json::Value, out: &mut Vec<u64>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, nested) in map {
                    if key == "maxItems" {
                        if let Some(v) = nested.as_u64() {
                            out.push(v);
                        }
                    }
                    collect_max_items(nested, out);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    collect_max_items(item, out);
                }
            }
            _ => {}
        }
    }

    let h2c_schema: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(schema_path("host_to_container.schema.json"))
            .expect("read host_to_container schema"),
    )
    .expect("parse host_to_container schema");
    let c2h_schema: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(schema_path("container_to_host.schema.json"))
            .expect("read container_to_host schema"),
    )
    .expect("parse container_to_host schema");

    let mut h2c_max_items = Vec::new();
    collect_max_items(&h2c_schema, &mut h2c_max_items);
    assert!(h2c_max_items.contains(&256));

    let mut c2h_max_items = Vec::new();
    collect_max_items(&c2h_schema, &mut c2h_max_items);
    assert!(c2h_max_items.contains(&64));
}

#[test]
fn protocol_docs_publish_wire_constraints() {
    let docs = std::fs::read_to_string(docs_path()).expect("read IPC protocol docs");
    assert!(docs.contains("## Wire Constraints"));
    assert!(docs.contains("init.context.messages"));
    assert!(docs.contains("max 256 entries"));
    assert!(docs.contains("max 64 entries"));
    assert!(docs.contains("Core ID fields"));
    assert!(docs.contains("maxLength 128"));
}

#[test]
fn bounded_text_limits_are_explicit_on_public_types() {
    assert_eq!(forgeclaw_ipc::message::IdentifierText::MAX_LEN, 128);
    assert_eq!(forgeclaw_ipc::message::ModelText::MAX_LEN, 256);
    assert_eq!(forgeclaw_ipc::message::TokenText::MAX_LEN, 2048);
    assert_eq!(forgeclaw_ipc::message::ShortText::MAX_LEN, 1024);
    assert_eq!(forgeclaw_ipc::message::ScheduleValueText::MAX_LEN, 512);
    assert_eq!(forgeclaw_ipc::message::MessageText::MAX_LEN, 32 * 1024);
    assert_eq!(forgeclaw_ipc::message::PromptText::MAX_LEN, 32 * 1024);
    assert_eq!(forgeclaw_ipc::message::OutputDeltaText::MAX_LEN, 64 * 1024);
    assert_eq!(
        forgeclaw_ipc::message::OutputResultText::MAX_LEN,
        256 * 1024
    );
    assert_eq!(forgeclaw_ipc::message::SessionIdText::MAX_LEN, 128);
    assert_eq!(forgeclaw_ipc::message::ListItemText::MAX_LEN, 1024);
    assert_eq!(forgeclaw_ipc::message::AbsoluteHttpUrl::MAX_LEN, 2048);
}

#[test]
fn dispatch_self_improvement_payload_still_roundtrips_with_valid_bounds() {
    let payload = DispatchSelfImprovementPayload {
        objective: "objective".parse().expect("valid objective"),
        scopes: vec!["scope".parse().expect("valid scope")]
            .try_into()
            .expect("scopes within bound"),
        acceptance_tests: vec!["cargo test".parse().expect("valid acceptance test")]
            .try_into()
            .expect("acceptance tests within bound"),
        branch_policy: forgeclaw_ipc::BranchPolicy::Create,
    };
    let json = serde_json::to_value(&payload).expect("serialize");
    let back: DispatchSelfImprovementPayload = serde_json::from_value(json).expect("deserialize");
    assert_eq!(back, payload);
}

#[test]
fn validated_core_ids_enforce_128_char_limit() {
    assert!(GroupId::new("x".repeat(128)).is_ok());
    assert!(TaskId::new("x".repeat(128)).is_ok());
    assert!(GroupId::new("x".repeat(129)).is_err());
    assert!(TaskId::new("x".repeat(129)).is_err());
}

#[test]
fn committed_schema_encodes_128_char_bounds_for_command_ids() {
    let c2h_schema: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(schema_path("container_to_host.schema.json"))
            .expect("read container_to_host schema"),
    )
    .expect("parse container_to_host schema");

    let target_group = &c2h_schema["$defs"]["SendMessagePayload"]["properties"]["target_group"];
    assert_eq!(target_group["maxLength"], 128);
    let group = &c2h_schema["$defs"]["ScheduleTaskPayload"]["properties"]["group"];
    assert_eq!(group["maxLength"], 128);
    let cancel_task_id = &c2h_schema["$defs"]["CancelTaskPayload"]["properties"]["task_id"];
    assert_eq!(cancel_task_id["maxLength"], 128);
    let pause_task_id = &c2h_schema["$defs"]["PauseTaskPayload"]["properties"]["task_id"];
    assert_eq!(pause_task_id["maxLength"], 128);
}
