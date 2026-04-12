//! Spec-fidelity gate.
//!
//! Every canonical JSON example from
//! [`docs/IPC_PROTOCOL.md`](../../../docs/IPC_PROTOCOL.md) lives in
//! `crates/ipc/fixtures/*.json`. This test deserializes each fixture
//! into the matching protocol enum variant, re-serializes it, and
//! asserts the re-serialized value is semantically equal to the
//! original. If a field in the spec is missing from the Rust type,
//! the deserialize step fails here before the drift reaches
//! downstream crates.

use forgeclaw_ipc::{ContainerToHost, HostToContainer};
use serde_json::Value;

fn roundtrip_container_to_host(raw: &str, label: &str) {
    let original: Value = serde_json::from_str(raw).expect("fixture is valid JSON");
    let parsed: ContainerToHost =
        serde_json::from_value(original.clone()).expect("parse ContainerToHost fixture");
    let reserialized: Value =
        serde_json::to_value(&parsed).expect("serialize ContainerToHost fixture");
    assert_eq!(
        reserialized, original,
        "container_to_host fixture `{label}` roundtripped to different value"
    );
}

fn roundtrip_host_to_container(raw: &str, label: &str) {
    let original: Value = serde_json::from_str(raw).expect("fixture is valid JSON");
    let parsed: HostToContainer =
        serde_json::from_value(original.clone()).expect("parse HostToContainer fixture");
    let reserialized: Value =
        serde_json::to_value(&parsed).expect("serialize HostToContainer fixture");
    assert_eq!(
        reserialized, original,
        "host_to_container fixture `{label}` roundtripped to different value"
    );
}

#[test]
fn ready_fixture_roundtrip() {
    roundtrip_container_to_host(include_str!("../fixtures/ready.json"), "ready");
}

#[test]
fn output_delta_fixture_roundtrip() {
    roundtrip_container_to_host(
        include_str!("../fixtures/output_delta.json"),
        "output_delta",
    );
}

#[test]
fn output_complete_fixture_roundtrip() {
    roundtrip_container_to_host(
        include_str!("../fixtures/output_complete.json"),
        "output_complete",
    );
}

#[test]
fn progress_fixture_roundtrip() {
    roundtrip_container_to_host(include_str!("../fixtures/progress.json"), "progress");
}

#[test]
fn command_fixture_roundtrip() {
    roundtrip_container_to_host(include_str!("../fixtures/command.json"), "command");
}

#[test]
fn error_fixture_roundtrip() {
    roundtrip_container_to_host(include_str!("../fixtures/error.json"), "error");
}

#[test]
fn heartbeat_fixture_roundtrip() {
    roundtrip_container_to_host(include_str!("../fixtures/heartbeat.json"), "heartbeat");
}

#[test]
fn init_fixture_roundtrip() {
    roundtrip_host_to_container(include_str!("../fixtures/init.json"), "init");
}

#[test]
fn messages_fixture_roundtrip() {
    roundtrip_host_to_container(include_str!("../fixtures/messages.json"), "messages");
}

#[test]
fn shutdown_fixture_roundtrip() {
    roundtrip_host_to_container(include_str!("../fixtures/shutdown.json"), "shutdown");
}

// --- Command sub-type fixtures ---

#[test]
fn command_schedule_task_fixture_roundtrip() {
    roundtrip_container_to_host(
        include_str!("../fixtures/command_schedule_task.json"),
        "command_schedule_task",
    );
}

#[test]
fn command_pause_task_fixture_roundtrip() {
    roundtrip_container_to_host(
        include_str!("../fixtures/command_pause_task.json"),
        "command_pause_task",
    );
}

#[test]
fn command_cancel_task_fixture_roundtrip() {
    roundtrip_container_to_host(
        include_str!("../fixtures/command_cancel_task.json"),
        "command_cancel_task",
    );
}

#[test]
fn command_register_group_fixture_roundtrip() {
    roundtrip_container_to_host(
        include_str!("../fixtures/command_register_group.json"),
        "command_register_group",
    );
}

#[test]
fn command_dispatch_tanren_fixture_roundtrip() {
    roundtrip_container_to_host(
        include_str!("../fixtures/command_dispatch_tanren.json"),
        "command_dispatch_tanren",
    );
}

#[test]
fn command_dispatch_self_improvement_fixture_roundtrip() {
    roundtrip_container_to_host(
        include_str!("../fixtures/command_dispatch_self_improvement.json"),
        "command_dispatch_self_improvement",
    );
}
