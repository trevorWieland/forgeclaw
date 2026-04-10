//! TOML value deep-merge for layered configuration loading.

use toml::Value;

/// Deep-merge `overlay` into `base`, returning the merged result.
///
/// Tables are merged recursively: keys in `overlay` take precedence.
/// Non-table values in `overlay` replace those in `base`.
pub(crate) fn deep_merge(base: Value, overlay: Value) -> Value {
    match (base, overlay) {
        (Value::Table(mut base_table), Value::Table(overlay_table)) => {
            for (key, overlay_val) in overlay_table {
                let merged = if let Some(base_val) = base_table.remove(&key) {
                    deep_merge(base_val, overlay_val)
                } else {
                    overlay_val
                };
                base_table.insert(key, merged);
            }
            Value::Table(base_table)
        }
        // Non-table values: overlay wins.
        (_base, overlay) => overlay,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_disjoint_tables() {
        let base: Value = toml::from_str("[a]\nx = 1").expect("parse base");
        let overlay: Value = toml::from_str("[b]\ny = 2").expect("parse overlay");
        let merged = deep_merge(base, overlay);
        let table = merged.as_table().expect("should be table");
        assert!(table.contains_key("a"));
        assert!(table.contains_key("b"));
    }

    #[test]
    fn overlay_overrides_scalar() {
        let base: Value = toml::from_str("[a]\nx = 1").expect("parse base");
        let overlay: Value = toml::from_str("[a]\nx = 2").expect("parse overlay");
        let merged = deep_merge(base, overlay);
        let x = merged
            .get("a")
            .and_then(|a| a.get("x"))
            .and_then(Value::as_integer)
            .expect("should have a.x");
        assert_eq!(x, 2);
    }

    #[test]
    fn nested_tables_merge_recursively() {
        let base: Value = toml::from_str("[a]\nx = 1\n[a.nested]\nfoo = true").expect("parse base");
        let overlay: Value =
            toml::from_str("[a]\ny = 2\n[a.nested]\nbar = false").expect("parse overlay");
        let merged = deep_merge(base, overlay);
        let a = merged.get("a").expect("should have a");
        // Both x and y should exist.
        assert!(a.get("x").is_some());
        assert!(a.get("y").is_some());
        // Nested should have both foo and bar.
        let nested = a.get("nested").expect("should have nested");
        assert!(nested.get("foo").is_some());
        assert!(nested.get("bar").is_some());
    }
}
