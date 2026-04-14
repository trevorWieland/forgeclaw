//! Shared protocol policy primitives.

use std::time::Duration;

use crate::codec::DEFAULT_MAX_UNKNOWN_SKIPS;
use crate::error::{IpcError, ProtocolError};

/// Cumulative byte budget for consecutive unknown-type frames.
pub(crate) const DEFAULT_MAX_UNKNOWN_BYTES: usize = 1024 * 1024;

/// Processing-phase heartbeat timeout per protocol spec.
pub(crate) const DEFAULT_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(60);

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
