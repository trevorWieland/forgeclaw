//! Semantically validated wire-string types used in IPC messages.

mod bounded_text;
mod time;
mod url;

pub use bounded_text::{
    BoundedTextError, IdentifierText, ListItemText, MessageText, ModelText, OutputDeltaText,
    OutputResultText, PromptText, ScheduleValueText, SessionIdText, ShortText, TokenText,
};
pub use time::{IanaTimezone, IpcTimestamp, TimestampError, TimezoneError};
pub use url::{AbsoluteHttpUrl, AbsoluteHttpUrlError};

#[cfg(test)]
mod tests {
    use super::{
        AbsoluteHttpUrl, IanaTimezone, IdentifierText, IpcTimestamp, ListItemText, MessageText,
        ModelText, OutputDeltaText, OutputResultText, PromptText, ScheduleValueText, SessionIdText,
        ShortText, TokenText,
    };

    #[test]
    fn timestamp_accepts_rfc3339() {
        let ts = IpcTimestamp::new("2026-04-03T10:00:00Z").expect("valid timestamp");
        assert_eq!(ts.as_str(), "2026-04-03T10:00:00Z");
    }

    #[test]
    fn timestamp_rejects_non_rfc3339() {
        let err = IpcTimestamp::new("2026-04-03 10:00:00").expect_err("invalid timestamp");
        assert!(err.value.contains("2026-04-03"));
    }

    #[test]
    fn timezone_accepts_iana_name() {
        let tz = IanaTimezone::new("America/New_York").expect("valid timezone");
        assert_eq!(tz.as_str(), "America/New_York");
    }

    #[test]
    fn timezone_rejects_invalid_name() {
        let err = IanaTimezone::new("Mars/Olympus").expect_err("invalid timezone");
        assert_eq!(err.value, "Mars/Olympus");
    }

    #[test]
    fn bounded_text_types_enforce_max_lengths() {
        assert!(IdentifierText::new("id").is_ok());
        assert!(ModelText::new("m").is_ok());
        assert!(TokenText::new("t").is_ok());
        assert!(ShortText::new("short").is_ok());
        assert!(ScheduleValueText::new("*/5 * * * *").is_ok());
        assert!(MessageText::new("hello").is_ok());
        assert!(PromptText::new("prompt").is_ok());
        assert!(OutputDeltaText::new("delta").is_ok());
        assert!(OutputResultText::new("result").is_ok());
        assert!(SessionIdText::new("sess").is_ok());
        assert!(ListItemText::new("item").is_ok());

        assert!(IdentifierText::new("x".repeat(129)).is_err());
        assert!(ModelText::new("x".repeat(257)).is_err());
        assert!(TokenText::new("x".repeat(2049)).is_err());
        assert!(ShortText::new("x".repeat(1025)).is_err());
        assert!(ScheduleValueText::new("x".repeat(513)).is_err());
        assert!(MessageText::new("x".repeat(32 * 1024 + 1)).is_err());
        assert!(PromptText::new("x".repeat(32 * 1024 + 1)).is_err());
        assert!(OutputDeltaText::new("x".repeat(64 * 1024 + 1)).is_err());
        assert!(OutputResultText::new("x".repeat(256 * 1024 + 1)).is_err());
        assert!(SessionIdText::new("x".repeat(129)).is_err());
        assert!(ListItemText::new("x".repeat(1025)).is_err());
    }

    #[test]
    fn absolute_http_url_enforces_scheme() {
        assert!(AbsoluteHttpUrl::new("https://example.com").is_ok());
        assert!(AbsoluteHttpUrl::new("http://example.com/v1").is_ok());
        assert!(AbsoluteHttpUrl::new("ftp://example.com").is_err());
        assert!(AbsoluteHttpUrl::new("/relative").is_err());
    }
}
