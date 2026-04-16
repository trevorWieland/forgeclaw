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
use crate::util::sampler::SampledCounter;
use crate::version::NegotiatedProtocolVersion;

use super::listener::UnauthorizedCommandLimitConfig;

pub(super) const UNAUTHORIZED_LOG_BURST: u64 = 5;
pub(super) const UNAUTHORIZED_LOG_EVERY: u64 = 10;

#[path = "protocol_logging.rs"]
mod logging;

pub(super) use logging::{
    log_fatal_protocol_error, log_outbound_validation_rejection, log_unauthorized_command,
    log_unknown_message,
};

/// Per-connection runtime protocol state.
///
/// # Concurrency invariants
///
/// All fields are guarded by a single [`AsyncMutex`](tokio::sync::Mutex)
/// held by the owning [`crate::server::IpcConnection`] (and its split
/// reader/writer halves). Consolidating them behind one lock keeps the
/// following invariants atomic:
///
/// 1. **Linear phase progression.** `lifecycle.phase()` only advances
///    forward (Idle → Processing → Draining → Closed). No path
///    observes an intermediate phase without the mutex because every
///    transition happens inside a critical section that also mutates
///    the deadline/timeout fields bound to that phase.
/// 2. **Heartbeat / draining deadlines are phase-consistent.**
///    `heartbeat_deadline` is only valid while `phase == Processing`,
///    `draining_deadline` only while `phase == DrainingAwaitingCompletion`.
///    Updates to the deadline and the phase happen under the same
///    lock acquisition so readers never see a stale deadline from a
///    previous phase.
/// 3. **Unauthorized budget ⇔ strike counter ⇔ backoff window are
///    co-updated.** Token refill, decrement, strike increment, and
///    backoff-until stamps are all touched by
///    [`record_unauthorized_rejection_at`] inside the same critical
///    section so enforcement decisions never race against refill.
/// 4. **Sampler monotonicity.** `unauthorized_log_sampler` is only
///    advanced by `record_unauthorized_rejection_at`, which holds the
///    mutex. The sampler's `attempt` counter is therefore monotonic
///    and aligned with the actual rejection count.
/// 5. **Poisoning happens outside this struct.** The `poisoned`
///    [`AtomicBool`](std::sync::atomic::AtomicBool) on
///    [`crate::server::IpcConnection`] is intentionally NOT stored
///    here; readers may observe `poisoned = true` without holding the
///    mutex so the write-side teardown path short-circuits instantly.
///    Callers must treat a `true` poisoned flag as authoritative
///    regardless of what this struct's phase says.
///
/// Any future refactor that splits this state across multiple locks
/// MUST preserve invariants 1–4 (the poisoned flag is already split).
/// A lock-free phase atomic may be layered on top, but it must
/// update *after* the mutex-protected transition so reads lagging by
/// one transition cannot see a phase ahead of the mutex view.
#[derive(Debug, Clone)]
pub(super) struct ConnectionState {
    lifecycle: LifecycleState,
    semantics: ProtocolSemantics,
    heartbeat_deadline: Option<Instant>,
    draining_deadline: Option<Instant>,
    draining_timeout_ms: Option<u64>,
    idle_read_timeout: Option<Duration>,
    unauthorized_limit: UnauthorizedCommandLimitConfig,
    unauthorized_tokens: f64,
    unauthorized_last_refill: Instant,
    unauthorized_log_sampler: SampledCounter,
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
            unauthorized_tokens: f64::from(unauthorized_limit.burst_capacity),
            unauthorized_last_refill: now,
            unauthorized_log_sampler: SampledCounter::new(
                UNAUTHORIZED_LOG_BURST,
                UNAUTHORIZED_LOG_EVERY,
            ),
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
    let sample = state.unauthorized_log_sampler.observe();
    refill_unauthorized_tokens(state, now);
    let (action, backoff) = unauthorized_enforcement_action(state, now);
    UnauthorizedRejectionOutcome {
        log: UnauthorizedLogDecision {
            attempt: sample.attempt,
            should_log: sample.should_log,
            suppressed_since_last: sample.suppressed_since_last,
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
