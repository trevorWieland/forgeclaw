use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize};

use crate::message::limits;

const RESERVED_EXTENSION_KEYS: &[&str] = &["version"];

pub(crate) const MAX_GROUP_EXTENSIONS_PROPERTIES: usize = 32;
const MAX_GROUP_EXTENSIONS_KEY_CHARS: usize = limits::MAX_IDENTIFIER_TEXT_CHARS;
const MAX_GROUP_EXTENSIONS_NESTED_DEPTH: usize = 8;
const MAX_GROUP_EXTENSIONS_ENCODED_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GroupExtensionsVersionError {
    #[error("extensions.version must contain at least one non-whitespace character")]
    EmptyOrWhitespace,
    #[error("extensions.version length {actual} exceeds maximum {max}")]
    TooLong { max: usize, actual: usize },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GroupExtensionsError {
    #[error("extensions key `{0}` is reserved")]
    ReservedKey(String),
    #[error("duplicate extensions key `{0}`")]
    DuplicateKey(String),
    #[error("extensions key `{key}` length {actual} exceeds maximum {max}")]
    KeyTooLong {
        key: String,
        max: usize,
        actual: usize,
    },
    #[error("extensions object has {actual} properties but maximum is {max}")]
    TooManyProperties { max: usize, actual: usize },
    #[error("extensions value depth {actual} exceeds maximum {max}")]
    MaxDepthExceeded { max: usize, actual: usize },
    #[error("extensions encoded size {actual} exceeds maximum {max}")]
    EncodedBytesExceeded { max: usize, actual: usize },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct GroupExtensionsVersion(String);

impl GroupExtensionsVersion {
    /// Construct a validated extension version string.
    pub fn new(version: impl Into<String>) -> Result<Self, GroupExtensionsVersionError> {
        let version = version.into();
        validate_version(&version)?;
        Ok(Self(version))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl AsRef<str> for GroupExtensionsVersion {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl<'de> Deserialize<'de> for GroupExtensionsVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(feature = "json-schema")]
impl schemars::JsonSchema for GroupExtensionsVersion {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        "GroupExtensionsVersion".into()
    }

    fn json_schema(_gen: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": limits::MAX_IDENTIFIER_TEXT_CHARS,
            "pattern": "\\S",
            "description": "Schema version for extension compatibility (non-empty, maxLength 128, e.g. `\"1\"`)."
        })
    }
}

/// Typed extension envelope for group registration.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[cfg_attr(
    feature = "json-schema",
    schemars(schema_with = "group_extensions_schema")
)]
pub struct GroupExtensions {
    version: GroupExtensionsVersion,
    data: serde_json::Map<String, serde_json::Value>,
}

impl GroupExtensions {
    #[must_use]
    pub fn new(version: GroupExtensionsVersion) -> Self {
        Self {
            version,
            data: serde_json::Map::new(),
        }
    }

    /// Construct extensions from explicit version + data.
    pub fn with_data(
        version: GroupExtensionsVersion,
        data: serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, GroupExtensionsError> {
        validate_extension_data_invariants(&data)?;
        validate_extension_encoded_bytes(&version, &data)?;
        Ok(Self { version, data })
    }

    #[must_use]
    pub fn version(&self) -> &GroupExtensionsVersion {
        &self.version
    }

    #[must_use]
    pub fn data(&self) -> &serde_json::Map<String, serde_json::Value> {
        &self.data
    }

    /// Insert one extension key/value.
    pub fn insert(
        &mut self,
        key: impl Into<String>,
        value: serde_json::Value,
    ) -> Result<Option<serde_json::Value>, GroupExtensionsError> {
        let key = key.into();
        reject_reserved_extension_key(&key)?;

        let previous = self.data.insert(key.clone(), value);
        if let Err(err) = validate_extension_data_invariants(&self.data)
            .and_then(|()| validate_extension_encoded_bytes(&self.version, &self.data))
        {
            match previous {
                Some(old) => {
                    self.data.insert(key, old);
                }
                None => {
                    self.data.remove(&key);
                }
            }
            return Err(err);
        }
        Ok(previous)
    }

    pub fn remove(&mut self, key: &str) -> Option<serde_json::Value> {
        if Self::is_reserved_key(key) {
            return None;
        }
        self.data.remove(key)
    }

    pub(crate) fn validate_wire_invariants(&self) -> Result<(), GroupExtensionsError> {
        validate_extension_data_invariants(&self.data)?;
        validate_extension_encoded_bytes(&self.version, &self.data)
    }

    pub(crate) fn is_reserved_key(key: &str) -> bool {
        RESERVED_EXTENSION_KEYS.contains(&key)
    }
}

impl Serialize for GroupExtensions {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.validate_wire_invariants()
            .map_err(serde::ser::Error::custom)?;
        let mut map = serializer.serialize_map(Some(self.data.len() + 1))?;
        map.serialize_entry("version", &self.version)?;
        for (key, value) in &self.data {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for GroupExtensions {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct GroupExtensionsVisitor;

        impl<'de> serde::de::Visitor<'de> for GroupExtensionsVisitor {
            type Value = GroupExtensions;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an object with required `version` and extension fields")
            }

            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: serde::de::MapAccess<'de>,
            {
                let mut version: Option<GroupExtensionsVersion> = None;
                let mut data = serde_json::Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if key == "version" {
                        if version.is_some() {
                            return Err(serde::de::Error::duplicate_field("version"));
                        }
                        version = Some(map.next_value::<GroupExtensionsVersion>()?);
                        continue;
                    }
                    reject_reserved_extension_key(&key).map_err(serde::de::Error::custom)?;
                    let value = map.next_value::<serde_json::Value>()?;
                    if data.insert(key.clone(), value).is_some() {
                        return Err(serde::de::Error::custom(
                            GroupExtensionsError::DuplicateKey(key),
                        ));
                    }
                }
                let version = version.ok_or_else(|| serde::de::Error::missing_field("version"))?;
                GroupExtensions::with_data(version, data).map_err(serde::de::Error::custom)
            }
        }

        deserializer.deserialize_map(GroupExtensionsVisitor)
    }
}

fn validate_version(version: &str) -> Result<(), GroupExtensionsVersionError> {
    if version.trim().is_empty() {
        return Err(GroupExtensionsVersionError::EmptyOrWhitespace);
    }
    let actual = version.chars().count();
    if actual > limits::MAX_IDENTIFIER_TEXT_CHARS {
        return Err(GroupExtensionsVersionError::TooLong {
            max: limits::MAX_IDENTIFIER_TEXT_CHARS,
            actual,
        });
    }
    Ok(())
}

fn reject_reserved_extension_key(key: &str) -> Result<(), GroupExtensionsError> {
    if GroupExtensions::is_reserved_key(key) {
        return Err(GroupExtensionsError::ReservedKey(key.to_owned()));
    }
    Ok(())
}

fn validate_extension_key_length(key: &str) -> Result<(), GroupExtensionsError> {
    let actual = key.chars().count();
    if actual > MAX_GROUP_EXTENSIONS_KEY_CHARS {
        return Err(GroupExtensionsError::KeyTooLong {
            key: key.to_owned(),
            max: MAX_GROUP_EXTENSIONS_KEY_CHARS,
            actual,
        });
    }
    Ok(())
}

fn validate_json_value_depth_and_keys(
    value: &serde_json::Value,
    depth: usize,
) -> Result<(), GroupExtensionsError> {
    if depth > MAX_GROUP_EXTENSIONS_NESTED_DEPTH {
        return Err(GroupExtensionsError::MaxDepthExceeded {
            max: MAX_GROUP_EXTENSIONS_NESTED_DEPTH,
            actual: depth,
        });
    }
    match value {
        serde_json::Value::Object(map) => {
            for (key, nested) in map {
                validate_extension_key_length(key)?;
                validate_json_value_depth_and_keys(nested, depth + 1)?;
            }
        }
        serde_json::Value::Array(values) => {
            for nested in values {
                validate_json_value_depth_and_keys(nested, depth + 1)?;
            }
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => {}
    }
    Ok(())
}

fn validate_extension_data_invariants(
    data: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), GroupExtensionsError> {
    let actual_properties = data.len().saturating_add(RESERVED_EXTENSION_KEYS.len());
    if actual_properties > MAX_GROUP_EXTENSIONS_PROPERTIES {
        return Err(GroupExtensionsError::TooManyProperties {
            max: MAX_GROUP_EXTENSIONS_PROPERTIES,
            actual: actual_properties,
        });
    }
    for (key, value) in data {
        reject_reserved_extension_key(key)?;
        validate_extension_key_length(key)?;
        validate_json_value_depth_and_keys(value, 1)?;
    }
    Ok(())
}

fn validate_extension_encoded_bytes(
    version: &GroupExtensionsVersion,
    data: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), GroupExtensionsError> {
    let mut wire = serde_json::Map::with_capacity(data.len() + 1);
    wire.insert(
        "version".to_owned(),
        serde_json::Value::String(version.as_str().to_owned()),
    );
    for (key, value) in data {
        wire.insert(key.clone(), value.clone());
    }
    let actual = serde_json::to_vec(&wire).map_or(usize::MAX, |bytes| bytes.len());
    if actual > MAX_GROUP_EXTENSIONS_ENCODED_BYTES {
        return Err(GroupExtensionsError::EncodedBytesExceeded {
            max: MAX_GROUP_EXTENSIONS_ENCODED_BYTES,
            actual,
        });
    }
    Ok(())
}

#[cfg(feature = "json-schema")]
fn extensions_version_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    <GroupExtensionsVersion as schemars::JsonSchema>::json_schema(generator)
}

#[cfg(feature = "json-schema")]
fn bounded_extension_value_schema(depth: usize) -> schemars::Schema {
    if depth == 1 {
        return schemars::json_schema!({
            "type": ["string", "number", "boolean", "null"]
        });
    }
    let child = bounded_extension_value_schema(depth - 1);
    schemars::json_schema!({
        "oneOf": [
            { "type": "string" },
            { "type": "number" },
            { "type": "boolean" },
            { "type": "null" },
            { "type": "array", "items": child.clone() },
            {
                "type": "object",
                "propertyNames": {
                    "type": "string",
                    "maxLength": MAX_GROUP_EXTENSIONS_KEY_CHARS
                },
                "additionalProperties": child
            }
        ]
    })
}

#[cfg(feature = "json-schema")]
pub(super) fn group_extensions_schema(
    generator: &mut schemars::SchemaGenerator,
) -> schemars::Schema {
    let _ = generator;
    schemars::json_schema!({
        "type": "object",
        "properties": {
            "version": extensions_version_schema(generator),
        },
        "required": ["version"],
        "maxProperties": MAX_GROUP_EXTENSIONS_PROPERTIES,
        "propertyNames": {
            "type": "string",
            "maxLength": MAX_GROUP_EXTENSIONS_KEY_CHARS
        },
        "additionalProperties": bounded_extension_value_schema(MAX_GROUP_EXTENSIONS_NESTED_DEPTH),
        "description": "Extension envelope with bounded key/value complexity. Top-level maxProperties includes the required `version` key."
    })
}
