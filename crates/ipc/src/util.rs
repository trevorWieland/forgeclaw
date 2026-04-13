//! Internal utilities for the IPC crate.

/// Maximum characters for any peer-controlled string emitted to logs.
///
/// Strings longer than this are truncated with a suffix indicating the
/// original byte length, so operators see enough context to diagnose
/// without an attacker being able to flood log storage.
const MAX_LOG_FIELD_LEN: usize = 256;

/// Truncate a peer-controlled string for safe inclusion in structured
/// log fields. Returns the original string unchanged if it fits
/// within [`MAX_LOG_FIELD_LEN`]; otherwise truncates at a char
/// boundary and appends the total byte length.
pub(crate) fn truncate_for_log(s: &str) -> String {
    if s.len() <= MAX_LOG_FIELD_LEN {
        return s.to_owned();
    }
    // Find a char boundary at or before MAX_LOG_FIELD_LEN so we
    // don't split a multi-byte codepoint.
    let end = s
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|&i| i <= MAX_LOG_FIELD_LEN)
        .last()
        .unwrap_or(0);
    format!("{}...<truncated, {} bytes total>", &s[..end], s.len())
}

#[cfg(test)]
mod tests {
    use super::{MAX_LOG_FIELD_LEN, truncate_for_log};

    #[test]
    fn short_string_unchanged() {
        let s = "hello world";
        assert_eq!(truncate_for_log(s), s);
    }

    #[test]
    fn exactly_max_len_unchanged() {
        let s = "x".repeat(MAX_LOG_FIELD_LEN);
        assert_eq!(truncate_for_log(&s), s);
    }

    #[test]
    fn over_max_is_truncated_with_suffix() {
        let s = "y".repeat(MAX_LOG_FIELD_LEN + 100);
        let result = truncate_for_log(&s);
        assert!(result.len() < s.len());
        assert!(result.contains("...<truncated,"));
        assert!(result.contains("bytes total>"));
    }

    #[test]
    fn multibyte_chars_not_split() {
        // 4-byte UTF-8 char repeated so total exceeds the cap.
        let s = "🔥".repeat(MAX_LOG_FIELD_LEN);
        let result = truncate_for_log(&s);
        // Should still be valid UTF-8 (no panic on format!).
        assert!(result.contains("...<truncated,"));
    }
}
