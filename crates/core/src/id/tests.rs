use super::*;

// Compile-time safety: different ID types are not interchangeable.
// The following would fail to compile if uncommented:
//   let g: GroupId = ContainerId::from("x");
//   fn takes_group(_: GroupId) {}
//   takes_group(ContainerId::from("x"));

// ---------------------------------------------------------------------------
// Unchecked construction (From)
// ---------------------------------------------------------------------------

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

#[test]
fn deserialize_rejects_empty_string() {
    let json = "\"\"";
    let result: Result<GroupId, _> = serde_json::from_str(json);
    assert!(
        result.is_err(),
        "deserialization should reject empty strings"
    );
    let msg = result.expect_err("should fail").to_string();
    assert!(msg.contains("empty"), "error should mention empty: {msg}");
}

#[test]
fn deserialize_rejects_whitespace_only() {
    let json = "\"   \"";
    let result: Result<ContainerId, _> = serde_json::from_str(json);
    assert!(
        result.is_err(),
        "deserialization should reject whitespace-only strings"
    );
}

#[test]
fn deserialize_rejects_over_max_len() {
    let json = format!("\"{}\"", "x".repeat(129));
    let result: Result<GroupId, _> = serde_json::from_str(&json);
    assert!(result.is_err(), "deserialization should reject >128 chars");
    let msg = result.expect_err("should fail").to_string();
    assert!(
        msg.contains("128"),
        "error should mention 128-char bound: {msg}"
    );
}

// ---------------------------------------------------------------------------
// Validated construction (new)
// ---------------------------------------------------------------------------

#[test]
fn new_accepts_valid_string() {
    let id = GroupId::new("my-group").expect("should succeed");
    assert_eq!(id.as_ref(), "my-group");
}

#[test]
fn new_rejects_empty_string() {
    let err = GroupId::new("").expect_err("should fail");
    assert!(
        err.reason.contains("empty"),
        "error should mention empty: {}",
        err.reason
    );
}

#[test]
fn new_rejects_whitespace_only() {
    let err = ContainerId::new("   ").expect_err("should fail");
    assert!(
        err.reason.contains("empty"),
        "error should mention empty: {}",
        err.reason
    );
}

#[test]
fn new_rejects_tab_only() {
    let err = ProviderId::new("\t\n").expect_err("should fail");
    assert!(
        err.reason.contains("empty"),
        "error should mention empty: {}",
        err.reason
    );
}

#[test]
fn new_preserves_leading_trailing_whitespace() {
    // Non-empty strings with whitespace are valid (trimming is caller's job).
    let id = ChannelId::new("  discord  ").expect("should succeed");
    assert_eq!(id.as_ref(), "  discord  ");
}

#[test]
fn new_from_owned_string() {
    let id = GroupId::new("my-group".to_owned()).expect("should succeed");
    assert_eq!(id.as_ref(), "my-group");
}

#[test]
fn new_accepts_exactly_128_chars() {
    let value = "x".repeat(128);
    let id = GroupId::new(value.clone()).expect("should succeed");
    assert_eq!(id.as_ref(), value);
}

#[test]
fn new_rejects_over_128_chars() {
    let err = GroupId::new("x".repeat(129)).expect_err("should fail");
    assert!(
        err.reason.contains("128"),
        "error should mention 128-char bound: {}",
        err.reason
    );
}

#[test]
fn new_empty_owned_string() {
    let result = GroupId::new(String::new());
    assert!(result.is_err());
}

#[test]
fn id_error_display() {
    let err = GroupId::new("").expect_err("should fail");
    insta::assert_snapshot!(err.to_string(), @"invalid identifier: GroupId cannot be empty or whitespace-only");
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

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
        fn container_id_roundtrips(s in ".*") {
            let id = ContainerId::from(s.clone());
            prop_assert_eq!(id.as_ref(), s.as_str());
        }

        #[test]
        fn provider_id_roundtrips(s in ".*") {
            let id = ProviderId::from(s.clone());
            prop_assert_eq!(id.as_ref(), s.as_str());
        }

        #[test]
        fn channel_id_roundtrips(s in ".*") {
            let id = ChannelId::from(s.clone());
            prop_assert_eq!(id.as_ref(), s.as_str());
        }

        #[test]
        fn job_id_roundtrips(s in ".*") {
            let id = JobId::from(s.clone());
            prop_assert_eq!(id.as_ref(), s.as_str());
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

        #[test]
        fn validated_rejects_empty_and_whitespace(s in r"\s*") {
            // All whitespace-only strings should be rejected.
            prop_assert!(GroupId::new(s).is_err());
        }

        #[test]
        fn validated_accepts_non_whitespace_within_max_len(
            s in proptest::string::string_regex(r"(?s).{1,128}").expect("valid regex"),
        ) {
            prop_assume!(s.chars().any(|c| !c.is_whitespace()));
            // Strings with at least one non-whitespace char and <=128 chars should be accepted.
            prop_assert!(GroupId::new(s).is_ok());
        }
    }
}
