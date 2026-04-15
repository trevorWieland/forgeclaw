//! Identity newtypes for compile-time type safety.
//!
//! Each ID is a newtype wrapper around `String` that prevents accidentally
//! passing one kind of identifier where another is expected.
//!
//! ## Construction
//!
//! IDs are always validated:
//! - [`GroupId::new()`] (and same for every ID type)
//! - `str::parse::<GroupId>()`
//! - [`TryFrom<String>`] / [`TryFrom<&str>`]
//!
//! Construction rejects empty/whitespace-only values and values longer
//! than 128 characters with an [`IdError`].
//!
//! Deserialization (`serde::Deserialize`) also uses the validated path —
//! empty or whitespace-only strings are rejected at parse time.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

const MAX_ID_CHARS: usize = 128;

/// Error returned when an ID string fails validation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid identifier: {reason}")]
pub struct IdError {
    /// Human-readable description of why the value was rejected.
    pub reason: String,
}

/// Generate a newtype ID wrapper around `String`.
///
/// Each generated type implements `Debug`, `Clone`, `PartialEq`, `Eq`, `Hash`,
/// `Display`, `Serialize`, `FromStr`, `TryFrom<String>`, `TryFrom<&str>`,
/// and `AsRef<str>`, plus a validated `new()` constructor and a custom
/// `Deserialize` that
/// rejects empty/whitespace values and values over 128 characters.
macro_rules! define_id {
    ($(#[doc = $doc:expr] $name:ident),+ $(,)?) => {
        $(
            #[doc = $doc]
            #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
            pub struct $name(String);

            #[cfg(feature = "json-schema")]
            impl schemars::JsonSchema for $name {
                fn inline_schema() -> bool {
                    true
                }

                fn schema_name() -> std::borrow::Cow<'static, str> {
                    stringify!($name).into()
                }

                fn json_schema(
                    _gen: &mut schemars::SchemaGenerator,
                ) -> schemars::Schema {
                    schemars::json_schema!({
                        "type": "string",
                        "minLength": 1,
                        "maxLength": MAX_ID_CHARS,
                        "pattern": "\\S",
                        "description": concat!(
                            stringify!($name),
                            " — must be non-empty, contain at least one non-whitespace character, and be at most 128 chars."
                        )
                    })
                }
            }

            impl $name {
                /// Create a validated ID, rejecting empty or whitespace-only
                /// values and values longer than 128 characters.
                ///
                /// Prefer this at ingress boundaries.
                ///
                /// # Errors
                ///
                /// Returns [`IdError`] if the value is empty, contains
                /// only whitespace, or exceeds 128 characters.
                pub fn new(s: impl Into<String>) -> Result<Self, IdError> {
                    let value = s.into();
                    if value.trim().is_empty() {
                        return Err(IdError {
                            reason: format!(
                                "{} cannot be empty or whitespace-only",
                                stringify!($name)
                            ),
                        });
                    }
                    let len = value.chars().count();
                    if len > MAX_ID_CHARS {
                        return Err(IdError {
                            reason: format!(
                                "{} cannot exceed {MAX_ID_CHARS} characters (got {len})",
                                stringify!($name)
                            ),
                        });
                    }
                    Ok(Self(value))
                }
            }

            impl fmt::Display for $name {
                fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str(&self.0)
                }
            }

            impl FromStr for $name {
                type Err = IdError;

                fn from_str(s: &str) -> Result<Self, Self::Err> {
                    Self::new(s)
                }
            }

            impl TryFrom<String> for $name {
                type Error = IdError;

                fn try_from(value: String) -> Result<Self, Self::Error> {
                    Self::new(value)
                }
            }

            impl TryFrom<&str> for $name {
                type Error = IdError;

                fn try_from(value: &str) -> Result<Self, Self::Error> {
                    Self::new(value)
                }
            }

            impl AsRef<str> for $name {
                fn as_ref(&self) -> &str {
                    &self.0
                }
            }

            impl<'de> Deserialize<'de> for $name {
                fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
                where
                    D: serde::Deserializer<'de>,
                {
                    let s = String::deserialize(deserializer)?;
                    $name::new(s).map_err(serde::de::Error::custom)
                }
            }
        )+
    };
}

define_id! {
    /// Unique identifier for a group (logical agent context).
    GroupId,
    /// Unique identifier for a container instance.
    ContainerId,
    /// Unique identifier for a provider (e.g. "anthropic", "ollama").
    ProviderId,
    /// Unique identifier for a channel (e.g. "discord", "telegram").
    ChannelId,
    /// Unique identifier for a queued job.
    JobId,
    /// Unique identifier for a scheduled task.
    TaskId,
    /// Unique identifier for a Tanren dispatch.
    DispatchId,
    /// Unique identifier for a budget pool.
    PoolId,
}

#[cfg(test)]
mod tests;
