//! Bounded collection types for wire-level list constraints.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use crate::message::limits;

use super::semantic::ListItemText;
use super::shared::HistoricalMessage;

/// Error returned when a bounded collection exceeds its max length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("collection length {actual} exceeds maximum {max}")]
pub struct BoundedCollectionError {
    /// Maximum allowed number of entries.
    pub max: usize,
    /// Actual number of entries supplied.
    pub actual: usize,
}

macro_rules! bounded_collection_type {
    (
        $(#[$meta:meta])*
        $name:ident,
        item = $item:ty,
        max = $max:expr,
        desc = $desc:literal
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
        #[serde(transparent)]
        pub struct $name(Vec<$item>);

        impl $name {
            /// Maximum allowed number of entries.
            pub const MAX_LEN: usize = $max;

            /// Construct a validated bounded collection.
            pub fn new(values: Vec<$item>) -> Result<Self, BoundedCollectionError> {
                let actual = values.len();
                if actual > Self::MAX_LEN {
                    return Err(BoundedCollectionError {
                        max: Self::MAX_LEN,
                        actual,
                    });
                }
                Ok(Self(values))
            }

            /// Number of items in the collection.
            #[must_use]
            pub fn len(&self) -> usize {
                self.0.len()
            }

            /// Returns `true` when empty.
            #[must_use]
            pub fn is_empty(&self) -> bool {
                self.0.is_empty()
            }

            /// Borrow as a slice.
            #[must_use]
            pub fn as_slice(&self) -> &[$item] {
                &self.0
            }

            /// Iterate over items.
            pub fn iter(&self) -> std::slice::Iter<'_, $item> {
                self.0.iter()
            }

            /// Consume into the underlying vector.
            #[must_use]
            pub fn into_vec(self) -> Vec<$item> {
                self.0
            }
        }

        impl AsRef<[$item]> for $name {
            fn as_ref(&self) -> &[$item] {
                self.as_slice()
            }
        }

        impl std::ops::Deref for $name {
            type Target = [$item];

            fn deref(&self) -> &Self::Target {
                self.as_slice()
            }
        }

        impl IntoIterator for $name {
            type Item = $item;
            type IntoIter = std::vec::IntoIter<$item>;

            fn into_iter(self) -> Self::IntoIter {
                self.0.into_iter()
            }
        }

        impl<'a> IntoIterator for &'a $name {
            type Item = &'a $item;
            type IntoIter = std::slice::Iter<'a, $item>;

            fn into_iter(self) -> Self::IntoIter {
                self.iter()
            }
        }

        impl From<$name> for Vec<$item> {
            fn from(value: $name) -> Self {
                value.into_vec()
            }
        }

        impl TryFrom<Vec<$item>> for $name {
            type Error = BoundedCollectionError;

            fn try_from(value: Vec<$item>) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let values = Vec::<$item>::deserialize(deserializer)?;
                Self::new(values).map_err(serde::de::Error::custom)
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

            fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
                let item = generator.subschema_for::<$item>();
                schemars::json_schema!({
                    "type": "array",
                    "maxItems": Self::MAX_LEN,
                    "items": item,
                    "description": $desc
                })
            }
        }
    };
}

bounded_collection_type!(
    /// Historical message list with max 256 entries.
    HistoricalMessages,
    item = HistoricalMessage,
    max = limits::MAX_HISTORICAL_MESSAGES,
    desc = "Historical message list with maxItems 256."
);

bounded_collection_type!(
    /// Self-improvement list items with max 64 entries.
    SelfImprovementListItems,
    item = ListItemText,
    max = limits::MAX_SELF_IMPROVEMENT_LIST_ITEMS,
    desc = "Self-improvement list items with maxItems 64."
);

#[cfg(test)]
mod tests {
    use super::{HistoricalMessages, SelfImprovementListItems};
    use crate::message::shared::HistoricalMessage;

    fn sample_historical_message() -> HistoricalMessage {
        HistoricalMessage {
            sender: "A".parse().expect("valid sender"),
            text: "x".parse().expect("valid text"),
            timestamp: "2026-04-03T10:00:00Z".parse().expect("valid timestamp"),
        }
    }

    #[test]
    fn historical_messages_reject_over_256() {
        let msgs = std::iter::repeat_n(sample_historical_message(), 257).collect::<Vec<_>>();
        let err = HistoricalMessages::new(msgs).expect_err("must reject >256");
        assert_eq!(err.max, 256);
        assert_eq!(err.actual, 257);
    }

    #[test]
    fn self_improvement_list_items_reject_over_64() {
        let items = std::iter::repeat_n("scope".parse().expect("valid item"), 65).collect();
        let err = SelfImprovementListItems::new(items).expect_err("must reject >64");
        assert_eq!(err.max, 64);
        assert_eq!(err.actual, 65);
    }

    #[test]
    fn bounded_collections_roundtrip_at_boundaries() {
        let max_messages = HistoricalMessages::new(
            std::iter::repeat_n(sample_historical_message(), HistoricalMessages::MAX_LEN).collect(),
        )
        .expect("max messages valid");
        let msg_json = serde_json::to_value(&max_messages).expect("serialize");
        let back_messages: HistoricalMessages =
            serde_json::from_value(msg_json).expect("deserialize");
        assert_eq!(back_messages.len(), HistoricalMessages::MAX_LEN);

        let max_items = SelfImprovementListItems::new(
            std::iter::repeat_n(
                "scope".parse().expect("valid item"),
                SelfImprovementListItems::MAX_LEN,
            )
            .collect(),
        )
        .expect("max list items valid");
        let item_json = serde_json::to_value(&max_items).expect("serialize");
        let back_items: SelfImprovementListItems =
            serde_json::from_value(item_json).expect("deserialize");
        assert_eq!(back_items.len(), SelfImprovementListItems::MAX_LEN);
    }
}
