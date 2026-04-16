//! Shared protocol policy primitives.

use std::time::Duration;

use crate::codec::DEFAULT_MAX_UNKNOWN_SKIPS;
use crate::error::{IpcError, ProtocolError};
use crate::forward_compat::{
    ForwardCompatPolicy, IgnoredFieldTally, enforce as enforce_forward_compat,
    promote_for_container, promote_for_host,
};
use crate::message::{ContainerToHost, HostToContainer};
use crate::semantics::ProtocolSemantics;
use tokio::time::Instant;

/// Cumulative byte budget for consecutive unknown-type frames.
pub(crate) const DEFAULT_MAX_UNKNOWN_BYTES: usize = 1024 * 1024;
/// Maximum unknown frames over an entire connection lifetime.
pub(crate) const DEFAULT_MAX_UNKNOWN_TOTAL_MESSAGES: usize = 256;
/// Maximum unknown-message bytes over an entire connection lifetime.
pub(crate) const DEFAULT_MAX_UNKNOWN_TOTAL_BYTES: usize = 4 * 1024 * 1024;
/// Unknown-message token-bucket burst capacity.
pub(crate) const DEFAULT_UNKNOWN_RATE_BURST_CAPACITY: u32 = 64;
/// Unknown-message token-bucket refill rate per second.
pub(crate) const DEFAULT_UNKNOWN_RATE_REFILL_PER_SECOND: u32 = 16;

/// Default per-connection lifetime cap on ignored-field leaf count
/// across **all** known-type messages.
pub(crate) const DEFAULT_IGNORED_FIELD_LIFETIME_KEYS: u32 = 1024;
/// Default per-connection lifetime cap on ignored-field byte volume
/// across **all** known-type messages.
pub(crate) const DEFAULT_IGNORED_FIELD_LIFETIME_BYTES: u32 = 1024 * 1024;

/// Processing-phase heartbeat timeout per protocol spec.
pub(crate) const DEFAULT_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(60);

/// Sampler burst count for unknown-message warnings: the first N
/// unknown messages on any connection always emit a sampled warn line
/// so operators see the initial incursion unambiguously.
pub(crate) const UNKNOWN_LOG_BURST: u64 = 5;
/// Sampler periodic interval for unknown-message warnings after the
/// burst is exhausted: every Nth unknown message emits a sampled warn
/// line. Debug-level audit logging is unaffected.
pub(crate) const UNKNOWN_LOG_EVERY: u64 = 10;

/// Per-connection unknown-frame accounting.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct UnknownFrameBudget {
    count: usize,
    bytes: usize,
}

impl UnknownFrameBudget {
    /// Reset consecutive unknown-message counters.
    pub(crate) fn reset(&mut self) {
        self.count = 0;
        self.bytes = 0;
    }

    /// Record one unknown frame of `frame_len` bytes.
    pub(crate) fn on_unknown(&mut self, frame_len: usize) -> Result<(), IpcError> {
        self.count += 1;
        self.bytes = self.bytes.saturating_add(frame_len);
        if self.count > DEFAULT_MAX_UNKNOWN_SKIPS {
            return Err(IpcError::Protocol(ProtocolError::TooManyUnknownMessages {
                count: self.count,
                limit: DEFAULT_MAX_UNKNOWN_SKIPS,
            }));
        }
        if self.bytes > DEFAULT_MAX_UNKNOWN_BYTES {
            return Err(IpcError::Protocol(ProtocolError::TooManyUnknownBytes {
                bytes: self.bytes,
                limit: DEFAULT_MAX_UNKNOWN_BYTES,
            }));
        }
        Ok(())
    }

    /// Number of consecutive unknown messages in the current streak.
    pub(crate) fn count(&self) -> usize {
        self.count
    }

    /// Cumulative bytes across the current unknown streak.
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }
}

/// Independent unknown-traffic controls applied per connection.
///
/// Set any field to `0` to disable that control explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownTrafficLimitConfig {
    /// Max unknown frames over connection lifetime (`0` disables).
    pub lifetime_message_limit: usize,
    /// Max unknown bytes over connection lifetime (`0` disables).
    pub lifetime_byte_limit: usize,
    /// Token-bucket burst capacity for unknown frame rate (`0` disables).
    pub rate_limit_burst_capacity: u32,
    /// Token refill rate (per second) for unknown frame rate (`0` disables).
    pub rate_limit_refill_per_second: u32,
    /// Max ignored-field leaves across **all** known-type messages in
    /// the connection's lifetime (`0` disables).
    pub ignored_field_lifetime_keys: u32,
    /// Max ignored-field bytes across **all** known-type messages in
    /// the connection's lifetime (`0` disables).
    pub ignored_field_lifetime_bytes: u32,
}

impl Default for UnknownTrafficLimitConfig {
    fn default() -> Self {
        Self {
            lifetime_message_limit: DEFAULT_MAX_UNKNOWN_TOTAL_MESSAGES,
            lifetime_byte_limit: DEFAULT_MAX_UNKNOWN_TOTAL_BYTES,
            rate_limit_burst_capacity: DEFAULT_UNKNOWN_RATE_BURST_CAPACITY,
            rate_limit_refill_per_second: DEFAULT_UNKNOWN_RATE_REFILL_PER_SECOND,
            ignored_field_lifetime_keys: DEFAULT_IGNORED_FIELD_LIFETIME_KEYS,
            ignored_field_lifetime_bytes: DEFAULT_IGNORED_FIELD_LIFETIME_BYTES,
        }
    }
}

impl UnknownTrafficLimitConfig {
    #[must_use]
    pub(crate) fn normalized(self) -> Self {
        let mut normalized = self;
        if normalized.rate_limit_burst_capacity == 0 || normalized.rate_limit_refill_per_second == 0
        {
            normalized.rate_limit_burst_capacity = 0;
            normalized.rate_limit_refill_per_second = 0;
        }
        normalized
    }

    #[must_use]
    pub(crate) fn rate_limiter_enabled(self) -> bool {
        self.rate_limit_burst_capacity > 0 && self.rate_limit_refill_per_second > 0
    }
}

/// Server-side unknown-frame tracker with both consecutive and
/// independent lifetime/rate protections. Also accumulates lifetime
/// ignored-field totals across all known-type messages.
#[derive(Debug, Clone, Copy)]
pub(crate) struct UnknownTrafficBudget {
    consecutive: UnknownFrameBudget,
    total_count: usize,
    total_bytes: usize,
    ignored_field_total_keys: u32,
    ignored_field_total_bytes: u32,
    limits: UnknownTrafficLimitConfig,
    rate_tokens: f64,
    rate_last_refill: Instant,
}

impl UnknownTrafficBudget {
    #[must_use]
    pub(crate) fn new(now: Instant, limits: UnknownTrafficLimitConfig) -> Self {
        let limits = limits.normalized();
        Self {
            consecutive: UnknownFrameBudget::default(),
            total_count: 0,
            total_bytes: 0,
            ignored_field_total_keys: 0,
            ignored_field_total_bytes: 0,
            rate_tokens: f64::from(limits.rate_limit_burst_capacity),
            rate_last_refill: now,
            limits,
        }
    }

    pub(crate) fn reset_consecutive(&mut self) {
        self.consecutive.reset();
    }

    pub(crate) fn on_unknown(&mut self, frame_len: usize, now: Instant) -> Result<(), IpcError> {
        self.consecutive.on_unknown(frame_len)?;
        self.total_count = self.total_count.saturating_add(1);
        self.total_bytes = self.total_bytes.saturating_add(frame_len);

        if self.limits.lifetime_message_limit > 0
            && self.total_count > self.limits.lifetime_message_limit
        {
            return Err(IpcError::Protocol(
                ProtocolError::TooManyUnknownMessagesTotal {
                    count: self.total_count,
                    limit: self.limits.lifetime_message_limit,
                },
            ));
        }
        if self.limits.lifetime_byte_limit > 0 && self.total_bytes > self.limits.lifetime_byte_limit
        {
            return Err(IpcError::Protocol(
                ProtocolError::TooManyUnknownBytesTotal {
                    bytes: self.total_bytes,
                    limit: self.limits.lifetime_byte_limit,
                },
            ));
        }

        self.consume_rate_token(now)
    }

    fn consume_rate_token(&mut self, now: Instant) -> Result<(), IpcError> {
        if !self.limits.rate_limiter_enabled() {
            return Ok(());
        }
        let elapsed = now
            .checked_duration_since(self.rate_last_refill)
            .unwrap_or_default();
        if !elapsed.is_zero() {
            let refill =
                elapsed.as_secs_f64() * f64::from(self.limits.rate_limit_refill_per_second);
            let capacity = f64::from(self.limits.rate_limit_burst_capacity);
            self.rate_tokens = (self.rate_tokens + refill).min(capacity);
            self.rate_last_refill = now;
        }

        if self.rate_tokens >= 1.0 {
            self.rate_tokens -= 1.0;
            return Ok(());
        }
        Err(IpcError::Protocol(
            ProtocolError::UnknownMessageRateLimitExceeded {
                burst_capacity: self.limits.rate_limit_burst_capacity,
                refill_per_second: self.limits.rate_limit_refill_per_second,
            },
        ))
    }

    #[must_use]
    pub(crate) fn count(&self) -> usize {
        self.consecutive.count()
    }

    #[must_use]
    pub(crate) fn bytes(&self) -> usize {
        self.consecutive.bytes()
    }

    #[must_use]
    pub(crate) fn total_count(&self) -> usize {
        self.total_count
    }

    #[must_use]
    pub(crate) fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    #[must_use]
    pub(crate) fn limits(&self) -> UnknownTrafficLimitConfig {
        self.limits
    }

    /// Apply per-frame forward-compat policy plus lifetime
    /// ignored-field budget for an inbound container→host message.
    pub(crate) fn on_ignored_container(
        &mut self,
        msg: &ContainerToHost,
        tally: IgnoredFieldTally,
        semantics: ProtocolSemantics,
    ) -> Result<(), IpcError> {
        let policy = promote_for_container(msg.forward_compat_base(), msg, semantics);
        self.apply_tally(policy, tally, msg.type_name())
    }

    /// Apply per-frame forward-compat policy plus lifetime
    /// ignored-field budget for an inbound host→container message.
    pub(crate) fn on_ignored_host(
        &mut self,
        msg: &HostToContainer,
        tally: IgnoredFieldTally,
        semantics: ProtocolSemantics,
    ) -> Result<(), IpcError> {
        let policy = promote_for_host(msg.forward_compat_base(), msg, semantics);
        self.apply_tally(policy, tally, msg.type_name())
    }

    fn apply_tally(
        &mut self,
        policy: ForwardCompatPolicy,
        tally: IgnoredFieldTally,
        message_type: &'static str,
    ) -> Result<(), IpcError> {
        enforce_forward_compat(policy, tally, message_type)?;
        if tally.is_empty() {
            return Ok(());
        }
        self.ignored_field_total_keys = self.ignored_field_total_keys.saturating_add(tally.keys);
        self.ignored_field_total_bytes = self.ignored_field_total_bytes.saturating_add(tally.bytes);
        if self.limits.ignored_field_lifetime_keys > 0
            && self.ignored_field_total_keys > self.limits.ignored_field_lifetime_keys
        {
            return Err(IpcError::Protocol(
                ProtocolError::IgnoredFieldBudgetExceeded {
                    message_type,
                    scope: "lifetime",
                    dimension: "keys",
                    observed: self.ignored_field_total_keys,
                    limit: self.limits.ignored_field_lifetime_keys,
                },
            ));
        }
        if self.limits.ignored_field_lifetime_bytes > 0
            && self.ignored_field_total_bytes > self.limits.ignored_field_lifetime_bytes
        {
            return Err(IpcError::Protocol(
                ProtocolError::IgnoredFieldBudgetExceeded {
                    message_type,
                    scope: "lifetime",
                    dimension: "bytes",
                    observed: self.ignored_field_total_bytes,
                    limit: self.limits.ignored_field_lifetime_bytes,
                },
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{UnknownTrafficBudget, UnknownTrafficLimitConfig};

    #[test]
    fn unknown_budget_lifetime_count_enforced_across_resets() {
        let now = tokio::time::Instant::now();
        let mut budget = UnknownTrafficBudget::new(
            now,
            UnknownTrafficLimitConfig {
                lifetime_message_limit: 3,
                lifetime_byte_limit: 0,
                rate_limit_burst_capacity: 0,
                rate_limit_refill_per_second: 0,
                ignored_field_lifetime_keys: 0,
                ignored_field_lifetime_bytes: 0,
            },
        );
        budget.on_unknown(10, now).expect("first unknown");
        budget.reset_consecutive();
        budget.on_unknown(10, now).expect("second unknown");
        budget.reset_consecutive();
        budget.on_unknown(10, now).expect("third unknown");
        budget.reset_consecutive();
        let err = budget
            .on_unknown(10, now)
            .expect_err("lifetime cap should be enforced");
        assert!(matches!(
            err,
            crate::error::IpcError::Protocol(
                crate::error::ProtocolError::TooManyUnknownMessagesTotal { .. }
            )
        ));
    }

    #[test]
    fn unknown_budget_lifetime_bytes_enforced_across_resets() {
        let now = tokio::time::Instant::now();
        let mut budget = UnknownTrafficBudget::new(
            now,
            UnknownTrafficLimitConfig {
                lifetime_message_limit: 0,
                lifetime_byte_limit: 100,
                rate_limit_burst_capacity: 0,
                rate_limit_refill_per_second: 0,
                ignored_field_lifetime_keys: 0,
                ignored_field_lifetime_bytes: 0,
            },
        );
        budget.on_unknown(60, now).expect("first unknown");
        budget.reset_consecutive();
        let err = budget
            .on_unknown(60, now)
            .expect_err("lifetime byte cap should be enforced");
        assert!(matches!(
            err,
            crate::error::IpcError::Protocol(
                crate::error::ProtocolError::TooManyUnknownBytesTotal { .. }
            )
        ));
    }

    #[test]
    fn unknown_budget_rate_limiter_enforced() {
        let now = tokio::time::Instant::now();
        let mut budget = UnknownTrafficBudget::new(
            now,
            UnknownTrafficLimitConfig {
                lifetime_message_limit: 0,
                lifetime_byte_limit: 0,
                rate_limit_burst_capacity: 2,
                rate_limit_refill_per_second: 1,
                ignored_field_lifetime_keys: 0,
                ignored_field_lifetime_bytes: 0,
            },
        );
        budget.on_unknown(1, now).expect("token 1");
        budget.on_unknown(1, now).expect("token 2");
        let err = budget
            .on_unknown(1, now)
            .expect_err("third unknown should hit rate limit");
        assert!(matches!(
            err,
            crate::error::IpcError::Protocol(
                crate::error::ProtocolError::UnknownMessageRateLimitExceeded { .. }
            )
        ));
    }
}
