//! Identity newtypes for compile-time type safety.
//!
//! Each ID is a newtype wrapper around `String` that prevents accidentally
//! passing one kind of identifier where another is expected.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Generate a newtype ID wrapper around `String`.
///
/// Each generated type implements `Debug`, `Clone`, `PartialEq`, `Eq`, `Hash`,
/// `Display`, `Serialize`, `Deserialize`, `From<String>`, `From<&str>`, and
/// `AsRef<str>`.
macro_rules! define_id {
    ($(#[doc = $doc:expr] $name:ident),+ $(,)?) => {
        $(
            #[doc = $doc]
            #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
            pub struct $name(String);

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
mod tests {
    use super::*;

    // Compile-time safety: different ID types are not interchangeable.
    // The following would fail to compile if uncommented:
    //   let g: GroupId = ContainerId::from("x");
    //   fn takes_group(_: GroupId) {}
    //   takes_group(ContainerId::from("x"));

    #[test]
    fn display_shows_inner_string() {
        let id = GroupId::from("my-group");
        assert_eq!(id.to_string(), "my-group");
    }

    #[test]
    fn from_string_and_as_ref_roundtrip() {
        let original = "test-id-123".to_owned();
        let id = ContainerId::from(original.clone());
        assert_eq!(id.as_ref(), original);
    }

    #[test]
    fn from_str_ref() {
        let id = ProviderId::from("anthropic");
        assert_eq!(id.as_ref(), "anthropic");
    }

    #[test]
    fn equality_and_hash() {
        use std::collections::HashSet;

        let a = ChannelId::from("discord");
        let b = ChannelId::from("discord");
        let c = ChannelId::from("telegram");
        assert_eq!(a, b);
        assert_ne!(a, c);

        let mut set = HashSet::new();
        set.insert(a);
        assert!(set.contains(&b));
        assert!(!set.contains(&c));
    }

    #[test]
    fn serde_roundtrip() {
        let id = JobId::from("job-42");
        let json = serde_json::to_string(&id).expect("serialize");
        let back: JobId = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(id, back);
    }

    mod proptest_ids {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn group_id_roundtrips(s in ".*") {
                let id = GroupId::from(s.clone());
                prop_assert_eq!(id.as_ref(), s.as_str());
                prop_assert_eq!(id.to_string(), s);
            }

            #[test]
            fn task_id_roundtrips(s in ".*") {
                let id = TaskId::from(s.clone());
                prop_assert_eq!(id.as_ref(), s.as_str());
            }

            #[test]
            fn dispatch_id_roundtrips(s in ".*") {
                let id = DispatchId::from(s.clone());
                prop_assert_eq!(id.as_ref(), s.as_str());
            }

            #[test]
            fn pool_id_roundtrips(s in ".*") {
                let id = PoolId::from(s.clone());
                prop_assert_eq!(id.as_ref(), s.as_str());
            }
        }
    }
}
