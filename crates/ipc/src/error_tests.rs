use super::{FrameError, IpcError, ProtocolError};

#[test]
fn oversize_display_carries_counts_not_bytes() {
    let err = FrameError::Oversize {
        size: 20_000_000,
        max: 10_485_760,
    };
    let display = err.to_string();
    assert!(display.contains("20000000"));
    assert!(display.contains("10485760"));
}

#[test]
fn empty_frame_display() {
    assert_eq!(
        FrameError::EmptyFrame.to_string(),
        "empty frame (zero-length payload)"
    );
}

#[test]
fn truncated_display() {
    let err = FrameError::Truncated {
        expected: 100,
        got: 37,
    };
    assert_eq!(
        err.to_string(),
        "truncated frame: expected 100 bytes, got 37"
    );
}

#[test]
fn invalid_utf8_display() {
    assert_eq!(
        FrameError::InvalidUtf8.to_string(),
        "invalid UTF-8 in frame payload"
    );
}

#[test]
fn unsupported_version_display() {
    let err = ProtocolError::UnsupportedVersion {
        peer: "2.0".to_owned(),
        local: "1.0",
    };
    assert_eq!(
        err.to_string(),
        "unsupported protocol version: peer=2.0, local=1.0"
    );
}

#[test]
fn unexpected_message_display() {
    let err = ProtocolError::UnexpectedMessage {
        expected: "ready",
        got: "output_delta",
    };
    assert_eq!(
        err.to_string(),
        "unexpected message: expected ready, got output_delta"
    );
}

#[test]
fn unknown_message_type_display_includes_name() {
    let err = ProtocolError::UnknownMessageType("bogus".to_owned());
    assert!(err.to_string().contains("bogus"));
}

#[test]
fn not_command_display() {
    let err = ProtocolError::NotCommand { got: "heartbeat" };
    assert_eq!(err.to_string(), "not a command message: got heartbeat");
}

#[test]
fn frame_error_wraps_into_ipc_error() {
    let ipc: IpcError = FrameError::EmptyFrame.into();
    assert!(matches!(ipc, IpcError::Frame(FrameError::EmptyFrame)));
}

#[test]
fn io_error_wraps_into_ipc_error() {
    let io = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset");
    let ipc: IpcError = io.into();
    assert!(matches!(ipc, IpcError::Io(_)));
}

#[test]
fn bind_race_is_fatal() {
    let err = IpcError::BindRace("inode mismatch".to_owned());
    assert!(err.is_fatal());
}

#[test]
fn protocol_error_wraps_into_ipc_error() {
    let p = ProtocolError::UnknownMessageType("x".to_owned());
    let ipc: IpcError = p.into();
    assert!(matches!(ipc, IpcError::Protocol(_)));
}

#[test]
fn too_many_unknown_messages_display() {
    let err = ProtocolError::TooManyUnknownMessages {
        count: 33,
        limit: 32,
    };
    assert_eq!(
        err.to_string(),
        "too many unknown messages: 33 consecutive (limit 32)"
    );
}

#[test]
fn too_many_unknown_messages_is_fatal() {
    let err = IpcError::Protocol(ProtocolError::TooManyUnknownMessages {
        count: 33,
        limit: 32,
    });
    assert!(err.is_fatal());
}

#[test]
fn too_many_unknown_bytes_display() {
    let err = ProtocolError::TooManyUnknownBytes {
        bytes: 1_500_000,
        limit: 1_048_576,
    };
    assert_eq!(
        err.to_string(),
        "too many unknown message bytes: 1500000 bytes (limit 1048576)"
    );
}

#[test]
fn too_many_unknown_bytes_is_fatal() {
    let err = IpcError::Protocol(ProtocolError::TooManyUnknownBytes {
        bytes: 1_500_000,
        limit: 1_048_576,
    });
    assert!(err.is_fatal());
}

#[test]
fn too_many_unknown_messages_total_is_fatal() {
    let err = IpcError::Protocol(ProtocolError::TooManyUnknownMessagesTotal {
        count: 400,
        limit: 256,
    });
    assert!(err.is_fatal());
}

#[test]
fn too_many_unknown_bytes_total_is_fatal() {
    let err = IpcError::Protocol(ProtocolError::TooManyUnknownBytesTotal {
        bytes: 5_000_000,
        limit: 4_194_304,
    });
    assert!(err.is_fatal());
}

#[test]
fn unknown_message_rate_limit_exceeded_is_fatal() {
    let err = IpcError::Protocol(ProtocolError::UnknownMessageRateLimitExceeded {
        burst_capacity: 2,
        refill_per_second: 1,
    });
    assert!(err.is_fatal());
}

#[test]
fn outbound_validation_is_not_fatal() {
    let err = IpcError::Protocol(ProtocolError::OutboundValidation {
        direction: "container_to_host",
        message_type: "command",
        field_path: "payload.target_group".to_owned(),
        reason: "must contain at least one non-whitespace character".to_owned(),
    });
    assert!(!err.is_fatal());
}

#[test]
fn outbound_state_divergence_is_fatal() {
    let err = IpcError::Protocol(ProtocolError::OutboundStateDivergence {
        direction: "host_to_container",
        message_type: "messages",
        reason: "job changed between prepare and commit".to_owned(),
    });
    assert!(err.is_fatal());
}

#[test]
fn unauthorized_display() {
    let err = ProtocolError::Unauthorized {
        command: "register_group",
        reason: "requires main-group privilege",
    };
    let s = err.to_string();
    assert!(s.contains("register_group"));
    assert!(s.contains("requires main-group privilege"));
}

#[test]
fn unauthorized_is_not_fatal() {
    let err = IpcError::Protocol(ProtocolError::Unauthorized {
        command: "register_group",
        reason: "requires main-group privilege",
    });
    assert!(!err.is_fatal());
}

#[test]
fn unauthorized_command_abuse_display() {
    let err = ProtocolError::UnauthorizedCommandAbuse {
        command: "register_group",
        reason: "requires main-group privilege",
        strikes: 5,
        disconnect_after_strikes: 5,
    };
    let s = err.to_string();
    assert!(s.contains("register_group"));
    assert!(s.contains("5/5"));
}

#[test]
fn unauthorized_command_abuse_is_fatal() {
    let err = IpcError::Protocol(ProtocolError::UnauthorizedCommandAbuse {
        command: "register_group",
        reason: "requires main-group privilege",
        strikes: 5,
        disconnect_after_strikes: 5,
    });
    assert!(err.is_fatal());
}

#[test]
fn not_command_is_not_fatal() {
    let err = IpcError::Protocol(ProtocolError::NotCommand {
        got: "output_delta",
    });
    assert!(!err.is_fatal());
}

#[test]
fn group_mismatch_display() {
    let err = ProtocolError::GroupMismatch {
        init_group_id: forgeclaw_core::GroupId::new("group-a").expect("valid group id"),
        session_group_id: forgeclaw_core::GroupId::new("group-b").expect("valid group id"),
    };
    let s = err.to_string();
    assert!(s.contains("group-a"));
    assert!(s.contains("group-b"));
}

#[test]
fn group_mismatch_is_fatal() {
    let err = IpcError::Protocol(ProtocolError::GroupMismatch {
        init_group_id: forgeclaw_core::GroupId::new("group-a").expect("valid group id"),
        session_group_id: forgeclaw_core::GroupId::new("group-b").expect("valid group id"),
    });
    assert!(err.is_fatal());
}

#[test]
fn peer_credential_rejected_display() {
    let err = ProtocolError::PeerCredentialRejected {
        reason: "peer uid mismatch".to_owned(),
        expected_uid: Some(1000),
        expected_gid: Some(1000),
        actual_uid: Some(2000),
        actual_gid: Some(2000),
    };
    let s = err.to_string();
    assert!(s.contains("peer credential rejected"));
    assert!(s.contains("peer uid mismatch"));
}

#[test]
fn peer_credential_rejected_is_fatal() {
    let err = IpcError::Protocol(ProtocolError::PeerCredentialRejected {
        reason: "peer gid mismatch".to_owned(),
        expected_uid: None,
        expected_gid: Some(1000),
        actual_uid: Some(1000),
        actual_gid: Some(2000),
    });
    assert!(err.is_fatal());
}

#[test]
fn lifecycle_violation_is_fatal() {
    let err = IpcError::Protocol(ProtocolError::LifecycleViolation {
        phase: "idle",
        direction: "container_to_host",
        message_type: "output_delta",
        reason: "requires active processing",
    });
    assert!(err.is_fatal());
}

#[test]
fn job_id_mismatch_is_fatal() {
    let err = IpcError::Protocol(ProtocolError::JobIdMismatch {
        direction: "container_to_host",
        message_type: "output_delta",
        expected: forgeclaw_core::JobId::new("job-1").expect("valid job id"),
        got: forgeclaw_core::JobId::new("job-2").expect("valid job id"),
    });
    assert!(err.is_fatal());
}

#[test]
fn heartbeat_timeout_display() {
    let err = ProtocolError::HeartbeatTimeout {
        phase: "processing",
        timeout_secs: 60,
    };
    assert_eq!(
        err.to_string(),
        "heartbeat timeout: no heartbeat for 60s while in phase processing"
    );
}

#[test]
fn idle_read_timeout_display() {
    let err = ProtocolError::IdleReadTimeout {
        phase: "idle",
        timeout_secs: 60,
    };
    assert_eq!(
        err.to_string(),
        "idle read timeout: no full frame for 60s while in phase idle"
    );
}

#[test]
fn idle_read_timeout_is_fatal() {
    let err = IpcError::Protocol(ProtocolError::IdleReadTimeout {
        phase: "idle",
        timeout_secs: 60,
    });
    assert!(err.is_fatal());
}

#[test]
fn heartbeat_timeout_is_not_fatal() {
    let err = IpcError::Protocol(ProtocolError::HeartbeatTimeout {
        phase: "processing",
        timeout_secs: 60,
    });
    assert!(!err.is_fatal());
}

#[test]
fn shutdown_deadline_exceeded_display() {
    let err = ProtocolError::ShutdownDeadlineExceeded {
        phase: "draining",
        deadline_ms: 1_500,
    };
    assert_eq!(
        err.to_string(),
        "shutdown deadline exceeded: no completion within 1500ms while in phase draining"
    );
}

#[test]
fn shutdown_deadline_exceeded_is_not_fatal() {
    let err = IpcError::Protocol(ProtocolError::ShutdownDeadlineExceeded {
        phase: "draining",
        deadline_ms: 1_500,
    });
    assert!(!err.is_fatal());
}

#[test]
fn invalid_command_payload_display() {
    let err = ProtocolError::InvalidCommandPayload {
        command: "register_group",
        reason: "extensions.version is required".to_owned(),
    };
    assert_eq!(
        err.to_string(),
        "invalid command payload: register_group — extensions.version is required"
    );
}

#[test]
fn invalid_command_payload_is_fatal() {
    let err = IpcError::Protocol(ProtocolError::InvalidCommandPayload {
        command: "register_group",
        reason: "extensions.version is required".to_owned(),
    });
    assert!(err.is_fatal());
}
