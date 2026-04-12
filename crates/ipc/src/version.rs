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
//! Callers check peer version via [`is_compatible`] inside the handshake path.

/// The protocol version implemented by this crate.
///
/// This is the value the host declares it implements and the value every
/// container-side adapter built against this crate will advertise in its
/// `Ready` message.
pub const PROTOCOL_VERSION: &str = "1.0";

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
    let Some((peer_major, _)) = parse_version(peer) else {
        return false;
    };
    let Some((local_major, _)) = parse_version(PROTOCOL_VERSION) else {
        return false;
    };
    peer_major == local_major
}

/// Parse a `"major.minor"` version string into its two numeric
/// components.
///
/// Returns `None` if either component is non-numeric, empty, or if
/// extra segments (dots) are present.
fn parse_version(version: &str) -> Option<(u32, u32)> {
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
    use super::{PROTOCOL_VERSION, is_compatible, parse_version};

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
}
