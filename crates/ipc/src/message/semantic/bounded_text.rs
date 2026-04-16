use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::message::limits;
use crate::version::parse_version_text;

/// Error returned when a bounded text value fails validation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BoundedTextError {
    /// The value exceeded the maximum allowed Unicode scalar length.
    #[error("text length {actual} exceeds maximum {max}")]
    Length {
        /// Maximum allowed character length.
        max: usize,
        /// Actual character length seen.
        actual: usize,
    },
    /// The value failed a format-specific validator layered on top of the
    /// length check.
    #[error("invalid format: {reason}")]
    Format {
        /// Static reason describing why the format check failed.
        reason: &'static str,
    },
}

/// Format validator for `ProtocolVersionText`. Delegates to
/// [`crate::version::parse_version_text`] so the runtime contract stays
/// byte-identical to the handshake negotiation logic.
pub(crate) fn validate_protocol_version_text(value: &str) -> Result<(), BoundedTextError> {
    parse_version_text(value)
        .map(|_| ())
        .ok_or(BoundedTextError::Format {
            reason: "protocol version must match `<major>.<minor>` with non-negative integers",
        })
}

/// Advisory format validator for `BranchName`. Rejects obviously
/// invalid git refs — strict `git check-ref-format` validation remains
/// the responsibility of the host/store layer that owns git state.
pub(crate) fn validate_branch_name(value: &str) -> Result<(), BoundedTextError> {
    if value.is_empty() {
        return Err(BoundedTextError::Format {
            reason: "branch name must not be empty",
        });
    }
    if value.starts_with('/') || value.ends_with('/') {
        return Err(BoundedTextError::Format {
            reason: "branch name must not begin or end with `/`",
        });
    }
    if value.contains("//") {
        return Err(BoundedTextError::Format {
            reason: "branch name must not contain consecutive `/` segments",
        });
    }
    if value.contains("..") {
        return Err(BoundedTextError::Format {
            reason: "branch name must not contain `..`",
        });
    }
    if value
        .chars()
        .any(|c| c.is_ascii_control() || c.is_whitespace())
    {
        return Err(BoundedTextError::Format {
            reason: "branch name must not contain whitespace or ASCII control characters",
        });
    }
    Ok(())
}

macro_rules! bounded_text_type {
    (
        $(#[$meta:meta])*
        $name:ident,
        max = $max:expr,
        desc = $desc:literal $(,)?
    ) => {
        bounded_text_impl!(
            $(#[$meta])*
            $name,
            max = $max,
            new_body = |value: String, actual: usize| {
                if actual > $name::MAX_LEN {
                    return Err(BoundedTextError::Length {
                        max: $name::MAX_LEN,
                        actual,
                    });
                }
                Ok($name(value))
            },
            schema_body = {
                "type": "string",
                "maxLength": Self::MAX_LEN,
                "description": $desc,
            }
        );
    };
    (
        $(#[$meta:meta])*
        $name:ident,
        max = $max:expr,
        desc = $desc:literal,
        extra_validate = $validator:path $(,)?
    ) => {
        bounded_text_impl!(
            $(#[$meta])*
            $name,
            max = $max,
            new_body = |value: String, actual: usize| {
                if actual > $name::MAX_LEN {
                    return Err(BoundedTextError::Length {
                        max: $name::MAX_LEN,
                        actual,
                    });
                }
                $validator(value.as_str())?;
                Ok($name(value))
            },
            schema_body = {
                "type": "string",
                "maxLength": Self::MAX_LEN,
                "description": $desc,
            }
        );
    };
    (
        $(#[$meta:meta])*
        $name:ident,
        max = $max:expr,
        extra_validate = $validator:path,
        schema_body = $body:tt $(,)?
    ) => {
        bounded_text_impl!(
            $(#[$meta])*
            $name,
            max = $max,
            new_body = |value: String, actual: usize| {
                if actual > $name::MAX_LEN {
                    return Err(BoundedTextError::Length {
                        max: $name::MAX_LEN,
                        actual,
                    });
                }
                $validator(value.as_str())?;
                Ok($name(value))
            },
            schema_body = $body
        );
    };
}

macro_rules! bounded_text_impl {
    (
        $(#[$meta:meta])*
        $name:ident,
        max = $max:expr,
        new_body = $new_body:expr,
        schema_body = $body:tt $(,)?
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
                let body: fn(String, usize) -> Result<$name, BoundedTextError> = $new_body;
                body(value, actual)
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
                schemars::json_schema!($body)
            }
        }
    };
}

bounded_text_type!(
    /// Adapter name identifying an agent runtime (e.g. `claude-code`).
    AdapterName,
    max = limits::MAX_IDENTIFIER_TEXT_CHARS,
    desc = "Adapter runtime identifier with maxLength 128.",
);

bounded_text_type!(
    /// Adapter implementation version string (opaque to the protocol).
    AdapterVersion,
    max = limits::MAX_IDENTIFIER_TEXT_CHARS,
    desc = "Adapter implementation version with maxLength 128.",
);

bounded_text_type!(
    /// IPC protocol version advertised by an adapter at handshake time.
    ///
    /// Format is `<major>.<minor>` where both components are
    /// non-negative integers. The numeric format is enforced both at
    /// runtime construction/deserialization time AND in the published
    /// JSON Schema via the `pattern` keyword, so polyglot adapters
    /// generated from the schema cannot construct values that the
    /// runtime then rejects.
    ProtocolVersionText,
    max = limits::MAX_IDENTIFIER_TEXT_CHARS,
    extra_validate = validate_protocol_version_text,
    schema_body = {
        "type": "string",
        "minLength": 1,
        "maxLength": Self::MAX_LEN,
        "pattern": "^[0-9]+\\.[0-9]+$",
        "description": "IPC protocol version (`<major>.<minor>`, both numeric, no extra segments). Enforced identically at runtime and in the JSON Schema. maxLength 128.",
    },
);

bounded_text_type!(
    /// Logical stage name emitted on progress updates.
    StageName,
    max = limits::MAX_IDENTIFIER_TEXT_CHARS,
    desc = "Progress stage identifier with maxLength 128.",
);

bounded_text_type!(
    /// Display name of the author of a historical message.
    SenderName,
    max = limits::MAX_IDENTIFIER_TEXT_CHARS,
    desc = "Historical message sender display name with maxLength 128.",
);

bounded_text_type!(
    /// Human-readable group name bound to a session identity.
    GroupName,
    max = limits::MAX_IDENTIFIER_TEXT_CHARS,
    desc = "Group display name with maxLength 128.",
);

bounded_text_type!(
    /// Target project name for a Tanren dispatch.
    ProjectName,
    max = limits::MAX_IDENTIFIER_TEXT_CHARS,
    desc = "Tanren target project name with maxLength 128.",
);

bounded_text_type!(
    /// Git branch name for a Tanren dispatch.
    ///
    /// Layered advisory validator rejects obvious malformed refs
    /// (empty, whitespace, control characters, leading/trailing `/`,
    /// consecutive `/`, `..`). The same advisory rules are encoded in
    /// the published JSON Schema via `allOf` composition (positive
    /// `pattern` plus `not`-pattern clauses) so polyglot adapters
    /// generated from the schema accept and reject the same set of
    /// values the runtime does. Strict `git check-ref-format`
    /// semantics remain the responsibility of the host/store layer.
    BranchName,
    max = limits::MAX_IDENTIFIER_TEXT_CHARS,
    extra_validate = validate_branch_name,
    schema_body = {
        "type": "string",
        "minLength": 1,
        "maxLength": Self::MAX_LEN,
        "allOf": [
            { "pattern": "^[^\\s\\u0000-\\u001f\\u007f]+$" },
            { "not": { "pattern": "^/" } },
            { "not": { "pattern": "/$" } },
            { "not": { "pattern": "//" } },
            { "not": { "pattern": "\\.\\." } }
        ],
        "description": "Advisory git branch name (no whitespace/control chars, no leading/trailing `/`, no consecutive `/`, no `..`). Enforced identically at runtime and in the JSON Schema. maxLength 128.",
    },
);

bounded_text_type!(
    /// Optional context mode override for scheduled tasks.
    ContextModeText,
    max = limits::MAX_IDENTIFIER_TEXT_CHARS,
    desc = "Scheduled task context-mode override with maxLength 128.",
);

bounded_text_type!(
    /// Optional environment profile for a Tanren dispatch.
    EnvironmentProfileText,
    max = limits::MAX_IDENTIFIER_TEXT_CHARS,
    desc = "Tanren environment profile with maxLength 128.",
);

bounded_text_type!(
    /// Model identifier text.
    ModelText,
    max = limits::MAX_MODEL_TEXT_CHARS,
    desc = "Model identifier with maxLength 256.",
);

bounded_text_type!(
    /// Token or credential text value.
    TokenText,
    max = limits::MAX_TOKEN_TEXT_CHARS,
    desc = "Token-like IPC text field with maxLength 2048.",
);

bounded_text_type!(
    /// Generic short freeform text.
    ShortText,
    max = limits::MAX_SHORT_TEXT_CHARS,
    desc = "Short IPC text field with maxLength 1024.",
);

bounded_text_type!(
    /// Schedule expression/value text.
    ScheduleValueText,
    max = limits::MAX_SCHEDULE_VALUE_TEXT_CHARS,
    desc = "Schedule value with maxLength 512.",
);

bounded_text_type!(
    /// Message-sized text value.
    MessageText,
    max = limits::MAX_MESSAGE_TEXT_CHARS,
    desc = "Message text with maxLength 32768.",
);

bounded_text_type!(
    /// Prompt/objective text value.
    PromptText,
    max = limits::MAX_PROMPT_TEXT_CHARS,
    desc = "Prompt/objective text with maxLength 32768.",
);

bounded_text_type!(
    /// Incremental streamed output chunk.
    OutputDeltaText,
    max = limits::MAX_OUTPUT_DELTA_TEXT_CHARS,
    desc = "Output delta text with maxLength 65536.",
);

bounded_text_type!(
    /// Final result text payload.
    OutputResultText,
    max = limits::MAX_OUTPUT_RESULT_TEXT_CHARS,
    desc = "Output completion result text with maxLength 262144.",
);

bounded_text_type!(
    /// Session identifier text.
    SessionIdText,
    max = limits::MAX_SESSION_ID_TEXT_CHARS,
    desc = "Session identifier text with maxLength 128.",
);

bounded_text_type!(
    /// Self-improvement scope/acceptance list item text.
    ListItemText,
    max = limits::MAX_LIST_ITEM_TEXT_CHARS,
    desc = "Self-improvement list item text with maxLength 1024.",
);
