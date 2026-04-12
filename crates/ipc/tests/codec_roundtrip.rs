//! Proptest-driven roundtrip for every protocol message variant.
//!
//! Every message type defined in
//! [`docs/IPC_PROTOCOL.md`](../../../docs/IPC_PROTOCOL.md) has a
//! proptest strategy here, and each strategy is wired through an
//! encode → frame → decode → compare loop. The loop also simulates a
//! slow socket by feeding the framed bytes into the decoder one
//! small chunk at a time, which exercises the partial-read path of
//! [`forgeclaw_ipc::FrameCodec::decode`] that normal single-buffer
//! tests would not hit.

use bytes::{Bytes, BytesMut};
use forgeclaw_core::{GroupId, JobId, TaskId};
use forgeclaw_ipc::{
    CancelTaskPayload, CommandBody, CommandPayload, ContainerToHost,
    DispatchSelfImprovementPayload, DispatchTanrenPayload, ErrorCode, ErrorPayload, FrameCodec,
    GroupInfo, HeartbeatPayload, HistoricalMessage, HostToContainer, InitConfig, InitContext,
    InitPayload, MessagesPayload, OutputCompletePayload, OutputDeltaPayload, PauseTaskPayload,
    ProgressPayload, ReadyPayload, RegisterGroupPayload, ScheduleTaskPayload, SendMessagePayload,
    ShutdownPayload, ShutdownReason, StopReason, TokenUsage,
};
use proptest::prelude::*;
use tokio_util::codec::{Decoder, Encoder};

// --- strategies --------------------------------------------------------

fn bounded_string() -> impl Strategy<Value = String> {
    // Keep strings small enough that any message variant stays well
    // under MAX_FRAME_BYTES. 1 KiB is plenty for spec coverage and
    // keeps proptest fast.
    "\\PC{0,1024}".prop_map(String::from)
}

fn short_string() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9_-]{1,32}".prop_map(String::from)
}

fn job_id_strategy() -> impl Strategy<Value = JobId> {
    short_string().prop_map(JobId::from)
}

fn group_id_strategy() -> impl Strategy<Value = GroupId> {
    short_string().prop_map(GroupId::from)
}

fn stop_reason_strategy() -> impl Strategy<Value = StopReason> {
    prop_oneof![
        Just(StopReason::EndTurn),
        Just(StopReason::MaxTokens),
        Just(StopReason::ToolUse),
    ]
}

fn error_code_strategy() -> impl Strategy<Value = ErrorCode> {
    prop_oneof![
        Just(ErrorCode::ProviderError),
        Just(ErrorCode::ToolError),
        Just(ErrorCode::AdapterError),
        Just(ErrorCode::ProtocolError),
        Just(ErrorCode::Timeout),
    ]
}

fn shutdown_reason_strategy() -> impl Strategy<Value = ShutdownReason> {
    prop_oneof![
        Just(ShutdownReason::IdleTimeout),
        Just(ShutdownReason::HostShutdown),
        Just(ShutdownReason::Eviction),
        Just(ShutdownReason::ErrorRecovery),
    ]
}

fn token_usage_strategy() -> impl Strategy<Value = TokenUsage> {
    (0u64..100_000, 0u64..100_000).prop_map(|(input_tokens, output_tokens)| TokenUsage {
        input_tokens,
        output_tokens,
    })
}

fn ready_strategy() -> impl Strategy<Value = ReadyPayload> {
    (short_string(), short_string()).prop_map(|(adapter, adapter_version)| ReadyPayload {
        adapter,
        adapter_version,
        // Fixed at the crate's supported version so the handshake
        // path in other tests can also reuse this strategy.
        protocol_version: "1.0".to_owned(),
    })
}

fn output_delta_strategy() -> impl Strategy<Value = OutputDeltaPayload> {
    (bounded_string(), job_id_strategy())
        .prop_map(|(text, job_id)| OutputDeltaPayload { text, job_id })
}

fn output_complete_strategy() -> impl Strategy<Value = OutputCompletePayload> {
    (
        job_id_strategy(),
        proptest::option::of(bounded_string()),
        proptest::option::of(short_string()),
        proptest::option::of(token_usage_strategy()),
        stop_reason_strategy(),
    )
        .prop_map(|(job_id, result, session_id, token_usage, stop_reason)| {
            OutputCompletePayload {
                job_id,
                result,
                session_id,
                token_usage,
                stop_reason,
            }
        })
}

fn progress_strategy() -> impl Strategy<Value = ProgressPayload> {
    (
        job_id_strategy(),
        short_string(),
        proptest::option::of(bounded_string()),
        proptest::option::of(0u8..=100),
    )
        .prop_map(|(job_id, stage, detail, percent)| ProgressPayload {
            job_id,
            stage,
            detail,
            percent,
        })
}

fn task_id_strategy() -> impl Strategy<Value = TaskId> {
    short_string().prop_map(TaskId::from)
}

fn command_strategy() -> impl Strategy<Value = CommandPayload> {
    let variant = prop_oneof![
        (group_id_strategy(), bounded_string()).prop_map(|(tg, t)| {
            CommandBody::SendMessage(SendMessagePayload {
                target_group: tg,
                text: t,
            })
        }),
        (
            group_id_strategy(),
            short_string(),
            short_string(),
            bounded_string(),
            proptest::option::of(short_string()),
        )
            .prop_map(|(g, st, sv, p, cm)| {
                CommandBody::ScheduleTask(ScheduleTaskPayload {
                    group: g,
                    schedule_type: st,
                    schedule_value: sv,
                    prompt: p,
                    context_mode: cm,
                })
            }),
        task_id_strategy()
            .prop_map(|tid| CommandBody::PauseTask(PauseTaskPayload { task_id: tid })),
        task_id_strategy()
            .prop_map(|tid| CommandBody::CancelTask(CancelTaskPayload { task_id: tid })),
        Just(CommandBody::RegisterGroup(RegisterGroupPayload {
            group_spec: serde_json::json!({"name": "g"}),
        })),
        (
            short_string(),
            short_string(),
            short_string(),
            bounded_string(),
            proptest::option::of(short_string()),
        )
            .prop_map(|(proj, br, ph, pr, ep)| {
                CommandBody::DispatchTanren(DispatchTanrenPayload {
                    project: proj,
                    branch: br,
                    phase: ph,
                    prompt: pr,
                    environment_profile: ep,
                })
            }),
        (
            bounded_string(),
            short_string(),
            bounded_string(),
            short_string(),
        )
            .prop_map(|(obj, sc, at, bp)| {
                CommandBody::DispatchSelfImprovement(DispatchSelfImprovementPayload {
                    objective: obj,
                    scope: sc,
                    acceptance_tests: at,
                    branch_policy: bp,
                })
            }),
    ];
    variant.prop_map(|body| CommandPayload { body })
}

fn error_payload_strategy() -> impl Strategy<Value = ErrorPayload> {
    (
        error_code_strategy(),
        bounded_string(),
        any::<bool>(),
        proptest::option::of(job_id_strategy()),
    )
        .prop_map(|(code, message, fatal, job_id)| ErrorPayload {
            code,
            message,
            fatal,
            job_id,
        })
}

fn heartbeat_strategy() -> impl Strategy<Value = HeartbeatPayload> {
    short_string().prop_map(|timestamp| HeartbeatPayload { timestamp })
}

fn container_to_host_strategy() -> impl Strategy<Value = ContainerToHost> {
    prop_oneof![
        ready_strategy().prop_map(ContainerToHost::Ready),
        output_delta_strategy().prop_map(ContainerToHost::OutputDelta),
        output_complete_strategy().prop_map(ContainerToHost::OutputComplete),
        progress_strategy().prop_map(ContainerToHost::Progress),
        command_strategy().prop_map(ContainerToHost::Command),
        error_payload_strategy().prop_map(ContainerToHost::Error),
        heartbeat_strategy().prop_map(ContainerToHost::Heartbeat),
    ]
}

fn historical_message_strategy() -> impl Strategy<Value = HistoricalMessage> {
    (short_string(), bounded_string(), short_string()).prop_map(|(sender, text, timestamp)| {
        HistoricalMessage {
            sender,
            text,
            timestamp,
        }
    })
}

fn group_info_strategy() -> impl Strategy<Value = GroupInfo> {
    (group_id_strategy(), short_string(), any::<bool>()).prop_map(|(id, name, is_main)| GroupInfo {
        id,
        name,
        is_main,
    })
}

fn init_context_strategy() -> impl Strategy<Value = InitContext> {
    (
        proptest::collection::vec(historical_message_strategy(), 0..4),
        group_info_strategy(),
        short_string(),
    )
        .prop_map(|(messages, group, timezone)| InitContext {
            messages,
            group,
            timezone,
        })
}

fn init_config_strategy() -> impl Strategy<Value = InitConfig> {
    (
        short_string(),
        short_string(),
        short_string(),
        1u32..=100_000,
        proptest::option::of(short_string()),
        any::<bool>(),
        1u32..=86_400,
    )
        .prop_map(
            |(
                provider_proxy_url,
                provider_proxy_token,
                model,
                max_tokens,
                session_id,
                tools_enabled,
                timeout_seconds,
            )| InitConfig {
                provider_proxy_url,
                provider_proxy_token,
                model,
                max_tokens,
                session_id,
                tools_enabled,
                timeout_seconds,
            },
        )
}

fn init_payload_strategy() -> impl Strategy<Value = InitPayload> {
    (
        job_id_strategy(),
        init_context_strategy(),
        init_config_strategy(),
    )
        .prop_map(|(job_id, context, config)| InitPayload {
            job_id,
            context,
            config,
        })
}

fn messages_payload_strategy() -> impl Strategy<Value = MessagesPayload> {
    (
        job_id_strategy(),
        proptest::collection::vec(historical_message_strategy(), 0..4),
    )
        .prop_map(|(job_id, messages)| MessagesPayload { job_id, messages })
}

fn shutdown_payload_strategy() -> impl Strategy<Value = ShutdownPayload> {
    (shutdown_reason_strategy(), 0u64..=600_000).prop_map(|(reason, deadline_ms)| ShutdownPayload {
        reason,
        deadline_ms,
    })
}

fn host_to_container_strategy() -> impl Strategy<Value = HostToContainer> {
    prop_oneof![
        init_payload_strategy().prop_map(HostToContainer::Init),
        messages_payload_strategy().prop_map(HostToContainer::Messages),
        shutdown_payload_strategy().prop_map(HostToContainer::Shutdown),
    ]
}

// --- helpers -----------------------------------------------------------

fn roundtrip_container_to_host(msg: &ContainerToHost) {
    let json = serde_json::to_vec(msg).expect("serialize");
    let bytes = Bytes::from(json);

    let mut codec = FrameCodec::new();
    let mut wire = BytesMut::new();
    codec.encode(bytes.clone(), &mut wire).expect("encode");

    // Feed the decoder one byte at a time to exercise the
    // partial-read path.
    let mut reader = FrameCodec::new();
    let mut buf = BytesMut::new();
    let mut out: Option<Bytes> = None;
    for byte in &wire {
        buf.extend_from_slice(&[*byte]);
        if let Some(frame) = reader.decode(&mut buf).expect("chunked decode") {
            out = Some(frame);
            break;
        }
    }
    let frame = out.expect("full frame decoded");
    let back: ContainerToHost = serde_json::from_slice(&frame).expect("typed decode");
    assert_eq!(&back, msg);
}

fn roundtrip_host_to_container(msg: &HostToContainer) {
    let json = serde_json::to_vec(msg).expect("serialize");
    let bytes = Bytes::from(json);

    let mut codec = FrameCodec::new();
    let mut wire = BytesMut::new();
    codec.encode(bytes.clone(), &mut wire).expect("encode");

    let mut reader = FrameCodec::new();
    let mut buf = BytesMut::new();
    let mut out: Option<Bytes> = None;
    for byte in &wire {
        buf.extend_from_slice(&[*byte]);
        if let Some(frame) = reader.decode(&mut buf).expect("chunked decode") {
            out = Some(frame);
            break;
        }
    }
    let frame = out.expect("full frame decoded");
    let back: HostToContainer = serde_json::from_slice(&frame).expect("typed decode");
    assert_eq!(&back, msg);
}

// --- tests -------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn container_to_host_roundtrips_over_chunked_reads(
        msg in container_to_host_strategy()
    ) {
        roundtrip_container_to_host(&msg);
    }

    #[test]
    fn host_to_container_roundtrips_over_chunked_reads(
        msg in host_to_container_strategy()
    ) {
        roundtrip_host_to_container(&msg);
    }
}
