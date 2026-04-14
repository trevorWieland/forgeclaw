//! Fast lexical probe helpers for extracting top-level `type`.

/// Fast lexical probe window for top-level `"type"` extraction.
const FAST_TYPE_PROBE_SCAN_BYTES: usize = 8 * 1024;

fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

fn parse_simple_json_string(text: &str, bytes: &[u8], i: usize) -> Option<(String, usize)> {
    if i >= bytes.len() || bytes[i] != b'"' {
        return None;
    }
    let mut idx = i + 1;
    while idx < bytes.len() {
        match bytes[idx] {
            b'"' => {
                let s = text.get((i + 1)..idx)?.to_owned();
                return Some((s, idx + 1));
            }
            b'\\' => return None,
            _ => idx += 1,
        }
    }
    None
}

/// Extracts a top-level `"type"` discriminator when it appears as the
/// first key in an object, using lexical scanning only.
pub(crate) fn probe_first_type_field(text: &str) -> Option<String> {
    let scan_len = text.len().min(FAST_TYPE_PROBE_SCAN_BYTES);
    let text = text.get(..scan_len)?;
    let bytes = text.as_bytes();

    let mut i = skip_ws(bytes, 0);
    if i >= bytes.len() || bytes[i] != b'{' {
        return None;
    }
    i += 1;
    i = skip_ws(bytes, i);
    let (key, mut i) = parse_simple_json_string(text, bytes, i)?;
    if key != "type" {
        return None;
    }
    i = skip_ws(bytes, i);
    if i >= bytes.len() || bytes[i] != b':' {
        return None;
    }
    i += 1;
    i = skip_ws(bytes, i);
    let (ty, _i) = parse_simple_json_string(text, bytes, i)?;
    Some(ty)
}

#[cfg(test)]
mod tests {
    use super::probe_first_type_field;

    #[test]
    fn extracts_type_when_first_field() {
        let raw = r#"{"type":"ready","adapter":"x"}"#;
        assert_eq!(probe_first_type_field(raw).as_deref(), Some("ready"));
    }

    #[test]
    fn returns_none_when_type_not_first() {
        let raw = r#"{"adapter":"x","type":"ready"}"#;
        assert!(probe_first_type_field(raw).is_none());
    }
}
