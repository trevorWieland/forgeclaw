//! Per-connection sampled-logging primitive.
//!
//! [`SampledCounter`] encapsulates the burst-then-every-N sampling
//! rule used across the IPC crate for high-cardinality log events
//! (unauthorized commands, unknown message types) without losing audit
//! signal. The underlying counter is **authoritative for rate-limit
//! accounting** even when the sampler decides to suppress the warn-
//! level line — callers that enforce budgets must never read
//! [`SampleDecision::should_log`] as if it were the occurrence count.

/// Sampling decision returned by [`SampledCounter::observe`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SampleDecision {
    /// Monotonic attempt number (starting at 1 for the first observed
    /// event).
    pub(crate) attempt: u64,
    /// Whether the caller should emit the sampled (higher-severity)
    /// log line. Debug/audit lines should always fire.
    pub(crate) should_log: bool,
    /// Attempts suppressed since the last emitted sampled log line.
    /// Zero on the emitted attempt itself.
    pub(crate) suppressed_since_last: u64,
}

/// Sampler that emits the first `burst` occurrences unconditionally
/// and then every `every`-th occurrence afterwards.
///
/// Constructing with `burst = 0` or `every = 0` is treated as
/// "observe but always suppress", which is useful in tests; production
/// call sites should pick non-zero values so at least the first
/// occurrence is visible.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SampledCounter {
    burst: u64,
    every: u64,
    attempts: u64,
    last_logged_attempt: Option<u64>,
}

impl SampledCounter {
    /// Construct a new sampler with the given burst and periodic
    /// thresholds. Both values are normalized to `1` or larger at
    /// call sites that expect at least one log line per occurrence
    /// class.
    pub(crate) const fn new(burst: u64, every: u64) -> Self {
        Self {
            burst,
            every,
            attempts: 0,
            last_logged_attempt: None,
        }
    }

    /// Record one occurrence and return whether the caller should
    /// emit the sampled log line.
    pub(crate) fn observe(&mut self) -> SampleDecision {
        self.attempts = self.attempts.saturating_add(1);
        let attempt = self.attempts;
        let should_log = attempt <= self.burst || (self.every > 0 && attempt % self.every == 0);
        let suppressed_since_last = if should_log {
            self.last_logged_attempt
                .map_or(0, |last| attempt.saturating_sub(last + 1))
        } else {
            0
        };
        if should_log {
            self.last_logged_attempt = Some(attempt);
        }
        SampleDecision {
            attempt,
            should_log,
            suppressed_since_last,
        }
    }

    /// Current monotonic attempt count (authoritative for budget
    /// accounting).
    #[cfg(test)]
    pub(crate) fn attempts(&self) -> u64 {
        self.attempts
    }
}

#[cfg(test)]
mod tests {
    use super::SampledCounter;

    #[test]
    fn first_burst_always_logs() {
        let mut s = SampledCounter::new(5, 10);
        for expected_attempt in 1..=5 {
            let d = s.observe();
            assert!(d.should_log, "attempt {expected_attempt} must log in burst");
            assert_eq!(d.attempt, expected_attempt);
            assert_eq!(d.suppressed_since_last, 0);
        }
    }

    #[test]
    fn after_burst_emits_every_n() {
        let mut s = SampledCounter::new(5, 10);
        for _ in 0..5 {
            let d = s.observe();
            assert!(d.should_log);
        }
        for attempt in 6..=9 {
            let d = s.observe();
            assert!(!d.should_log, "attempt {attempt} must be suppressed");
        }
        let d = s.observe(); // attempt 10
        assert!(d.should_log, "attempt 10 must log (every=10)");
        assert_eq!(d.attempt, 10);
        assert_eq!(d.suppressed_since_last, 4);
    }

    #[test]
    fn suppressed_since_last_accumulates_between_logs() {
        let mut s = SampledCounter::new(1, 10);
        let first = s.observe();
        assert!(first.should_log);
        // attempts 2..=9 suppressed
        for _ in 2..=9 {
            let d = s.observe();
            assert!(!d.should_log);
        }
        let tenth = s.observe();
        assert!(tenth.should_log);
        assert_eq!(tenth.suppressed_since_last, 8);
    }

    #[test]
    fn monotonic_counter_survives_suppression() {
        let mut s = SampledCounter::new(2, 100);
        for i in 1..=50 {
            let d = s.observe();
            assert_eq!(d.attempt, i);
        }
        assert_eq!(s.attempts(), 50);
    }

    #[test]
    fn zero_every_only_logs_within_burst() {
        let mut s = SampledCounter::new(3, 0);
        assert!(s.observe().should_log);
        assert!(s.observe().should_log);
        assert!(s.observe().should_log);
        for _ in 0..20 {
            assert!(!s.observe().should_log);
        }
    }
}
