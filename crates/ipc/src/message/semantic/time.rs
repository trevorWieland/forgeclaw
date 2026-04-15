use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use chrono::DateTime;
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

/// Error returned when an IPC timestamp string is not valid RFC3339.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid RFC3339 timestamp: {value}")]
pub struct TimestampError {
    /// The invalid value that was rejected.
    pub value: String,
}

/// Error returned when a timezone is not a valid IANA zone name.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid IANA timezone: {value}")]
pub struct TimezoneError {
    /// The invalid value that was rejected.
    pub value: String,
}

/// RFC3339 timestamp used by IPC message payloads.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct IpcTimestamp(String);

impl IpcTimestamp {
    /// Create a validated IPC timestamp.
    pub fn new(value: impl Into<String>) -> Result<Self, TimestampError> {
        let value = value.into();
        if DateTime::parse_from_rfc3339(&value).is_err() {
            return Err(TimestampError { value });
        }
        Ok(Self(value))
    }

    /// Access the wire value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for IpcTimestamp {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for IpcTimestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for IpcTimestamp {
    type Err = TimestampError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<String> for IpcTimestamp {
    type Error = TimestampError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<&str> for IpcTimestamp {
    type Error = TimestampError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for IpcTimestamp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(feature = "json-schema")]
impl schemars::JsonSchema for IpcTimestamp {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        "IpcTimestamp".into()
    }

    fn json_schema(_gen: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "format": "date-time",
            "pattern": "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{1,9})?(?:Z|[+-]\\d{2}:\\d{2})$",
            "description": "RFC3339 timestamp used by the IPC wire contract."
        })
    }
}

/// IANA timezone identifier used by IPC message payloads.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct IanaTimezone(String);

impl IanaTimezone {
    /// Create a validated IANA timezone.
    pub fn new(value: impl Into<String>) -> Result<Self, TimezoneError> {
        let value = value.into();
        if Tz::from_str(&value).is_err() {
            return Err(TimezoneError { value });
        }
        Ok(Self(value))
    }

    /// Access the wire value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for IanaTimezone {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for IanaTimezone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for IanaTimezone {
    type Err = TimezoneError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<String> for IanaTimezone {
    type Error = TimezoneError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<&str> for IanaTimezone {
    type Error = TimezoneError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for IanaTimezone {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(feature = "json-schema")]
impl schemars::JsonSchema for IanaTimezone {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        "IanaTimezone".into()
    }

    fn json_schema(_gen: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let values: Vec<String> = chrono_tz::TZ_VARIANTS
            .iter()
            .map(ToString::to_string)
            .collect();
        schemars::json_schema!({
            "type": "string",
            "enum": values,
            "description": "IANA timezone name validated at runtime (e.g. America/New_York, UTC)."
        })
    }
}
