//! Explicit receive policies for unknown message types.

/// How receive APIs should handle unknown message types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownTypePolicy {
    /// Spec-default behavior: skip unknown message types with bounded
    /// count/byte budgets.
    SkipBounded,
    /// Strict behavior: return [`crate::ProtocolError::UnknownMessageType`]
    /// immediately for unknown types.
    Strict,
}
