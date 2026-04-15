use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use forgeclaw_core::GroupId;
use tokio::time::Instant;
use tracing_subscriber::fmt::MakeWriter;

use super::{
    ConnectionPhase, ConnectionState, UnauthorizedEnforcementAction, enforce_inbound_state,
    enforce_outbound_state, log_fatal_protocol_error, record_unauthorized_rejection,
    record_unauthorized_rejection_at,
};
use crate::error::{FrameError, IpcError, ProtocolError};
use crate::message::{
    CommandBody, CommandPayload, ContainerToHost, ErrorPayload, GroupCapabilities, GroupInfo,
    HistoricalMessages, HostToContainer, MessagesPayload, OutputCompletePayload,
    OutputDeltaPayload, ProgressPayload, SendMessagePayload, ShutdownPayload, ShutdownReason,
    StopReason,
};
use crate::peer_cred::SessionIdentity;
use crate::policy::UnknownFrameBudget;
use crate::server::UnauthorizedCommandLimitConfig;
use crate::version::{PROTOCOL_VERSION, negotiate};

fn state_with_job(now: Instant, job_id: &str) -> ConnectionState {
    ConnectionState::new(
        now,
        job_id.into(),
        negotiate(PROTOCOL_VERSION).expect("local version must negotiate"),
        UnauthorizedCommandLimitConfig::default(),
    )
}

fn idle_state(now: Instant, job_id: &str) -> ConnectionState {
    let mut state = state_with_job(now, job_id);
    state.lifecycle = crate::lifecycle::LifecycleState::new(job_id.into());
    state.lifecycle.clone_from(&{
        let mut lifecycle = crate::lifecycle::LifecycleState::new(job_id.into());
        let _ = crate::lifecycle::enforce_container_to_host(
            &mut lifecycle,
            &ContainerToHost::OutputComplete(OutputCompletePayload {
                job_id: job_id.into(),
                result: None,
                session_id: None,
                token_usage: None,
                stop_reason: StopReason::EndTurn,
            }),
        );
        lifecycle
    });
    state.heartbeat_deadline = None;
    state
}

fn draining_state(now: Instant, job_id: &str) -> ConnectionState {
    let mut state = state_with_job(now, job_id);
    let _ = crate::lifecycle::enforce_host_to_container(
        &mut state.lifecycle,
        &HostToContainer::Shutdown(ShutdownPayload {
            reason: ShutdownReason::HostShutdown,
            deadline_ms: 1000,
        }),
    );
    state.heartbeat_deadline = None;
    state
}

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
    let mut state = state_with_job(now, "job-1");
    enforce_outbound_state(
        &mut state,
        &HostToContainer::Shutdown(ShutdownPayload {
            reason: ShutdownReason::HostShutdown,
            deadline_ms: 1000,
        }),
        now,
    )
    .expect("shutdown allowed");
    assert_eq!(
        state.lifecycle.phase(),
        ConnectionPhase::DrainingAwaitingCompletion
    );

    let err = enforce_outbound_state(
        &mut state,
        &HostToContainer::Messages(MessagesPayload {
            job_id: "job-1".into(),
            messages: HistoricalMessages::default(),
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
    let mut state = idle_state(now, "job-1");
    let err = enforce_inbound_state(
        &mut state,
        &ContainerToHost::OutputDelta(OutputDeltaPayload {
            text: "x".parse().expect("valid text"),
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
    let mut state = state_with_job(now, "job-1");
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
    assert_eq!(state.lifecycle.phase(), ConnectionPhase::Idle);
}

#[test]
fn inbound_rejects_repeated_output_complete_while_draining() {
    let now = Instant::now();
    let mut state = draining_state(now, "job-1");
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
    assert_eq!(state.lifecycle.phase(), ConnectionPhase::Closed);

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

#[test]
fn inbound_rejects_command_when_idle() {
    let now = Instant::now();
    let mut state = idle_state(now, "job-1");
    let err = enforce_inbound_state(
        &mut state,
        &ContainerToHost::Command(CommandPayload {
            body: CommandBody::SendMessage(SendMessagePayload {
                target_group: "group-main".into(),
                text: "hello".parse().expect("valid text"),
            }),
        }),
        now,
    )
    .expect_err("command should require processing");
    assert!(matches!(
        err,
        IpcError::Protocol(ProtocolError::LifecycleViolation { .. })
    ));
}

#[test]
fn inbound_rejects_job_mismatch_for_output_delta() {
    let now = Instant::now();
    let mut state = state_with_job(now, "job-1");
    let err = enforce_inbound_state(
        &mut state,
        &ContainerToHost::OutputDelta(OutputDeltaPayload {
            text: "x".parse().expect("valid text"),
            job_id: "job-2".into(),
        }),
        now,
    )
    .expect_err("mismatched job should fail");
    assert!(matches!(
        err,
        IpcError::Protocol(ProtocolError::JobIdMismatch { .. })
    ));
}

#[test]
fn inbound_rejects_job_mismatch_for_progress() {
    let now = Instant::now();
    let mut state = state_with_job(now, "job-1");
    let err = enforce_inbound_state(
        &mut state,
        &ContainerToHost::Progress(ProgressPayload {
            job_id: "job-2".into(),
            stage: "stage".parse().expect("valid stage"),
            detail: None,
            percent: None,
        }),
        now,
    )
    .expect_err("mismatched job should fail");
    assert!(matches!(
        err,
        IpcError::Protocol(ProtocolError::JobIdMismatch { .. })
    ));
}

#[test]
fn inbound_rejects_job_mismatch_for_error_with_job() {
    let now = Instant::now();
    let mut state = state_with_job(now, "job-1");
    let err = enforce_inbound_state(
        &mut state,
        &ContainerToHost::Error(ErrorPayload {
            code: crate::message::ErrorCode::AdapterError,
            message: "boom".parse().expect("valid error message"),
            fatal: false,
            job_id: Some("job-2".into()),
        }),
        now,
    )
    .expect_err("mismatched error job should fail");
    assert!(matches!(
        err,
        IpcError::Protocol(ProtocolError::JobIdMismatch { .. })
    ));
}

#[test]
fn outbound_messages_reject_job_mismatch_during_processing() {
    let now = Instant::now();
    let mut state = state_with_job(now, "job-1");
    let err = enforce_outbound_state(
        &mut state,
        &HostToContainer::Messages(MessagesPayload {
            job_id: "job-2".into(),
            messages: HistoricalMessages::default(),
        }),
        now,
    )
    .expect_err("job mismatch should fail");
    assert!(matches!(
        err,
        IpcError::Protocol(ProtocolError::JobIdMismatch { .. })
    ));
}

#[test]
fn outbound_messages_rebind_job_when_idle() {
    let now = Instant::now();
    let mut state = idle_state(now, "job-1");
    enforce_outbound_state(
        &mut state,
        &HostToContainer::Messages(MessagesPayload {
            job_id: "job-2".into(),
            messages: HistoricalMessages::default(),
        }),
        now,
    )
    .expect("messages should rebind in idle");
    assert_eq!(state.lifecycle.phase(), ConnectionPhase::Processing);
    assert_eq!(state.lifecycle.active_job_id().as_ref(), "job-2");
}

#[test]
fn unauthorized_log_sampling_bursts_then_samples() {
    let now = Instant::now();
    let mut state = state_with_job(now, "job-1");
    for attempt in 1..=5 {
        let decision = record_unauthorized_rejection(&mut state).log;
        assert!(decision.should_log, "attempt {attempt} should log");
        assert_eq!(decision.attempt, attempt);
        assert_eq!(decision.suppressed_since_last, 0);
    }
    for attempt in 6..10 {
        let decision = record_unauthorized_rejection(&mut state).log;
        assert!(
            !decision.should_log,
            "attempt {attempt} should be sampled out"
        );
    }
    let tenth = record_unauthorized_rejection(&mut state).log;
    assert!(tenth.should_log);
    assert_eq!(tenth.attempt, 10);
    assert_eq!(tenth.suppressed_since_last, 4);
}

#[test]
fn unauthorized_log_sampling_reports_exact_suppressed_count_per_interval() {
    let now = Instant::now();
    let mut state = state_with_job(now, "job-1");

    for _ in 1..20 {
        let _ = record_unauthorized_rejection(&mut state);
    }

    let twentieth = record_unauthorized_rejection(&mut state).log;
    assert!(twentieth.should_log, "attempt 20 should be sampled in");
    assert_eq!(twentieth.attempt, 20);
    assert_eq!(
        twentieth.suppressed_since_last, 9,
        "attempt 20 should report exactly attempts 11..19 as suppressed"
    );
}

#[test]
fn unauthorized_limiter_applies_backoff_then_disconnects() {
    let now = Instant::now();
    let mut state = ConnectionState::new(
        now,
        "job-1".into(),
        negotiate(PROTOCOL_VERSION).expect("local version must negotiate"),
        UnauthorizedCommandLimitConfig {
            burst_capacity: 1,
            refill_per_second: 1,
            backoff: Duration::from_millis(200),
            disconnect_after_strikes: 2,
        },
    );

    let first = record_unauthorized_rejection_at(&mut state, now);
    assert_eq!(first.action, UnauthorizedEnforcementAction::Allow);

    let second = record_unauthorized_rejection_at(&mut state, now);
    assert_eq!(second.action, UnauthorizedEnforcementAction::Backoff);
    assert_eq!(second.log.strikes, 1);
    assert_eq!(second.log.backoff_ms, 200);

    let third = record_unauthorized_rejection_at(&mut state, now + Duration::from_millis(10));
    assert_eq!(third.action, UnauthorizedEnforcementAction::Disconnect);
    assert_eq!(third.log.strikes, 2);
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
            name: "Main".parse().expect("valid name"),
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
