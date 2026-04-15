use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::message::limits;

/// Error returned when a bounded text value exceeds its max length.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("text length {actual} exceeds maximum {max}")]
pub struct BoundedTextError {
    /// Maximum allowed character length.
    pub max: usize,
    /// Actual character length seen.
    pub actual: usize,
}

macro_rules! bounded_text_type {
    (
        $(#[$meta:meta])*
        $name:ident,
        max = $max:expr,
        desc = $desc:literal
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Maximum allowed length in Unicode scalar values.
            pub const MAX_LEN: usize = $max;

            /// Construct a validated text value.
            pub fn new(value: impl Into<String>) -> Result<Self, BoundedTextError> {
                let value = value.into();
                let actual = value.chars().count();
                if actual > Self::MAX_LEN {
                    return Err(BoundedTextError {
                        max: Self::MAX_LEN,
                        actual,
                    });
                }
                Ok(Self(value))
            }

            /// Access the wire value.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Consume and return the owned string.
            #[must_use]
            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl std::ops::Deref for $name {
            type Target = str;

            fn deref(&self) -> &Self::Target {
                self.as_str()
            }
        }

        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self.as_str() == *other
            }
        }

        impl PartialEq<$name> for &str {
            fn eq(&self, other: &$name) -> bool {
                *self == other.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = BoundedTextError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::new(s)
            }
        }

        impl TryFrom<String> for $name {
            type Error = BoundedTextError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = BoundedTextError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let raw = String::deserialize(deserializer)?;
                Self::new(raw).map_err(serde::de::Error::custom)
            }
        }

        #[cfg(feature = "json-schema")]
        impl schemars::JsonSchema for $name {
            fn inline_schema() -> bool {
                true
            }

            fn schema_name() -> Cow<'static, str> {
                stringify!($name).into()
            }

            fn json_schema(_gen: &mut schemars::SchemaGenerator) -> schemars::Schema {
                schemars::json_schema!({
                    "type": "string",
                    "maxLength": Self::MAX_LEN,
                    "description": $desc,
                })
            }
        }
    };
}

bounded_text_type!(
    /// Identifier-like text value (adapter, stage, branch, group names, etc.).
    IdentifierText,
    max = limits::MAX_IDENTIFIER_TEXT_CHARS,
    desc = "Identifier-like IPC text field with maxLength 128."
);

bounded_text_type!(
    /// Model identifier text.
    ModelText,
    max = limits::MAX_MODEL_TEXT_CHARS,
    desc = "Model identifier with maxLength 256."
);

bounded_text_type!(
    /// Token or credential text value.
    TokenText,
    max = limits::MAX_TOKEN_TEXT_CHARS,
    desc = "Token-like IPC text field with maxLength 2048."
);

bounded_text_type!(
    /// Generic short freeform text.
    ShortText,
    max = limits::MAX_SHORT_TEXT_CHARS,
    desc = "Short IPC text field with maxLength 1024."
);

bounded_text_type!(
    /// Schedule expression/value text.
    ScheduleValueText,
    max = limits::MAX_SCHEDULE_VALUE_TEXT_CHARS,
    desc = "Schedule value with maxLength 512."
);

bounded_text_type!(
    /// Message-sized text value.
    MessageText,
    max = limits::MAX_MESSAGE_TEXT_CHARS,
    desc = "Message text with maxLength 32768."
);

bounded_text_type!(
    /// Prompt/objective text value.
    PromptText,
    max = limits::MAX_PROMPT_TEXT_CHARS,
    desc = "Prompt/objective text with maxLength 32768."
);

bounded_text_type!(
    /// Incremental streamed output chunk.
    OutputDeltaText,
    max = limits::MAX_OUTPUT_DELTA_TEXT_CHARS,
    desc = "Output delta text with maxLength 65536."
);

bounded_text_type!(
    /// Final result text payload.
    OutputResultText,
    max = limits::MAX_OUTPUT_RESULT_TEXT_CHARS,
    desc = "Output completion result text with maxLength 262144."
);

bounded_text_type!(
    /// Session identifier text.
    SessionIdText,
    max = limits::MAX_SESSION_ID_TEXT_CHARS,
    desc = "Session identifier text with maxLength 128."
);

bounded_text_type!(
    /// Self-improvement scope/acceptance list item text.
    ListItemText,
    max = limits::MAX_LIST_ITEM_TEXT_CHARS,
    desc = "Self-improvement list item text with maxLength 1024."
);
