//! Protocol version constants and compatibility rules.
//!
//! The IPC protocol uses a major-minor version scheme of the form `"major.minor"`,
//! carried as a string on the wire in [`crate::message::ReadyPayload::protocol_version`].
//!
//! Version compatibility follows [`docs/IPC_PROTOCOL.md`](../../../docs/IPC_PROTOCOL.md):
//!
//! - Additive minor changes within a major version are always compatible (existing
//!   fields are never removed or retyped).
//! - Major version bumps are breaking — the host rejects adapters whose major
//!   version does not match.
//!
//! Handshake code should derive a [`NegotiatedProtocolVersion`] via
//! [`negotiate`] and persist it in connection state so minor-sensitive
//! behavior can be gated explicitly.

/// The protocol version implemented by this crate.
///
/// This is the value the host declares it implements and the value every
/// container-side adapter built against this crate will advertise in its
/// `Ready` message.
pub const PROTOCOL_VERSION: &str = "1.0";

/// Negotiated protocol version for a live connection.
///
/// Negotiation succeeds only when major versions match. The negotiated
/// minor is the minimum of local and peer minor versions, so behavior
/// gates can safely branch on "supported by both sides."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NegotiatedProtocolVersion {
    major: u32,
    minor: u32,
}

impl NegotiatedProtocolVersion {
    /// Returns the negotiated major version.
    #[must_use]
    pub fn major(self) -> u32 {
        self.major
    }

    /// Returns the negotiated minor version.
    #[must_use]
    pub fn minor(self) -> u32 {
        self.minor
    }

    #[cfg(test)]
    pub(crate) const fn new_for_tests(major: u32, minor: u32) -> Self {
        Self { major, minor }
    }
}

/// Compares a peer's advertised protocol version against [`PROTOCOL_VERSION`].
///
/// Returns `true` iff the peer's major version matches the local major version.
/// Minor-version differences are always accepted (additive-only changes within
/// a major version, per `docs/IPC_PROTOCOL.md`).
///
/// A peer version that cannot be parsed as exactly `"<major>.<minor>"` where
/// both components are non-negative integers is rejected. Extra segments
/// (e.g. `"1.0.3"`), non-numeric components (e.g. `"1.foo"`), and empty
/// components (e.g. `"1."`) are all rejected.
#[must_use]
pub fn is_compatible(peer: &str) -> bool {
    negotiate(peer).is_some()
}

/// Negotiate a runtime protocol version with a peer.
///
/// Returns `None` when either side's version is malformed or majors do
/// not match.
#[must_use]
pub fn negotiate(peer: &str) -> Option<NegotiatedProtocolVersion> {
    let (peer_major, peer_minor) = parse_version(peer)?;
    let (local_major, local_minor) = parse_version(PROTOCOL_VERSION)?;
    if peer_major != local_major {
        return None;
    }
    Some(NegotiatedProtocolVersion {
        major: local_major,
        minor: peer_minor.min(local_minor),
    })
}

/// Parse a `"major.minor"` version string into its two numeric
/// components.
///
/// Returns `None` if either component is non-numeric, empty, or if
/// extra segments (dots) are present.
fn parse_version(version: &str) -> Option<(u32, u32)> {
    parse_version_text(version)
}

/// Shared `"major.minor"` parser used by both the handshake negotiator
/// and the `ProtocolVersionText` wire-type validator. Keeping a single
/// implementation prevents drift between runtime validation and
/// handshake acceptance.
pub(crate) fn parse_version_text(version: &str) -> Option<(u32, u32)> {
    let (major_str, minor_str) = version.split_once('.')?;
    // Reject extra dots (e.g. "1.0.3").
    if minor_str.contains('.') {
        return None;
    }
    let major = major_str.parse::<u32>().ok()?;
    let minor = minor_str.parse::<u32>().ok()?;
    Some((major, minor))
}

#[cfg(test)]
mod tests {
    use super::{PROTOCOL_VERSION, is_compatible, negotiate, parse_version};

    #[test]
    fn local_version_is_one_zero() {
        assert_eq!(PROTOCOL_VERSION, "1.0");
    }

    #[test]
    fn parse_version_valid() {
        assert_eq!(parse_version("1.0"), Some((1, 0)));
        assert_eq!(parse_version("2.17"), Some((2, 17)));
        assert_eq!(parse_version("0.9"), Some((0, 9)));
    }

    #[test]
    fn parse_version_rejects_extra_segments() {
        assert_eq!(parse_version("1.0.3"), None);
        assert_eq!(parse_version("42.0.3"), None);
        assert_eq!(parse_version("1.0.0"), None);
    }

    #[test]
    fn parse_version_rejects_empty_components() {
        assert_eq!(parse_version("1."), None);
        assert_eq!(parse_version(".1"), None);
        assert_eq!(parse_version("."), None);
    }

    #[test]
    fn parse_version_rejects_non_numeric() {
        assert_eq!(parse_version("one.zero"), None);
        assert_eq!(parse_version("v1.0"), None);
        assert_eq!(parse_version("1.foo"), None);
    }

    #[test]
    fn parse_version_rejects_missing_dot() {
        assert_eq!(parse_version("1"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn exact_match_is_compatible() {
        assert!(is_compatible("1.0"));
    }

    #[test]
    fn same_major_different_minor_is_compatible() {
        assert!(is_compatible("1.0"));
        assert!(is_compatible("1.5"));
        assert!(is_compatible("1.99"));
    }

    #[test]
    fn different_major_is_incompatible() {
        assert!(!is_compatible("2.0"));
        assert!(!is_compatible("0.9"));
    }

    #[test]
    fn malformed_versions_are_incompatible() {
        assert!(!is_compatible("abc"));
        assert!(!is_compatible(""));
        assert!(!is_compatible("1"));
        assert!(!is_compatible("1."));
        assert!(!is_compatible("1.foo"));
        assert!(!is_compatible("1.0.3"));
        assert!(!is_compatible(".1"));
    }

    #[test]
    fn negotiate_uses_shared_major_and_lower_minor() {
        let v = negotiate("1.0").expect("compatible");
        assert_eq!(v.major(), 1);
        assert_eq!(v.minor(), 0);

        let v = negotiate("1.9").expect("compatible");
        assert_eq!(v.major(), 1);
        assert_eq!(v.minor(), 0);
    }

    #[test]
    fn negotiate_rejects_malformed_or_major_mismatch() {
        assert!(negotiate("2.0").is_none());
        assert!(negotiate("invalid").is_none());
    }
}
