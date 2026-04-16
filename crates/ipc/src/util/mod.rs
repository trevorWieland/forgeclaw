//! Internal utilities for the IPC crate.

pub(crate) mod sampler;

use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use tokio::io::AsyncWrite;

/// Maximum bytes for any peer-controlled string emitted to logs.
const MAX_LOG_FIELD_BYTES: usize = 256;

/// Truncate a peer-controlled string for safe inclusion in structured
/// log fields.
pub(crate) fn truncate_for_log(s: &str) -> String {
    if s.len() <= MAX_LOG_FIELD_BYTES {
        return s.to_owned();
    }
    let mut end = MAX_LOG_FIELD_BYTES;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...<truncated, {} bytes total>", &s[..end], s.len())
}

// ── Shared write half for split-mode cross-half shutdown ──────────

/// An `AsyncWrite` wrapper that stores the inner writer behind a
/// shared mutex so the reader half can take and shut it down on fatal
/// errors. This is the mechanism that makes malformed-frame teardown
/// immediately peer-visible in split mode.
///
/// The `std::sync::Mutex` is held only during synchronous `poll_*`
/// calls (never across `.await` points). Contention is negligible:
/// the writer is the only caller that uses `poll_*`, and the reader
/// only locks briefly to `take()` the half on the fatal-error path.
pub(crate) struct SharedWriteHalf<T> {
    inner: Arc<Mutex<Option<T>>>,
}

impl<T> SharedWriteHalf<T> {
    pub(crate) fn new(writer: T) -> (Self, ShutdownHandle<T>) {
        let inner = Arc::new(Mutex::new(Some(writer)));
        let handle = ShutdownHandle {
            inner: Arc::clone(&inner),
        };
        (Self { inner }, handle)
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for SharedWriteHalf<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedWriteHalf").finish_non_exhaustive()
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for SharedWriteHalf<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let Ok(mut guard) = self.inner.lock() else {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "write half mutex poisoned",
            )));
        };
        let Some(ref mut half) = *guard else {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "write half shut down by reader",
            )));
        };
        Pin::new(half).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let Ok(mut guard) = self.inner.lock() else {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "write half mutex poisoned",
            )));
        };
        let Some(ref mut half) = *guard else {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "write half shut down by reader",
            )));
        };
        Pin::new(half).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let Ok(mut guard) = self.inner.lock() else {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "write half mutex poisoned",
            )));
        };
        let Some(ref mut half) = *guard else {
            return Poll::Ready(Ok(()));
        };
        Pin::new(half).poll_shutdown(cx)
    }
}

/// A handle that the reader half holds to shut down the write half
/// on fatal errors, making the teardown immediately peer-visible.
pub(crate) struct ShutdownHandle<T> {
    inner: Arc<Mutex<Option<T>>>,
}

impl<T> Clone for ShutdownHandle<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T: AsyncWrite + Unpin> ShutdownHandle<T> {
    /// Take the write half out and shut it down. After this call,
    /// the writer's `AsyncWrite` methods will return `BrokenPipe`.
    /// The peer will observe EOF on their read side.
    pub(crate) async fn trigger(&self) {
        let half = {
            let Ok(mut guard) = self.inner.lock() else {
                return;
            };
            guard.take()
        };
        // Guard dropped — now shut down outside the lock.
        if let Some(mut h) = half {
            let _ = tokio::io::AsyncWriteExt::shutdown(&mut h).await;
        }
    }
}

impl<T> std::fmt::Debug for ShutdownHandle<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShutdownHandle").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_LOG_FIELD_BYTES, truncate_for_log};

    #[test]
    fn short_string_unchanged() {
        let s = "hello world";
        assert_eq!(truncate_for_log(s), s);
    }

    #[test]
    fn exactly_max_len_unchanged() {
        let s = "x".repeat(MAX_LOG_FIELD_BYTES);
        assert_eq!(truncate_for_log(&s), s);
    }

    #[test]
    fn over_max_is_truncated_with_suffix() {
        let s = "y".repeat(MAX_LOG_FIELD_BYTES + 100);
        let result = truncate_for_log(&s);
        assert!(result.len() < s.len());
        assert!(result.contains("...<truncated,"));
        assert!(result.contains("bytes total>"));
    }

    #[test]
    fn multibyte_chars_not_split() {
        let s = "🔥".repeat(MAX_LOG_FIELD_BYTES);
        let result = truncate_for_log(&s);
        let prefix_bytes = result
            .split("...<truncated,")
            .next()
            .expect("prefix before suffix")
            .len();
        assert!(prefix_bytes <= MAX_LOG_FIELD_BYTES);
        assert!(result.contains("...<truncated,"));
    }
}
