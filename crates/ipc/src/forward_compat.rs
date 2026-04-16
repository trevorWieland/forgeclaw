//! Per-message-type forward-compatibility policy.
//!
//! Peers on a newer minor version may legitimately include fields that
//! older decoders do not recognize. Baseline protocol 1.0 accepts those
//! additive fields for forward compatibility, but silent acceptance is a
//! traffic-shaping gap: a malicious peer could attach an arbitrarily
//! large JSON blob to a `heartbeat` and bypass the unknown-**type**
//! abuse controls (`crate::policy`). This module budgets **ignored
//! fields** per message type.
//!
//! Policy lives at the payload struct via the [`KnownMessage`] trait so
//! that adding a new payload without picking a [`ForwardCompatPolicy`]
//! is a compile error. The top-level enums ([`crate::ContainerToHost`]
//! and [`crate::HostToContainer`]) dispatch through a `match` that
//! mirrors their `type_name()` match — a new variant is therefore a
//! compile error in **both** methods simultaneously.

use crate::error::{IpcError, ProtocolError};
use crate::message::{ContainerToHost, HostToContainer};
use crate::semantics::ProtocolSemantics;

/// Wire-side accounting of ignored fields observed during a single
/// inbound decode.
///
/// `keys` counts distinct ignored leaves reported by `serde_ignored`.
/// `bytes` is the sum of the JSON-text byte lengths of the ignored
/// subtrees as they arrived on the wire.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IgnoredFieldTally {
    /// Number of ignored field leaves observed in this frame.
    pub keys: u32,
    /// Cumulative JSON-text byte length of ignored subtrees in this
    /// frame.
    pub bytes: u32,
}

impl IgnoredFieldTally {
    /// Returns `true` if this tally recorded no ignored fields.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.keys == 0 && self.bytes == 0
    }

    /// Accumulate one more ignored leaf with `subtree_bytes` bytes of
    /// JSON text. Saturates at `u32::MAX`.
    pub(crate) fn push(&mut self, subtree_bytes: usize) {
        self.keys = self.keys.saturating_add(1);
        let clamped = u32::try_from(subtree_bytes).unwrap_or(u32::MAX);
        self.bytes = self.bytes.saturating_add(clamped);
    }
}

/// Per-message-type posture toward unknown fields on the wire.
///
/// The baseline policy for each payload struct is declared at the
/// payload via [`KnownMessage::FORWARD_COMPAT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForwardCompatPolicy {
    /// The message type is never permitted to carry unknown fields —
    /// equivalent to `#[serde(deny_unknown_fields)]` at the runtime
    /// layer. Used for high-frequency / narrow-shape types like
    /// `heartbeat` and `output_delta` where forward-compat additions
    /// should instead go on a sibling message type.
    Reject,
    /// The message type may carry unknown fields up to per-frame caps.
    /// Lifetime caps are tracked separately by the per-connection
    /// unknown-traffic budget (see `crate::policy`).
    Budget {
        /// Maximum ignored-field leaf count per frame.
        max_keys_per_frame: u32,
        /// Maximum ignored-field byte count per frame.
        max_bytes_per_frame: u32,
    },
}

impl ForwardCompatPolicy {
    /// Default budget applied to low-frequency / broad-shape payloads.
    /// Picked so that a single extra additive field (typical forward
    /// compat) always fits, but a 4 KiB+ junk blob does not.
    pub const DEFAULT_BUDGET: Self = Self::Budget {
        max_keys_per_frame: 16,
        max_bytes_per_frame: 4096,
    };
}

/// Trait implemented by every payload struct the IPC protocol knows.
///
/// `FORWARD_COMPAT` is `const` so the policy decision lives in the
/// payload's source file and is impossible to omit when adding a new
/// payload type — adding a payload without an `impl KnownMessage` line
/// fails to compile at the enum dispatch sites below.
pub trait KnownMessage {
    /// Forward-compatibility policy for this payload in baseline
    /// (`V1_0`) semantics. `V1_1Plus` may promote the policy to
    /// [`ForwardCompatPolicy::Reject`] for selected types via
    /// [`promote_for_container`] / [`promote_for_host`].
    const FORWARD_COMPAT: ForwardCompatPolicy;
}

/// Promotion rule — applies stricter semantics when the negotiated
/// minor is ≥ 1.1 for container→host messages.
///
/// High-frequency types (`heartbeat`, `output_delta`) are promoted to
/// `Reject` at 1.1+. These shapes are never expected to carry
/// additive fields; any extra field is, per the 1.1 gate, a protocol
/// violation.
#[must_use]
pub fn promote_for_container(
    base: ForwardCompatPolicy,
    msg: &ContainerToHost,
    sem: ProtocolSemantics,
) -> ForwardCompatPolicy {
    match sem {
        ProtocolSemantics::V1_0 => base,
        ProtocolSemantics::V1_1Plus => match msg {
            ContainerToHost::Heartbeat(_) | ContainerToHost::OutputDelta(_) => {
                ForwardCompatPolicy::Reject
            }
            _ => base,
        },
    }
}

/// Promotion rule for host→container messages. No promotions are
/// currently defined; kept symmetrical with [`promote_for_container`]
/// so future minor bumps plug in uniformly.
#[must_use]
pub fn promote_for_host(
    base: ForwardCompatPolicy,
    _msg: &HostToContainer,
    _sem: ProtocolSemantics,
) -> ForwardCompatPolicy {
    base
}

/// Enforce a resolved [`ForwardCompatPolicy`] against a single-frame
/// [`IgnoredFieldTally`]. Lifetime caps are enforced separately by the
/// caller via [`crate::policy::UnknownTrafficBudget::on_ignored_fields`].
pub(crate) fn enforce(
    policy: ForwardCompatPolicy,
    tally: IgnoredFieldTally,
    message_type: &'static str,
) -> Result<(), IpcError> {
    if tally.is_empty() {
        return Ok(());
    }
    match policy {
        ForwardCompatPolicy::Reject => {
            Err(IpcError::Protocol(ProtocolError::UnknownFieldsRejected {
                message_type,
                key_count: tally.keys,
            }))
        }
        ForwardCompatPolicy::Budget {
            max_keys_per_frame,
            max_bytes_per_frame,
        } => {
            if max_keys_per_frame > 0 && tally.keys > max_keys_per_frame {
                return Err(IpcError::Protocol(
                    ProtocolError::IgnoredFieldBudgetExceeded {
                        message_type,
                        scope: "per_frame",
                        dimension: "keys",
                        observed: tally.keys,
                        limit: max_keys_per_frame,
                    },
                ));
            }
            if max_bytes_per_frame > 0 && tally.bytes > max_bytes_per_frame {
                return Err(IpcError::Protocol(
                    ProtocolError::IgnoredFieldBudgetExceeded {
                        message_type,
                        scope: "per_frame",
                        dimension: "bytes",
                        observed: tally.bytes,
                        limit: max_bytes_per_frame,
                    },
                ));
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ForwardCompatPolicy, IgnoredFieldTally, enforce, promote_for_container, promote_for_host,
    };
    use crate::message::shared::ShutdownReason;
    use crate::message::{ContainerToHost, HeartbeatPayload, HostToContainer, ShutdownPayload};
    use crate::semantics::ProtocolSemantics;

    fn sample_heartbeat() -> ContainerToHost {
        ContainerToHost::Heartbeat(HeartbeatPayload {
            timestamp: "2026-04-16T00:00:00Z".parse().expect("valid timestamp"),
        })
    }

    fn sample_shutdown() -> HostToContainer {
        HostToContainer::Shutdown(ShutdownPayload {
            reason: ShutdownReason::IdleTimeout,
            deadline_ms: 1,
        })
    }

    #[test]
    fn empty_tally_is_always_ok() {
        enforce(
            ForwardCompatPolicy::Reject,
            IgnoredFieldTally::default(),
            "heartbeat",
        )
        .expect("empty tally under Reject");
        enforce(
            ForwardCompatPolicy::DEFAULT_BUDGET,
            IgnoredFieldTally::default(),
            "ready",
        )
        .expect("empty tally under Budget");
    }

    #[test]
    fn reject_rejects_any_ignored_field() {
        let err = enforce(
            ForwardCompatPolicy::Reject,
            IgnoredFieldTally { keys: 1, bytes: 4 },
            "heartbeat",
        )
        .expect_err("Reject must fail on any ignored field");
        assert!(matches!(
            err,
            crate::error::IpcError::Protocol(
                crate::error::ProtocolError::UnknownFieldsRejected { .. }
            )
        ));
    }

    #[test]
    fn budget_enforces_per_frame_keys() {
        let policy = ForwardCompatPolicy::Budget {
            max_keys_per_frame: 2,
            max_bytes_per_frame: 1024,
        };
        enforce(policy, IgnoredFieldTally { keys: 2, bytes: 8 }, "ready")
            .expect("exactly at limit passes");
        let err = enforce(policy, IgnoredFieldTally { keys: 3, bytes: 8 }, "ready")
            .expect_err("over-limit keys rejected");
        let crate::error::IpcError::Protocol(
            crate::error::ProtocolError::IgnoredFieldBudgetExceeded { dimension, .. },
        ) = err
        else {
            unreachable!("expected IgnoredFieldBudgetExceeded, got {err:?}")
        };
        assert_eq!(dimension, "keys");
    }

    #[test]
    fn budget_enforces_per_frame_bytes() {
        let policy = ForwardCompatPolicy::Budget {
            max_keys_per_frame: 8,
            max_bytes_per_frame: 16,
        };
        let err = enforce(policy, IgnoredFieldTally { keys: 1, bytes: 17 }, "ready")
            .expect_err("over-limit bytes rejected");
        let crate::error::IpcError::Protocol(
            crate::error::ProtocolError::IgnoredFieldBudgetExceeded { dimension, .. },
        ) = err
        else {
            unreachable!("expected IgnoredFieldBudgetExceeded, got {err:?}")
        };
        assert_eq!(dimension, "bytes");
    }

    #[test]
    fn v1_0_does_not_promote_container_messages() {
        let base = ForwardCompatPolicy::DEFAULT_BUDGET;
        let msg = sample_heartbeat();
        assert_eq!(
            promote_for_container(base, &msg, ProtocolSemantics::V1_0),
            base
        );
    }

    #[test]
    fn v1_1plus_promotes_heartbeat_to_reject() {
        let base = ForwardCompatPolicy::DEFAULT_BUDGET;
        let msg = sample_heartbeat();
        assert_eq!(
            promote_for_container(base, &msg, ProtocolSemantics::V1_1Plus),
            ForwardCompatPolicy::Reject
        );
    }

    #[test]
    fn host_side_has_no_promotions_today() {
        let base = ForwardCompatPolicy::DEFAULT_BUDGET;
        let msg = sample_shutdown();
        assert_eq!(
            promote_for_host(base, &msg, ProtocolSemantics::V1_1Plus),
            base
        );
    }

    #[test]
    fn tally_push_saturates() {
        let mut t = IgnoredFieldTally::default();
        t.push(usize::MAX);
        assert_eq!(t.bytes, u32::MAX);
        assert_eq!(t.keys, 1);
    }
}
