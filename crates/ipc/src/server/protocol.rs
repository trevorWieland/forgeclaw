//! Shared server-side protocol enforcement helpers.

use std::time::Duration;

use tokio::time::Instant;

use crate::error::{IpcError, ProtocolError};
use crate::lifecycle::{
    ConnectionPhase, LifecycleAction, LifecycleState, enforce_container_to_host,
    enforce_host_to_container,
};
use crate::message::{ContainerToHost, HostToContainer};
use crate::policy::DEFAULT_HEARTBEAT_TIMEOUT;
use crate::semantics::{ProtocolSemantics, validate_container_to_host, validate_host_to_container};
use crate::version::NegotiatedProtocolVersion;

use super::listener::UnauthorizedCommandLimitConfig;

const UNAUTHORIZED_LOG_BURST: u64 = 5;
const UNAUTHORIZED_LOG_EVERY: u64 = 10;

#[path = "protocol_logging.rs"]
mod logging;

pub(super) use logging::{
    log_fatal_protocol_error, log_outbound_validation_rejection, log_unauthorized_command,
    log_unknown_message,
};

/// Per-connection runtime protocol state.
#[derive(Debug, Clone)]
pub(super) struct ConnectionState {
    lifecycle: LifecycleState,
    semantics: ProtocolSemantics,
    heartbeat_deadline: Option<Instant>,
    draining_deadline: Option<Instant>,
    draining_timeout_ms: Option<u64>,
    idle_read_timeout: Option<Duration>,
    unauthorized_limit: UnauthorizedCommandLimitConfig,
    unauthorized_rejections: u64,
    unauthorized_tokens: f64,
    unauthorized_last_refill: Instant,
    unauthorized_last_logged_attempt: Option<u64>,
    unauthorized_strikes: u32,
    unauthorized_backoff_until: Option<Instant>,
}

impl ConnectionState {
    pub(super) fn new(
        now: Instant,
        active_job_id: forgeclaw_core::JobId,
        negotiated_version: NegotiatedProtocolVersion,
        unauthorized_limit: UnauthorizedCommandLimitConfig,
        idle_read_timeout: Option<Duration>,
    ) -> Self {
        let unauthorized_limit = normalize_unauthorized_limit(unauthorized_limit);
        Self {
            lifecycle: LifecycleState::new(active_job_id),
            semantics: ProtocolSemantics::from_negotiated(negotiated_version),
            heartbeat_deadline: Some(now + DEFAULT_HEARTBEAT_TIMEOUT),
            draining_deadline: None,
            draining_timeout_ms: None,
            idle_read_timeout,
            unauthorized_limit,
            unauthorized_rejections: 0,
            unauthorized_tokens: f64::from(unauthorized_limit.burst_capacity),
            unauthorized_last_refill: now,
            unauthorized_last_logged_attempt: None,
            unauthorized_strikes: 0,
            unauthorized_backoff_until: None,
        }
    }

    pub(super) fn phase_name(&self) -> &'static str {
        self.lifecycle.phase_name()
    }

    pub(super) fn semantics(&self) -> ProtocolSemantics {
        self.semantics
    }
}

fn normalize_unauthorized_limit(
    config: UnauthorizedCommandLimitConfig,
) -> UnauthorizedCommandLimitConfig {
    UnauthorizedCommandLimitConfig {
        burst_capacity: config.burst_capacity.max(1),
        refill_per_second: config.refill_per_second.max(1),
        backoff: config.backoff,
        disconnect_after_strikes: config.disconnect_after_strikes.max(1),
    }
}

pub(super) fn recv_deadline(state: &ConnectionState, now: Instant) -> Option<Instant> {
    match state.lifecycle.phase() {
        ConnectionPhase::Processing => state.heartbeat_deadline,
        ConnectionPhase::DrainingAwaitingCompletion => state.draining_deadline,
        ConnectionPhase::Idle => state.idle_read_timeout.map(|timeout| now + timeout),
        ConnectionPhase::Closed => None,
    }
}

pub(super) fn recv_timeout_error(state: &ConnectionState) -> IpcError {
    match state.lifecycle.phase() {
        ConnectionPhase::Processing => IpcError::Protocol(ProtocolError::HeartbeatTimeout {
            phase: state.phase_name(),
            timeout_secs: DEFAULT_HEARTBEAT_TIMEOUT.as_secs(),
        }),
        ConnectionPhase::DrainingAwaitingCompletion => {
            IpcError::Protocol(ProtocolError::ShutdownDeadlineExceeded {
                phase: state.phase_name(),
                deadline_ms: state.draining_timeout_ms.unwrap_or(0),
            })
        }
        ConnectionPhase::Idle => {
            let timeout = state.idle_read_timeout.map_or(0, |value| value.as_secs());
            IpcError::Protocol(ProtocolError::IdleReadTimeout {
                phase: state.phase_name(),
                timeout_secs: timeout,
            })
        }
        ConnectionPhase::Closed => IpcError::Closed,
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct UnauthorizedLogDecision {
    pub(super) attempt: u64,
    pub(super) should_log: bool,
    pub(super) suppressed_since_last: u64,
    pub(super) action: UnauthorizedEnforcementAction,
    pub(super) strikes: u32,
    pub(super) disconnect_after_strikes: u32,
    pub(super) backoff_ms: u64,
    pub(super) tokens_remaining: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UnauthorizedEnforcementAction {
    Allow,
    Backoff,
    Disconnect,
}

impl UnauthorizedEnforcementAction {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Backoff => "backoff",
            Self::Disconnect => "disconnect",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct UnauthorizedRejectionOutcome {
    pub(super) log: UnauthorizedLogDecision,
    pub(super) action: UnauthorizedEnforcementAction,
    pub(super) backoff: Option<Duration>,
}

pub(super) fn record_unauthorized_rejection(
    state: &mut ConnectionState,
) -> UnauthorizedRejectionOutcome {
    record_unauthorized_rejection_at(state, Instant::now())
}

pub(super) fn record_unauthorized_rejection_at(
    state: &mut ConnectionState,
    now: Instant,
) -> UnauthorizedRejectionOutcome {
    state.unauthorized_rejections = state.unauthorized_rejections.saturating_add(1);
    let attempt = state.unauthorized_rejections;
    refill_unauthorized_tokens(state, now);
    let (action, backoff) = unauthorized_enforcement_action(state, now);
    let should_log = attempt <= UNAUTHORIZED_LOG_BURST || attempt % UNAUTHORIZED_LOG_EVERY == 0;
    let suppressed_since_last = if should_log {
        state
            .unauthorized_last_logged_attempt
            .map_or(0, |last| attempt.saturating_sub(last + 1))
    } else {
        0
    };
    if should_log {
        state.unauthorized_last_logged_attempt = Some(attempt);
    }
    UnauthorizedRejectionOutcome {
        log: UnauthorizedLogDecision {
            attempt,
            should_log,
            suppressed_since_last,
            action,
            strikes: state.unauthorized_strikes,
            disconnect_after_strikes: state.unauthorized_limit.disconnect_after_strikes,
            backoff_ms: backoff.map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
            tokens_remaining: state.unauthorized_tokens,
        },
        action,
        backoff,
    }
}

fn refill_unauthorized_tokens(state: &mut ConnectionState, now: Instant) {
    let elapsed = now
        .checked_duration_since(state.unauthorized_last_refill)
        .unwrap_or_default();
    if elapsed.is_zero() {
        return;
    }
    let refill = elapsed.as_secs_f64() * f64::from(state.unauthorized_limit.refill_per_second);
    let capacity = f64::from(state.unauthorized_limit.burst_capacity);
    state.unauthorized_tokens = (state.unauthorized_tokens + refill).min(capacity);
    state.unauthorized_last_refill = now;
}

fn unauthorized_enforcement_action(
    state: &mut ConnectionState,
    now: Instant,
) -> (UnauthorizedEnforcementAction, Option<Duration>) {
    if let Some(until) = state.unauthorized_backoff_until {
        if now < until {
            let remaining = until - now;
            state.unauthorized_strikes = state.unauthorized_strikes.saturating_add(1);
            if state.unauthorized_strikes >= state.unauthorized_limit.disconnect_after_strikes {
                state.unauthorized_backoff_until = None;
                return (UnauthorizedEnforcementAction::Disconnect, None);
            }
            return (UnauthorizedEnforcementAction::Backoff, Some(remaining));
        }
        state.unauthorized_backoff_until = None;
    }

    if state.unauthorized_tokens >= 1.0 {
        state.unauthorized_tokens -= 1.0;
        state.unauthorized_strikes = 0;
        return (UnauthorizedEnforcementAction::Allow, None);
    }

    state.unauthorized_strikes = state.unauthorized_strikes.saturating_add(1);
    if state.unauthorized_strikes >= state.unauthorized_limit.disconnect_after_strikes {
        return (UnauthorizedEnforcementAction::Disconnect, None);
    }

    let backoff = state.unauthorized_limit.backoff;
    state.unauthorized_backoff_until = Some(now + backoff);
    (UnauthorizedEnforcementAction::Backoff, Some(backoff))
}

pub(super) fn validate_outbound_state(
    state: &ConnectionState,
    msg: &HostToContainer,
) -> Result<(), IpcError> {
    validate_host_to_container(state.semantics, msg)?;
    let mut lifecycle = state.lifecycle.clone();
    let _ = enforce_host_to_container(&mut lifecycle, msg)?;
    Ok(())
}

pub(super) fn enforce_outbound_state(
    state: &mut ConnectionState,
    msg: &HostToContainer,
    now: Instant,
) -> Result<LifecycleAction, IpcError> {
    validate_host_to_container(state.semantics, msg)?;
    let phase_before = state.lifecycle.phase();
    let action = enforce_host_to_container(&mut state.lifecycle, msg)?;
    match msg {
        HostToContainer::Messages(_) => {
            if matches!(phase_before, ConnectionPhase::Idle) {
                // Only (re)arm heartbeat when entering Processing.
                state.heartbeat_deadline = Some(now + DEFAULT_HEARTBEAT_TIMEOUT);
            }
            state.draining_deadline = None;
            state.draining_timeout_ms = None;
        }
        HostToContainer::Shutdown(payload) => {
            state.heartbeat_deadline = None;
            state.draining_deadline = Some(now + Duration::from_millis(payload.deadline_ms));
            state.draining_timeout_ms = Some(payload.deadline_ms);
        }
        HostToContainer::Init(_) => {}
    }
    Ok(action)
}

pub(super) fn enforce_inbound_state(
    state: &mut ConnectionState,
    msg: &ContainerToHost,
    now: Instant,
) -> Result<LifecycleAction, IpcError> {
    validate_container_to_host(state.semantics, msg)?;
    let action = enforce_container_to_host(&mut state.lifecycle, msg)?;
    match msg {
        ContainerToHost::OutputComplete(_) => {
            state.heartbeat_deadline = None;
            state.draining_deadline = None;
            state.draining_timeout_ms = None;
        }
        ContainerToHost::Heartbeat(_)
            if matches!(state.lifecycle.phase(), ConnectionPhase::Processing) =>
        {
            state.heartbeat_deadline = Some(now + DEFAULT_HEARTBEAT_TIMEOUT);
        }
        _ => {}
    }
    Ok(action)
}

#[cfg(test)]
#[path = "protocol_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "protocol_logging_tests.rs"]
mod logging_tests;
