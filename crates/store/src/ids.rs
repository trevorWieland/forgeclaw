//! Identifier generation helpers.
//!
//! All persisted rows use string primary keys. Callers typically generate
//! the ID via [`generate_id`] before calling a store method, so the ID is
//! known upstream and can be used as a correlation key in the event bus.

use uuid::Uuid;

/// Generate a new UUIDv7 string suitable for use as a primary key.
///
/// UUIDv7 is time-ordered (lexicographic sort matches creation order),
/// 128-bit, and collision-resistant. This makes it a drop-in replacement
/// for ULID while staying on the standard `uuid` crate.
#[must_use]
pub fn generate_id() -> String {
    Uuid::now_v7().to_string()
}

#[cfg(test)]
mod tests {
    use super::generate_id;

    #[test]
    fn ids_are_unique() {
        let a = generate_id();
        let b = generate_id();
        assert_ne!(a, b);
    }

    #[test]
    fn ids_are_36_characters() {
        // Standard UUID canonical form: 8-4-4-4-12 = 36 chars.
        assert_eq!(generate_id().len(), 36);
    }

    #[test]
    fn ids_are_time_ordered() {
        // Sequential generation should yield lexicographically-ordered IDs.
        let a = generate_id();
        let b = generate_id();
        assert!(a <= b, "expected {a} <= {b}");
    }
}
