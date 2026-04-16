//! Semantically validated wire-string types used in IPC messages.

pub(crate) mod bounded_text;
mod time;
mod url;

pub use bounded_text::{
    AdapterName, AdapterVersion, BoundedTextError, BranchName, ContextModeText,
    EnvironmentProfileText, GroupName, ListItemText, MessageText, ModelText, OutputDeltaText,
    OutputResultText, ProjectName, PromptText, ProtocolVersionText, ScheduleValueText, SenderName,
    SessionIdText, ShortText, StageName, TokenText,
};
pub use time::{IanaTimezone, IpcTimestamp, TimestampError, TimezoneError};
pub use url::{AbsoluteHttpUrl, AbsoluteHttpUrlError};

#[cfg(test)]
mod tests {
    use super::{
        AbsoluteHttpUrl, AdapterName, AdapterVersion, BranchName, IanaTimezone, IpcTimestamp,
        ListItemText, MessageText, ModelText, OutputDeltaText, OutputResultText, PromptText,
        ProtocolVersionText, ScheduleValueText, SessionIdText, ShortText, StageName, TokenText,
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
        assert!(AdapterName::new("id").is_ok());
        assert!(AdapterVersion::new("1.0.0").is_ok());
        assert!(StageName::new("stage").is_ok());
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

        assert!(AdapterName::new("x".repeat(129)).is_err());
        assert!(AdapterVersion::new("x".repeat(129)).is_err());
        assert!(StageName::new("x".repeat(129)).is_err());
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
    fn protocol_version_text_enforces_major_minor() {
        assert!(ProtocolVersionText::new("1.0").is_ok());
        assert!(ProtocolVersionText::new("42.17").is_ok());
        assert!(ProtocolVersionText::new("0.0").is_ok());
    }

    #[test]
    fn protocol_version_text_rejects_malformed_formats() {
        for bad in ["", "1", "1.", ".1", "1.foo", "v1.0", "1.0.3", "one.two"] {
            assert!(
                ProtocolVersionText::new(bad).is_err(),
                "expected rejection for `{bad}`"
            );
        }
    }

    #[test]
    fn branch_name_accepts_common_refs() {
        for good in ["main", "feat/x", "release/1.x", "lane-0.4", "a/b/c"] {
            assert!(BranchName::new(good).is_ok(), "expected `{good}` to parse");
        }
    }

    #[test]
    fn branch_name_rejects_malformed_refs() {
        for bad in [
            "",
            "/main",
            "main/",
            "a//b",
            "a..b",
            "with space",
            "with\ttab",
            "control\x01char",
        ] {
            assert!(
                BranchName::new(bad).is_err(),
                "expected rejection for `{bad}`"
            );
        }
    }

    #[test]
    fn absolute_http_url_enforces_scheme() {
        assert!(AbsoluteHttpUrl::new("https://example.com").is_ok());
        assert!(AbsoluteHttpUrl::new("http://example.com/v1").is_ok());
        assert!(AbsoluteHttpUrl::new("ftp://example.com").is_err());
        assert!(AbsoluteHttpUrl::new("/relative").is_err());
    }
}
