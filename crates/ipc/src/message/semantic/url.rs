use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::message::limits;

/// Error returned when a URL field is invalid.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AbsoluteHttpUrlError {
    /// URL exceeded max allowed length.
    #[error("URL length {actual} exceeds maximum {max}")]
    TooLong {
        /// Maximum allowed character length.
        max: usize,
        /// Actual character length seen.
        actual: usize,
    },
    /// URL failed parsing.
    #[error("invalid URL: {value}")]
    InvalidUrl {
        /// The invalid URL value.
        value: String,
    },
    /// URL used a non-http(s) scheme.
    #[error("URL must use http or https scheme")]
    UnsupportedScheme,
    /// URL must be absolute with host.
    #[error("URL must be absolute with host")]
    NotAbsolute,
}

/// Absolute HTTP(S) URL used by provider proxy config.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct AbsoluteHttpUrl(String);

impl AbsoluteHttpUrl {
    /// Maximum allowed length in Unicode scalar values.
    pub const MAX_LEN: usize = limits::MAX_ABSOLUTE_HTTP_URL_CHARS;

    /// Construct a validated absolute HTTP(S) URL.
    pub fn new(value: impl Into<String>) -> Result<Self, AbsoluteHttpUrlError> {
        let value = value.into();
        let actual = value.chars().count();
        if actual > Self::MAX_LEN {
            return Err(AbsoluteHttpUrlError::TooLong {
                max: Self::MAX_LEN,
                actual,
            });
        }
        let parsed = url::Url::parse(&value).map_err(|_| AbsoluteHttpUrlError::InvalidUrl {
            value: value.clone(),
        })?;
        let scheme = parsed.scheme();
        if scheme != "http" && scheme != "https" {
            return Err(AbsoluteHttpUrlError::UnsupportedScheme);
        }
        if parsed.host_str().is_none() {
            return Err(AbsoluteHttpUrlError::NotAbsolute);
        }
        Ok(Self(value))
    }

    /// Access the wire value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for AbsoluteHttpUrl {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for AbsoluteHttpUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for AbsoluteHttpUrl {
    type Err = AbsoluteHttpUrlError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<String> for AbsoluteHttpUrl {
    type Error = AbsoluteHttpUrlError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<&str> for AbsoluteHttpUrl {
    type Error = AbsoluteHttpUrlError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for AbsoluteHttpUrl {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(feature = "json-schema")]
impl schemars::JsonSchema for AbsoluteHttpUrl {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        "AbsoluteHttpUrl".into()
    }

    fn json_schema(_gen: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "format": "uri",
            "maxLength": Self::MAX_LEN,
            "pattern": "^https?://",
            "description": "Absolute HTTP(S) URL with maxLength 2048."
        })
    }
}
