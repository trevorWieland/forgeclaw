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

    /// A command-specific receive API observed a valid non-command
    /// message.
    ///
    /// This is non-fatal and expected when callers consume a shared
    /// inbound stream that may legally interleave `command` with other
    /// message types (for example `output_delta` and `heartbeat`).
    #[error("not a command message: got {got}")]
    NotCommand {
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

    /// The peer sent too many cumulative bytes across consecutive
    /// unknown-message frames, exceeding the configured budget.
    ///
    /// This is fatal — the connection is poisoned when this fires.
    #[error("too many unknown message bytes: {bytes} bytes (limit {limit})")]
    TooManyUnknownBytes {
        /// How many unknown-message bytes were observed in the current
        /// consecutive streak.
        bytes: usize,
        /// The configured byte limit.
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

    /// Repeated unauthorized commands exceeded per-connection abuse
    /// limits and triggered forced disconnect.
    #[error(
        "unauthorized command abuse: command {command} exceeded limit ({strikes}/{disconnect_after_strikes}) — {reason}"
    )]
    UnauthorizedCommandAbuse {
        /// The wire name of the command being abused.
        command: &'static str,
        /// Why each attempt is unauthorized.
        reason: &'static str,
        /// Consecutive exhausted-budget strikes observed.
        strikes: u32,
        /// Configured strike threshold that triggers disconnect.
        disconnect_after_strikes: u32,
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

    /// A message was valid JSON but arrived at an illegal protocol
    /// lifecycle phase.
    #[error(
        "lifecycle violation: {direction} message `{message_type}` is invalid in phase {phase} ({reason})"
    )]
    LifecycleViolation {
        /// Current server-side lifecycle phase.
        phase: &'static str,
        /// Message direction (`host_to_container` or
        /// `container_to_host`).
        direction: &'static str,
        /// Wire `type` discriminator.
        message_type: &'static str,
        /// Human-readable reason for the rejection.
        reason: &'static str,
    },

    /// A message with a job-scoped payload referenced the wrong job.
    #[error(
        "job_id mismatch: {direction} message `{message_type}` expected job_id {expected}, got {got}"
    )]
    JobIdMismatch {
        /// Message direction (`host_to_container` or
        /// `container_to_host`).
        direction: &'static str,
        /// Wire `type` discriminator.
        message_type: &'static str,
        /// Active job bound to the connection.
        expected: forgeclaw_core::JobId,
        /// Job ID provided in the incoming message.
        got: forgeclaw_core::JobId,
    },

    /// No heartbeat was observed before the processing deadline.
    ///
    /// This is non-fatal at the IPC layer so the caller can execute
    /// the spec-required escalation path (send `shutdown`, then kill
    /// the container if needed).
    #[error("heartbeat timeout: no heartbeat for {timeout_secs}s while in phase {phase}")]
    HeartbeatTimeout {
        /// Current server-side lifecycle phase (`processing`).
        phase: &'static str,
        /// Configured heartbeat timeout in seconds.
        timeout_secs: u64,
    },

    /// The container failed to complete draining before the host's
    /// shutdown deadline.
    ///
    /// This is non-fatal at the IPC layer so the caller can execute
    /// the spec-required escalation path (force-kill the container).
    #[error(
        "shutdown deadline exceeded: no completion within {deadline_ms}ms while in phase {phase}"
    )]
    ShutdownDeadlineExceeded {
        /// Current server-side lifecycle phase (`draining`).
        phase: &'static str,
        /// Shutdown deadline configured by the host message.
        deadline_ms: u64,
    },

    /// A command payload is syntactically valid JSON but violates
    /// version-sensitive protocol rules.
    #[error("invalid command payload: {command} — {reason}")]
    InvalidCommandPayload {
        /// Wire command name.
        command: &'static str,
        /// Validation failure reason.
        reason: &'static str,
    },
}

/// Top-level IPC crate error.
#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    /// Underlying I/O failure (socket, filesystem, accept).
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Socket bind attestation detected a path race.
    #[error("socket bind race detected: {0}")]
    BindRace(String),

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
            Self::Io(_) | Self::BindRace(_) | Self::Frame(_) | Self::Timeout(_) => true,
            Self::Protocol(p) => !matches!(
                p,
                ProtocolError::UnknownMessageType(_)
                    | ProtocolError::NotCommand { .. }
                    | ProtocolError::Unauthorized { .. }
                    | ProtocolError::HeartbeatTimeout { .. }
                    | ProtocolError::ShutdownDeadlineExceeded { .. }
            ),
            Self::Serialize(_) | Self::Closed => false,
        }
    }
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;
