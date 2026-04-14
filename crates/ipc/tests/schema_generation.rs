//! Generate JSON Schemas from the IPC message types and snapshot
//! them with insta to detect unintentional drift.
//!
//! Run with `cargo insta review` after intentional schema changes.

use forgeclaw_ipc::{ContainerToHost, HostToContainer};

#[test]
fn container_to_host_schema_snapshot() {
    let schema = schemars::schema_for!(ContainerToHost);
    let json = serde_json::to_string_pretty(&schema).expect("serialize schema");
    insta::assert_snapshot!("container_to_host_schema", json);
}

#[test]
fn host_to_container_schema_snapshot() {
    let schema = schemars::schema_for!(HostToContainer);
    let json = serde_json::to_string_pretty(&schema).expect("serialize schema");
    insta::assert_snapshot!("host_to_container_schema", json);
}
