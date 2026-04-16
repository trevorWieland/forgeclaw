//! Internal utilities for the IPC crate.

pub(crate) mod sampler;

use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use tokio::io::AsyncWrite;

/// Maximum bytes for any peer-controlled string emitted to logs.
const MAX_LOG_FIELD_BYTES: usize = 256;

/// Truncate and sanitize a peer-controlled string for safe inclusion in
/// structured log fields.
///
/// Two invariants:
///
/// 1. The original byte length is reported in the truncation suffix so
///    operators can tell how much input was elided.
/// 2. Every Unicode `is_control()` scalar (ASCII C0/C1, DEL, and the
///    rest of the Unicode control range) is escaped via
///    [`char::escape_default`] so newlines, tabs, ANSI escape
///    sequences, and other invisible bytes cannot disrupt log
///    parseability or smuggle terminal control sequences. Non-control
///    Unicode (including extended scalars and emoji) passes through
///    unchanged.
pub(crate) fn truncate_for_log(s: &str) -> String {
    if s.len() <= MAX_LOG_FIELD_BYTES {
        return sanitize_for_log(s);
    }
    let mut end = MAX_LOG_FIELD_BYTES;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}...<truncated, {} bytes total>",
        sanitize_for_log(&s[..end]),
        s.len()
    )
}

/// Escape control characters in a string using [`char::escape_default`]
/// so they cannot disturb downstream log sinks. ASCII C0/C1 control
/// codes, DEL, and any Unicode scalar with `is_control()` are escaped;
/// printable scalars are left as-is.
fn sanitize_for_log(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_control() {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
    }
    out
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

    #[test]
    fn escapes_newline() {
        assert_eq!(truncate_for_log("\n"), "\\n");
        assert_eq!(truncate_for_log("line1\nline2"), "line1\\nline2");
    }

    #[test]
    fn escapes_tab() {
        assert_eq!(truncate_for_log("\t"), "\\t");
        assert_eq!(truncate_for_log("col1\tcol2"), "col1\\tcol2");
    }

    #[test]
    fn escapes_carriage_return() {
        assert_eq!(truncate_for_log("\r"), "\\r");
        assert_eq!(truncate_for_log("line\r\nfeed"), "line\\r\\nfeed");
    }

    #[test]
    fn escapes_ansi_escape_sequence() {
        // ANSI color codes start with ESC (0x1b). Escaping the ESC
        // byte neutralizes the entire sequence in plaintext sinks.
        let result = truncate_for_log("\x1b[31mred\x1b[0m");
        assert_eq!(result, "\\u{1b}[31mred\\u{1b}[0m");
    }

    #[test]
    fn escapes_null_byte() {
        assert_eq!(truncate_for_log("\0"), "\\u{0}");
        assert_eq!(truncate_for_log("a\0b"), "a\\u{0}b");
    }

    #[test]
    fn escapes_del_char() {
        assert_eq!(truncate_for_log("\x7f"), "\\u{7f}");
    }

    #[test]
    fn escapes_c1_control_codes() {
        // U+0080..U+009F are also control codes.
        assert_eq!(truncate_for_log("\u{80}"), "\\u{80}");
        assert_eq!(truncate_for_log("\u{9f}"), "\\u{9f}");
    }

    #[test]
    fn preserves_non_control_unicode() {
        for s in ["héllo", "🔥", "日本語", "Ω", "\\backslash", "'quote'"] {
            assert_eq!(truncate_for_log(s), s, "must preserve `{s}`");
        }
    }

    #[test]
    fn sanitizes_within_truncated_prefix() {
        // Control chars in the kept prefix still get escaped, and the
        // truncation suffix still reports the *original* byte length.
        let payload = format!(
            "{}\n{}",
            "a".repeat(MAX_LOG_FIELD_BYTES / 2),
            "b".repeat(500)
        );
        let result = truncate_for_log(&payload);
        let original_len = payload.len();
        assert!(
            result.contains("\\n"),
            "control char in retained prefix must be escaped"
        );
        assert!(
            result.contains(&format!("...<truncated, {original_len} bytes total>")),
            "suffix must report original byte length, got: {result}"
        );
    }

    #[test]
    fn sanitization_does_not_introduce_raw_control_chars() {
        // Pathological input: mixture of every kind of control char.
        let messy = "\0\t\n\r\x1b\x7f\u{80}\u{9f}plain";
        let result = truncate_for_log(messy);
        assert!(
            !result.chars().any(char::is_control),
            "sanitized output must not contain control chars, got: {result:?}"
        );
        assert!(result.contains("plain"));
    }
}
