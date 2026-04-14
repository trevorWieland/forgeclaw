//! Identity newtypes for compile-time type safety.
//!
//! Each ID is a newtype wrapper around `String` that prevents accidentally
//! passing one kind of identifier where another is expected.
//!
//! ## Construction
//!
//! - **Validated** — [`GroupId::new()`] (and the same for every ID type)
//!   rejects empty or whitespace-only values with an [`IdError`].  Use
//!   this at ingress boundaries (config loading, API endpoints, IPC).
//!
//! - **Unchecked** — [`From<String>`] / [`From<&str>`] accept any value
//!   without validation.  Use only in internal code that has already
//!   validated the input or in tests.
//!
//! Deserialization (`serde::Deserialize`) uses the validated path — empty
//! or whitespace-only strings are rejected at parse time.

use serde::{Deserialize, Serialize};
use std::fmt;

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
/// `Display`, `Serialize`, `From<String>`, `From<&str>`, and `AsRef<str>`,
/// plus a validated `new()` constructor and a custom `Deserialize` that
/// rejects empty/whitespace values.
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
                        "pattern": "\\S",
                        "description": concat!(
                            stringify!($name),
                            " — must be non-empty and contain at least one non-whitespace character."
                        )
                    })
                }
            }

            impl $name {
                /// Create a validated ID, rejecting empty or whitespace-only
                /// values.
                ///
                /// Prefer this over `From` at ingress boundaries.
                ///
                /// # Errors
                ///
                /// Returns [`IdError`] if the value is empty or contains
                /// only whitespace.
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
                    Ok(Self(value))
                }
            }

            impl fmt::Display for $name {
                fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str(&self.0)
                }
            }

            impl From<String> for $name {
                fn from(s: String) -> Self {
                    Self(s)
                }
            }

            impl From<&str> for $name {
                fn from(s: &str) -> Self {
                    Self(s.to_owned())
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
