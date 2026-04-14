//! Error taxonomy for the IPC crate.
//!
//! The crate defines three nested error types so callers can reason
//! about failure at the layer that produced them:
//!
//! - [`FrameError`] — everything that can go wrong at the framing layer
//!   (length prefix, UTF-8 decode, JSON decode, size limits).
//! - [`ProtocolError`] — wire-shape was valid but the message violated
//!   the protocol contract (unsupported version, wrong message at the
//!   wrong time, unknown `type` discriminator).
//! - [`IpcError`] — the top-level error returned by server / client
//!   methods. Wraps the two categories above plus I/O and
//!   encode-side serialization failures.
//!
//! # Payload hygiene
//!
//! Error messages never echo untrusted payload bytes. `MalformedJson`
//! carries only the `serde_json::Error::to_string()` output (which
//! contains a position, not the raw text). Oversize / truncated errors
//! carry byte counts. This keeps logs safe to emit without reviewing
//! payloads first.

use forgeclaw_core::GroupId;

/// Framing-layer errors.
///
/// Any of these variants signals that the connection is no longer in a
/// recoverable state — the server / client wrapper tears down the
/// socket and bubbles the error up to the caller. Per
/// [`docs/IPC_PROTOCOL.md`](../../../docs/IPC_PROTOCOL.md) §Error
/// Handling, malformed frames always close the socket.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// A frame's declared length exceeds [`crate::codec::MAX_FRAME_BYTES`].
    #[error("frame size {size} exceeds max {max}")]
    Oversize {
        /// The oversized length the peer declared.
        size: usize,
        /// The configured maximum.
        max: usize,
    },

    /// A frame's declared length is zero.
    ///
    /// No message type in this protocol legitimately serializes to an
    /// empty payload, so zero-length frames are rejected at the
    /// framing layer instead of being passed to the deserializer.
    #[error("empty frame (zero-length payload)")]
    EmptyFrame,

    /// The peer closed the socket mid-frame.
    ///
    /// Reported only when the peer disconnected *after* sending at
    /// least one byte of a frame; a clean disconnect between frames
    /// surfaces as [`IpcError::Closed`] instead.
    #[error("truncated frame: expected {expected} bytes, got {got}")]
    Truncated {
        /// The number of payload bytes the length prefix declared.
        expected: usize,
        /// How many payload bytes were actually received before EOF.
        got: usize,
    },

    /// The frame payload was not valid UTF-8.
    #[error("invalid UTF-8 in frame payload")]
    InvalidUtf8,

    /// The frame payload was valid UTF-8 but not valid JSON, or the
    /// JSON did not match any known message type or payload shape.
    ///
    /// Stores only the `serde_json::Error` display output (line /
    /// column), never the raw payload text.
    #[error("malformed JSON: {0}")]
    MalformedJson(String),
}

/// Protocol-layer errors.
///
/// The wire shape was valid — bytes deserialized into a JSON object —
/// but the message violated the contract in `docs/IPC_PROTOCOL.md`.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    /// The peer advertised a protocol version whose major does not
    /// match [`crate::version::PROTOCOL_VERSION`].
    #[error("unsupported protocol version: peer={peer}, local={local}")]
    UnsupportedVersion {
        /// The version string the peer advertised in its `Ready`.
        peer: String,
        /// The local version this crate implements.
        local: &'static str,
    },

    /// The handshake received a message that was structurally valid
    /// but not the expected type for this step of the lifecycle.
    #[error("unexpected message: expected {expected}, got {got}")]
    UnexpectedMessage {
        /// The message type name the handshake expected.
        expected: &'static str,
        /// The message type name actually received.
        got: &'static str,
    },

    /// The frame decoded as JSON with a `type` field whose value does
    /// not match any known message variant.
    ///
    /// Callers may choose to log-and-ignore (forward compatibility) or
    /// tear down the connection; the codec surfaces the error without
    /// prescribing policy. Per `docs/IPC_PROTOCOL.md` §Error Handling,
    /// hosts typically log-and-continue so adapters may send
    /// experimental message types.
    #[error("unknown message type: {0}")]
    UnknownMessageType(String),

    /// The peer sent too many consecutive frames with unknown message
    /// types, exceeding the configured skip limit.
    ///
    /// This is fatal — the connection is poisoned when this fires.
    #[error("too many unknown messages: {count} consecutive (limit {limit})")]
    TooManyUnknownMessages {
        /// How many consecutive unknown messages were received.
        count: usize,
        /// The configured limit.
        limit: usize,
    },

    /// A command was rejected because the session lacks the required
    /// authorization.
    ///
    /// This is non-fatal — the connection remains open for further
    /// messages (the rejected command is simply dropped).
    #[error("unauthorized: command {command} — {reason}")]
    Unauthorized {
        /// The wire name of the command that was rejected.
        command: &'static str,
        /// Why the command was rejected.
        reason: &'static str,
    },

    /// The group identity in the `init` payload diverges from the
    /// host-authoritative group identity passed to `handshake`.
    ///
    /// This is fatal — the handshake cannot proceed with inconsistent
    /// group state.
    #[error(
        "group mismatch: init carried group {init_group_id}, \
         but session group is {session_group_id}"
    )]
    GroupMismatch {
        /// The group ID from `init.context.group`.
        init_group_id: GroupId,
        /// The group ID the host passed as the authoritative identity.
        session_group_id: GroupId,
    },
}

/// Top-level IPC crate error.
#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    /// Underlying I/O failure (socket, filesystem, accept).
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Framing-layer error.
    #[error("frame error: {0}")]
    Frame(#[from] FrameError),

    /// Encode-side JSON serialization failure.
    ///
    /// This is deliberately *not* `#[from] serde_json::Error` so that
    /// decode-side JSON failures (handled via
    /// [`FrameError::MalformedJson`]) cannot accidentally be routed
    /// here.
    #[error("serialization error: {0}")]
    Serialize(String),

    /// Protocol-layer error.
    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),

    /// The peer closed the connection cleanly between frames.
    #[error("connection closed")]
    Closed,

    /// A timed operation (handshake, recv) exceeded its deadline.
    #[error("operation timed out after {0:?}")]
    Timeout(std::time::Duration),
}

impl IpcError {
    /// Builds an [`IpcError::Serialize`] from a `serde_json::Error`.
    ///
    /// Encoding is the one place where we own the object being
    /// serialized, so carrying the error's display output back to the
    /// caller is safe — the "untrusted payload" concern only applies
    /// to decode errors.
    pub(crate) fn serialize(err: &serde_json::Error) -> Self {
        Self::Serialize(err.to_string())
    }

    /// Returns `true` if this error implies the connection is no
    /// longer in a recoverable state and should be torn down.
    ///
    /// Fatal errors include all framing errors (the stream is
    /// desynchronized), I/O failures, timeouts, version mismatches,
    /// and unexpected messages during handshake. Non-fatal:
    /// [`ProtocolError::UnknownMessageType`] (forward compatibility),
    /// [`IpcError::Closed`] (already done), and
    /// [`IpcError::Serialize`] (encode-side only).
    #[must_use]
    pub fn is_fatal(&self) -> bool {
        match self {
            Self::Io(_) | Self::Frame(_) | Self::Timeout(_) => true,
            Self::Protocol(p) => !matches!(
                p,
                ProtocolError::UnknownMessageType(_) | ProtocolError::Unauthorized { .. }
            ),
            Self::Serialize(_) | Self::Closed => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FrameError, IpcError, ProtocolError};

    #[test]
    fn oversize_display_carries_counts_not_bytes() {
        let err = FrameError::Oversize {
            size: 20_000_000,
            max: 10_485_760,
        };
        let display = err.to_string();
        assert!(display.contains("20000000"));
        assert!(display.contains("10485760"));
    }

    #[test]
    fn empty_frame_display() {
        assert_eq!(
            FrameError::EmptyFrame.to_string(),
            "empty frame (zero-length payload)"
        );
    }

    #[test]
    fn truncated_display() {
        let err = FrameError::Truncated {
            expected: 100,
            got: 37,
        };
        assert_eq!(
            err.to_string(),
            "truncated frame: expected 100 bytes, got 37"
        );
    }

    #[test]
    fn invalid_utf8_display() {
        assert_eq!(
            FrameError::InvalidUtf8.to_string(),
            "invalid UTF-8 in frame payload"
        );
    }

    #[test]
    fn unsupported_version_display() {
        let err = ProtocolError::UnsupportedVersion {
            peer: "2.0".to_owned(),
            local: "1.0",
        };
        assert_eq!(
            err.to_string(),
            "unsupported protocol version: peer=2.0, local=1.0"
        );
    }

    #[test]
    fn unexpected_message_display() {
        let err = ProtocolError::UnexpectedMessage {
            expected: "ready",
            got: "output_delta",
        };
        assert_eq!(
            err.to_string(),
            "unexpected message: expected ready, got output_delta"
        );
    }

    #[test]
    fn unknown_message_type_display_includes_name() {
        let err = ProtocolError::UnknownMessageType("bogus".to_owned());
        assert!(err.to_string().contains("bogus"));
    }

    #[test]
    fn frame_error_wraps_into_ipc_error() {
        let ipc: IpcError = FrameError::EmptyFrame.into();
        assert!(matches!(ipc, IpcError::Frame(FrameError::EmptyFrame)));
    }

    #[test]
    fn io_error_wraps_into_ipc_error() {
        let io = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset");
        let ipc: IpcError = io.into();
        assert!(matches!(ipc, IpcError::Io(_)));
    }

    #[test]
    fn protocol_error_wraps_into_ipc_error() {
        let p = ProtocolError::UnknownMessageType("x".to_owned());
        let ipc: IpcError = p.into();
        assert!(matches!(ipc, IpcError::Protocol(_)));
    }

    #[test]
    fn too_many_unknown_messages_display() {
        let err = ProtocolError::TooManyUnknownMessages {
            count: 33,
            limit: 32,
        };
        assert_eq!(
            err.to_string(),
            "too many unknown messages: 33 consecutive (limit 32)"
        );
    }

    #[test]
    fn too_many_unknown_messages_is_fatal() {
        let err = IpcError::Protocol(ProtocolError::TooManyUnknownMessages {
            count: 33,
            limit: 32,
        });
        assert!(err.is_fatal());
    }

    #[test]
    fn unauthorized_display() {
        let err = ProtocolError::Unauthorized {
            command: "register_group",
            reason: "requires main-group privilege",
        };
        let s = err.to_string();
        assert!(s.contains("register_group"));
        assert!(s.contains("requires main-group privilege"));
    }

    #[test]
    fn unauthorized_is_not_fatal() {
        let err = IpcError::Protocol(ProtocolError::Unauthorized {
            command: "register_group",
            reason: "requires main-group privilege",
        });
        assert!(!err.is_fatal());
    }

    #[test]
    fn group_mismatch_display() {
        let err = ProtocolError::GroupMismatch {
            init_group_id: "group-a".into(),
            session_group_id: "group-b".into(),
        };
        let s = err.to_string();
        assert!(s.contains("group-a"));
        assert!(s.contains("group-b"));
    }

    #[test]
    fn group_mismatch_is_fatal() {
        let err = IpcError::Protocol(ProtocolError::GroupMismatch {
            init_group_id: "group-a".into(),
            session_group_id: "group-b".into(),
        });
        assert!(err.is_fatal());
    }
}
