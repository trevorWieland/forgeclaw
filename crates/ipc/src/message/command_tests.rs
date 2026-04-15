use forgeclaw_core::{GroupId, TaskId};
use serde_json::json;

use super::*;

fn sample_extensions() -> GroupExtensions {
    let mut ext = GroupExtensions::new(GroupExtensionsVersion::new("1").expect("valid version"));
    ext.insert("trigger", json!("@bot"))
        .expect("insert non-reserved key");
    ext
}

#[test]
fn send_message_roundtrip() {
    let cmd = CommandPayload {
        body: CommandBody::SendMessage(SendMessagePayload {
            target_group: GroupId::from("group-main"),
            text: "Task completed successfully.".parse().expect("valid text"),
        }),
    };
    let json = serde_json::to_value(&cmd).expect("serialize");
    assert_eq!(json["command"], "send_message");
    assert_eq!(json["payload"]["target_group"], "group-main");
    assert_eq!(json["payload"]["text"], "Task completed successfully.");
    let back: CommandPayload = serde_json::from_value(json).expect("deserialize");
    assert_eq!(back, cmd);
}

#[test]
fn schedule_task_roundtrip() {
    let cmd = CommandPayload {
        body: CommandBody::ScheduleTask(ScheduleTaskPayload {
            group: GroupId::from("group-main"),
            schedule_type: ScheduleType::Cron,
            schedule_value: "0 9 * * *".parse().expect("valid schedule"),
            prompt: "Check status".parse().expect("valid prompt"),
            context_mode: Some("full".parse().expect("valid context mode")),
        }),
    };
    let json = serde_json::to_value(&cmd).expect("serialize");
    assert_eq!(json["command"], "schedule_task");
    assert_eq!(json["payload"]["schedule_type"], "cron");
    let back: CommandPayload = serde_json::from_value(json).expect("deserialize");
    assert_eq!(back, cmd);
}

#[test]
fn schedule_task_omits_context_mode_when_none() {
    let cmd = CommandPayload {
        body: CommandBody::ScheduleTask(ScheduleTaskPayload {
            group: GroupId::from("g"),
            schedule_type: ScheduleType::Once,
            schedule_value: "2026-04-12T00:00:00Z".parse().expect("valid schedule"),
            prompt: "p".parse().expect("valid prompt"),
            context_mode: None,
        }),
    };
    let json = serde_json::to_value(&cmd).expect("serialize");
    let payload = json["payload"].as_object().expect("payload object");
    assert!(!payload.contains_key("context_mode"));
}

#[test]
fn pause_task_roundtrip() {
    let cmd = CommandPayload {
        body: CommandBody::PauseTask(PauseTaskPayload {
            task_id: TaskId::from("task-1"),
        }),
    };
    let json = serde_json::to_value(&cmd).expect("serialize");
    assert_eq!(json["command"], "pause_task");
    assert_eq!(json["payload"]["task_id"], "task-1");
    let back: CommandPayload = serde_json::from_value(json).expect("deserialize");
    assert_eq!(back, cmd);
}

#[test]
fn cancel_task_roundtrip() {
    let cmd = CommandPayload {
        body: CommandBody::CancelTask(CancelTaskPayload {
            task_id: TaskId::from("task-2"),
        }),
    };
    let json = serde_json::to_value(&cmd).expect("serialize");
    assert_eq!(json["command"], "cancel_task");
    let back: CommandPayload = serde_json::from_value(json).expect("deserialize");
    assert_eq!(back, cmd);
}

#[test]
fn register_group_roundtrip() {
    let cmd = CommandPayload {
        body: CommandBody::RegisterGroup(RegisterGroupPayload {
            name: "New Group".parse().expect("valid name"),
            extensions: Some(sample_extensions()),
        }),
    };
    let json = serde_json::to_value(&cmd).expect("serialize");
    assert_eq!(json["command"], "register_group");
    assert_eq!(json["payload"]["name"], "New Group");
    assert_eq!(json["payload"]["extensions"]["version"], "1");
    assert_eq!(json["payload"]["extensions"]["trigger"], "@bot");
    let back: CommandPayload = serde_json::from_value(json).expect("deserialize");
    assert_eq!(back, cmd);
}

#[test]
fn group_extensions_version_rejects_whitespace_only() {
    let err = GroupExtensionsVersion::new("   ").expect_err("whitespace-only must fail");
    assert!(matches!(
        err,
        GroupExtensionsVersionError::EmptyOrWhitespace
    ));
}

#[test]
fn group_extensions_with_data_rejects_reserved_key() {
    let mut data = serde_json::Map::new();
    data.insert("version".to_owned(), json!("bad"));
    let err = GroupExtensions::with_data(GroupExtensionsVersion::new("1").expect("valid"), data)
        .expect_err("reserved key must fail");
    assert!(matches!(err, GroupExtensionsError::ReservedKey(_)));
}

#[test]
fn group_extensions_insert_rejects_reserved_key() {
    let mut ext = GroupExtensions::new(GroupExtensionsVersion::new("1").expect("valid"));
    let err = ext
        .insert("version", json!("x"))
        .expect_err("reserved key must fail");
    assert!(matches!(err, GroupExtensionsError::ReservedKey(_)));
}

#[test]
fn register_group_extensions_missing_version_rejected_by_deserializer() {
    let raw = r#"{
        "command":"register_group",
        "payload":{"name":"g","extensions":{"trigger":"@bot"}}
    }"#;
    serde_json::from_str::<CommandPayload>(raw).expect_err("missing version must fail");
}

#[test]
fn register_group_extensions_duplicate_version_rejected_by_deserializer() {
    let raw = r#"{
        "command":"register_group",
        "payload":{"name":"g","extensions":{"version":"1","version":"2"}}
    }"#;
    serde_json::from_str::<CommandPayload>(raw).expect_err("duplicate version must fail");
}

#[test]
fn register_group_omits_extensions_when_none() {
    let cmd = CommandPayload {
        body: CommandBody::RegisterGroup(RegisterGroupPayload {
            name: "Minimal Group".parse().expect("valid name"),
            extensions: None,
        }),
    };
    let json = serde_json::to_value(&cmd).expect("serialize");
    let payload = json["payload"].as_object().expect("payload object");
    assert!(!payload.contains_key("extensions"));
}

#[test]
fn dispatch_tanren_roundtrip() {
    let cmd = CommandPayload {
        body: CommandBody::DispatchTanren(DispatchTanrenPayload {
            project: "forgeclaw".parse().expect("valid project"),
            branch: "main".parse().expect("valid branch"),
            phase: TanrenPhase::DoTask,
            prompt: "Implement feature X".parse().expect("valid prompt"),
            environment_profile: Some("large-vm".parse().expect("valid environment profile")),
        }),
    };
    let json = serde_json::to_value(&cmd).expect("serialize");
    assert_eq!(json["command"], "dispatch_tanren");
    assert_eq!(json["payload"]["phase"], "do-task");
    let back: CommandPayload = serde_json::from_value(json).expect("deserialize");
    assert_eq!(back, cmd);
}

#[test]
fn dispatch_self_improvement_roundtrip() {
    let cmd = CommandPayload {
        body: CommandBody::DispatchSelfImprovement(DispatchSelfImprovementPayload {
            objective: "Add retry logic".parse().expect("valid objective"),
            scopes: vec!["crates/ipc".parse().expect("valid scope")]
                .try_into()
                .expect("valid scopes"),
            acceptance_tests: vec![
                "cargo nextest run -p forgeclaw-ipc"
                    .parse()
                    .expect("valid acceptance test"),
            ]
            .try_into()
            .expect("valid acceptance tests"),
            branch_policy: BranchPolicy::Create,
        }),
    };
    let json = serde_json::to_value(&cmd).expect("serialize");
    assert_eq!(json["command"], "dispatch_self_improvement");
    assert!(json["payload"]["scopes"].is_array());
    assert!(json["payload"]["acceptance_tests"].is_array());
    let back: CommandPayload = serde_json::from_value(json).expect("deserialize");
    assert_eq!(back, cmd);
}

#[test]
fn classify_returns_scoped_for_group_commands() {
    let scoped_commands = [
        CommandBody::SendMessage(SendMessagePayload {
            target_group: GroupId::from("g"),
            text: "t".parse().expect("valid text"),
        }),
        CommandBody::ScheduleTask(ScheduleTaskPayload {
            group: GroupId::from("g"),
            schedule_type: ScheduleType::Once,
            schedule_value: "v".parse().expect("valid schedule"),
            prompt: "p".parse().expect("valid prompt"),
            context_mode: None,
        }),
        CommandBody::PauseTask(PauseTaskPayload {
            task_id: TaskId::from("t"),
        }),
        CommandBody::CancelTask(CancelTaskPayload {
            task_id: TaskId::from("t"),
        }),
        CommandBody::DispatchTanren(DispatchTanrenPayload {
            project: "p".parse().expect("valid project"),
            branch: "b".parse().expect("valid branch"),
            phase: TanrenPhase::DoTask,
            prompt: "pr".parse().expect("valid prompt"),
            environment_profile: None,
        }),
    ];
    for cmd in scoped_commands {
        assert!(!cmd.is_privileged());
        assert!(matches!(cmd.classify(), ClassifiedCommand::Scoped(_)));
    }
}

#[test]
fn classify_returns_privileged_for_main_commands() {
    let privileged_commands = [
        CommandBody::RegisterGroup(RegisterGroupPayload {
            name: "g".parse().expect("valid name"),
            extensions: None,
        }),
        CommandBody::DispatchSelfImprovement(DispatchSelfImprovementPayload {
            objective: "o".parse().expect("valid objective"),
            scopes: vec!["s".parse().expect("valid scope")]
                .try_into()
                .expect("valid scopes"),
            acceptance_tests: vec!["t".parse().expect("valid acceptance test")]
                .try_into()
                .expect("valid acceptance tests"),
            branch_policy: BranchPolicy::Create,
        }),
    ];
    for cmd in privileged_commands {
        assert!(cmd.is_privileged());
        assert!(matches!(cmd.classify(), ClassifiedCommand::Privileged(_)));
    }
}
