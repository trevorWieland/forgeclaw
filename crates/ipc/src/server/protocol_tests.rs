use std::time::Duration;

use forgeclaw_core::{GroupId, JobId};
use tokio::time::Instant;

use super::{
    ConnectionPhase, ConnectionState, UnauthorizedEnforcementAction, enforce_inbound_state,
    enforce_outbound_state, record_unauthorized_rejection, record_unauthorized_rejection_at,
};
use crate::error::{IpcError, ProtocolError};
use crate::message::{
    CommandBody, CommandPayload, ContainerToHost, ErrorPayload, HistoricalMessages,
    HostToContainer, MessagesPayload, OutputCompletePayload, OutputDeltaPayload, ProgressPayload,
    SendMessagePayload, ShutdownPayload, ShutdownReason, StopReason,
};
use crate::policy::DEFAULT_HEARTBEAT_TIMEOUT;
use crate::policy::UnknownFrameBudget;
use crate::server::UnauthorizedCommandLimitConfig;
use crate::version::{PROTOCOL_VERSION, negotiate};

fn group_id(value: &str) -> GroupId {
    GroupId::new(value).expect("valid group id")
}

fn parse_job_id(value: &str) -> JobId {
    JobId::new(value).expect("valid job id")
}

fn state_with_job(now: Instant, job_id: &str) -> ConnectionState {
    ConnectionState::new(
        now,
        parse_job_id(job_id),
        negotiate(PROTOCOL_VERSION).expect("local version must negotiate"),
        UnauthorizedCommandLimitConfig::default(),
        Some(Duration::from_secs(60)),
    )
}

fn idle_state(now: Instant, job_id: &str) -> ConnectionState {
    let mut state = state_with_job(now, job_id);
    state.lifecycle = crate::lifecycle::LifecycleState::new(parse_job_id(job_id));
    state.lifecycle.clone_from(&{
        let mut lifecycle = crate::lifecycle::LifecycleState::new(parse_job_id(job_id));
        let _ = crate::lifecycle::enforce_container_to_host(
            &mut lifecycle,
            &ContainerToHost::OutputComplete(OutputCompletePayload {
                job_id: parse_job_id(job_id),
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
            job_id: parse_job_id("job-1"),
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
            job_id: parse_job_id("job-1"),
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
            job_id: parse_job_id("job-1"),
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
            job_id: parse_job_id("job-1"),
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
            job_id: parse_job_id("job-1"),
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
                target_group: group_id("group-main"),
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
            job_id: parse_job_id("job-2"),
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
            job_id: parse_job_id("job-2"),
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
            job_id: Some(parse_job_id("job-2")),
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
            job_id: parse_job_id("job-2"),
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
            job_id: parse_job_id("job-2"),
            messages: HistoricalMessages::default(),
        }),
        now,
    )
    .expect("messages should rebind in idle");
    assert_eq!(state.lifecycle.phase(), ConnectionPhase::Processing);
    assert_eq!(state.lifecycle.active_job_id().as_ref(), "job-2");
}

#[test]
fn outbound_messages_does_not_extend_processing_heartbeat_deadline() {
    let now = Instant::now();
    let mut state = state_with_job(now, "job-1");
    let initial_deadline = state
        .heartbeat_deadline
        .expect("processing state should have heartbeat deadline");

    let later = now + Duration::from_secs(10);
    enforce_outbound_state(
        &mut state,
        &HostToContainer::Messages(MessagesPayload {
            job_id: parse_job_id("job-1"),
            messages: HistoricalMessages::default(),
        }),
        later,
    )
    .expect("messages allowed during processing");

    assert_eq!(
        state.heartbeat_deadline,
        Some(initial_deadline),
        "outbound messages must not refresh heartbeat while already processing"
    );
}

#[test]
fn outbound_messages_arms_fresh_heartbeat_when_reentering_processing_from_idle() {
    let now = Instant::now();
    let mut state = idle_state(now, "job-1");
    state.heartbeat_deadline = None;
    let later = now + Duration::from_secs(7);

    enforce_outbound_state(
        &mut state,
        &HostToContainer::Messages(MessagesPayload {
            job_id: parse_job_id("job-2"),
            messages: HistoricalMessages::default(),
        }),
        later,
    )
    .expect("messages should re-enter processing from idle");

    assert_eq!(
        state.heartbeat_deadline,
        Some(later + DEFAULT_HEARTBEAT_TIMEOUT),
        "idle->processing should arm a new heartbeat deadline"
    );
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
        parse_job_id("job-1"),
        negotiate(PROTOCOL_VERSION).expect("local version must negotiate"),
        UnauthorizedCommandLimitConfig {
            burst_capacity: 1,
            refill_per_second: 1,
            backoff: Duration::from_millis(200),
            disconnect_after_strikes: 2,
        },
        Some(Duration::from_secs(60)),
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
