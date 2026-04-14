use std::io::Write;
use std::sync::{Arc, Mutex};

use forgeclaw_core::GroupId;
use tokio::time::Instant;
use tracing_subscriber::fmt::MakeWriter;

use super::{
    ConnectionPhase, ConnectionState, enforce_inbound_state, enforce_outbound_state,
    log_fatal_protocol_error,
};
use crate::error::{FrameError, IpcError, ProtocolError};
use crate::message::{
    ContainerToHost, GroupCapabilities, GroupInfo, HostToContainer, MessagesPayload,
    OutputCompletePayload, OutputDeltaPayload, ShutdownPayload, ShutdownReason, StopReason,
};
use crate::peer_cred::SessionIdentity;
use crate::policy::UnknownFrameBudget;

#[test]
fn unknown_budget_enforces_byte_limit() {
    let mut budget = UnknownFrameBudget::default();
    budget.on_unknown(700_000).expect("first chunk");
    let err = budget.on_unknown(400_000).expect_err("should exceed 1 MiB");
    assert!(matches!(
        err,
        IpcError::Protocol(ProtocolError::TooManyUnknownBytes {
            bytes: 1_100_000,
            limit: 1_048_576
        })
    ));
}

#[test]
fn unknown_budget_resets_on_known_message() {
    let mut budget = UnknownFrameBudget::default();
    budget.on_unknown(700_000).expect("first chunk");
    budget.reset();
    budget.on_unknown(700_000).expect("budget should reset");
}

#[test]
fn lifecycle_transitions_are_enforced() {
    let now = Instant::now();
    let mut state = ConnectionState::new(now);
    enforce_outbound_state(
        &mut state,
        &HostToContainer::Shutdown(ShutdownPayload {
            reason: ShutdownReason::HostShutdown,
            deadline_ms: 1000,
        }),
        now,
    )
    .expect("shutdown allowed");
    assert_eq!(state.phase, ConnectionPhase::DrainingAwaitingCompletion);

    let err = enforce_outbound_state(
        &mut state,
        &HostToContainer::Messages(MessagesPayload {
            job_id: "job-1".into(),
            messages: vec![],
        }),
        now,
    )
    .expect_err("messages should be rejected while draining");
    assert!(matches!(
        err,
        IpcError::Protocol(ProtocolError::LifecycleViolation { .. })
    ));
}

#[test]
fn inbound_output_delta_rejected_when_idle() {
    let now = Instant::now();
    let mut state = ConnectionState {
        phase: ConnectionPhase::Idle,
        heartbeat_deadline: None,
    };
    let err = enforce_inbound_state(
        &mut state,
        &ContainerToHost::OutputDelta(OutputDeltaPayload {
            text: "x".to_owned(),
            job_id: "job-1".into(),
        }),
        now,
    )
    .expect_err("output_delta requires processing");
    assert!(matches!(
        err,
        IpcError::Protocol(ProtocolError::LifecycleViolation { .. })
    ));
}

#[test]
fn inbound_output_complete_transitions_processing_to_idle() {
    let now = Instant::now();
    let mut state = ConnectionState::new(now);
    enforce_inbound_state(
        &mut state,
        &ContainerToHost::OutputComplete(OutputCompletePayload {
            job_id: "job-1".into(),
            result: None,
            session_id: None,
            token_usage: None,
            stop_reason: StopReason::EndTurn,
        }),
        now,
    )
    .expect("complete should be accepted");
    assert_eq!(state.phase, ConnectionPhase::Idle);
}

#[test]
fn inbound_rejects_repeated_output_complete_while_draining() {
    let now = Instant::now();
    let mut state = ConnectionState {
        phase: ConnectionPhase::DrainingAwaitingCompletion,
        heartbeat_deadline: None,
    };
    enforce_inbound_state(
        &mut state,
        &ContainerToHost::OutputComplete(OutputCompletePayload {
            job_id: "job-1".into(),
            result: None,
            session_id: None,
            token_usage: None,
            stop_reason: StopReason::EndTurn,
        }),
        now,
    )
    .expect("first completion should be accepted");
    assert_eq!(state.phase, ConnectionPhase::DrainingCompleted);

    let err = enforce_inbound_state(
        &mut state,
        &ContainerToHost::OutputComplete(OutputCompletePayload {
            job_id: "job-1".into(),
            result: None,
            session_id: None,
            token_usage: None,
            stop_reason: StopReason::EndTurn,
        }),
        now,
    )
    .expect_err("second completion should fail");
    assert!(matches!(
        err,
        IpcError::Protocol(ProtocolError::LifecycleViolation { .. })
    ));
}

#[derive(Clone, Default)]
struct CaptureWriter {
    inner: Arc<Mutex<Vec<u8>>>,
}

impl CaptureWriter {
    fn output(&self) -> String {
        let bytes = self.inner.lock().expect("lock").clone();
        String::from_utf8(bytes).expect("utf8 log output")
    }
}

struct CaptureGuard {
    inner: Arc<Mutex<Vec<u8>>>,
}

impl Write for CaptureGuard {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.lock().expect("lock").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for CaptureWriter {
    type Writer = CaptureGuard;

    fn make_writer(&'a self) -> Self::Writer {
        CaptureGuard {
            inner: Arc::clone(&self.inner),
        }
    }
}

fn sample_identity() -> Arc<Mutex<SessionIdentity>> {
    Arc::new(Mutex::new(SessionIdentity::new(
        None,
        GroupInfo {
            id: GroupId::from("group-main"),
            name: "Main".to_owned(),
            is_main: true,
            capabilities: GroupCapabilities::default(),
        },
    )))
}

#[test]
fn fatal_logging_includes_identity_and_error_class() {
    let capture = CaptureWriter::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_ansi(false)
        .without_time()
        .with_writer(capture.clone())
        .finish();
    let dispatch = tracing::Dispatch::new(subscriber);
    let identity = sample_identity();
    tracing::dispatcher::with_default(&dispatch, || {
        log_fatal_protocol_error(
            &identity,
            "handshake",
            &IpcError::Frame(FrameError::InvalidUtf8),
        );
    });
    let output = capture.output();
    assert!(
        output.contains("\"protocol_phase\":\"handshake\""),
        "{output}"
    );
    assert!(
        output.contains("\"error_class\":\"frame_invalid_utf8\""),
        "{output}"
    );
    assert!(output.contains("\"group_id\":\"group-main\""), "{output}");
}

#[test]
fn fatal_logging_truncates_peer_controlled_protocol_version() {
    let capture = CaptureWriter::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_ansi(false)
        .without_time()
        .with_writer(capture.clone())
        .finish();
    let dispatch = tracing::Dispatch::new(subscriber);
    let identity = sample_identity();
    let long_peer = "x".repeat(8_000);
    let err = IpcError::Protocol(ProtocolError::UnsupportedVersion {
        peer: long_peer.clone(),
        local: "1.0",
    });

    tracing::dispatcher::with_default(&dispatch, || {
        log_fatal_protocol_error(&identity, "handshake", &err);
    });

    let output = capture.output();
    assert!(output.contains("<truncated"), "{output}");
    assert!(
        !output.contains(&long_peer),
        "full peer value leaked to logs"
    );
}
