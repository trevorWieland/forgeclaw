use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize};

const RESERVED_EXTENSION_KEYS: &[&str] = &["version"];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GroupExtensionsVersionError {
    #[error("extensions.version must contain at least one non-whitespace character")]
    EmptyOrWhitespace,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GroupExtensionsError {
    #[error("extensions key `{0}` is reserved")]
    ReservedKey(String),
    #[error("duplicate extensions key `{0}`")]
    DuplicateKey(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct GroupExtensionsVersion(String);

impl GroupExtensionsVersion {
    /// Construct a validated extension version string.
    pub fn new(version: impl Into<String>) -> Result<Self, GroupExtensionsVersionError> {
        let version = version.into();
        if version.trim().is_empty() {
            return Err(GroupExtensionsVersionError::EmptyOrWhitespace);
        }
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
            "pattern": "\\S",
            "description": "Schema version for extension compatibility (non-empty, e.g. `\"1\"`)."
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
        validate_extension_data_keys(&data)?;
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
        Ok(self.data.insert(key, value))
    }

    pub fn remove(&mut self, key: &str) -> Option<serde_json::Value> {
        if Self::is_reserved_key(key) {
            return None;
        }
        self.data.remove(key)
    }

    pub(crate) fn validate_wire_invariants(&self) -> Result<(), GroupExtensionsError> {
        validate_extension_data_keys(&self.data)
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
                Ok(GroupExtensions { version, data })
            }
        }

        deserializer.deserialize_map(GroupExtensionsVisitor)
    }
}

fn reject_reserved_extension_key(key: &str) -> Result<(), GroupExtensionsError> {
    if GroupExtensions::is_reserved_key(key) {
        return Err(GroupExtensionsError::ReservedKey(key.to_owned()));
    }
    Ok(())
}

fn validate_extension_data_keys(
    data: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), GroupExtensionsError> {
    for key in data.keys() {
        reject_reserved_extension_key(key)?;
    }
    Ok(())
}

#[cfg(feature = "json-schema")]
fn extensions_version_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    <GroupExtensionsVersion as schemars::JsonSchema>::json_schema(generator)
}

#[cfg(feature = "json-schema")]
pub(super) fn group_extensions_schema(
    generator: &mut schemars::SchemaGenerator,
) -> schemars::Schema {
    schemars::json_schema!({
        "type": "object",
        "properties": {
            "version": extensions_version_schema(generator),
        },
        "required": ["version"],
        "additionalProperties": true,
    })
}
