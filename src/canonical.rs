//! The canonical form of a JSON value, and its digest.
//!
//! The canonical form is compact JSON with object keys sorted at every level
//! and arrays in their own order. The digest is SHA-256 over those bytes,
//! written `sha256:<hex>`. Any reader, in any language, gets the same digest
//! from the same value, so it can check a seal it did not write
//! (`crate::seal`). `tests/canonical_digests.rs` pins the bytes and the
//! digests for a set of values.

use serde_json::Value;
use sha2::{Digest, Sha256};

/// The canonical bytes of `value`, as a string.
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(&mut out, value);
    out
}

fn write_canonical(out: &mut String, value: &Value) {
    match value {
        Value::Object(map) => {
            out.push('{');
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                write_canonical(out, &map[key]);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(out, item);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

/// Whether `value` has the shape of a digest: `sha256:` and 64 lowercase hex
/// digits.
pub fn is_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

/// The digest of bytes that are already canonical.
pub fn digest_json(json: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(json.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

/// The digest of `value`'s canonical form.
pub fn digest_value(value: &Value) -> String {
    digest_json(&canonical_json(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_sorted_at_every_level_and_arrays_keep_their_order() {
        let value = serde_json::json!({"b": 1, "a": {"y": true, "x": [3, 1, 2]}, "c": "hi"});
        assert_eq!(
            canonical_json(&value),
            r#"{"a":{"x":[3,1,2],"y":true},"b":1,"c":"hi"}"#
        );
        assert_eq!(digest_value(&value), digest_json(&canonical_json(&value)));
    }

    #[test]
    fn a_digest_is_sha256_and_64_lowercase_hex_digits() {
        assert!(is_sha256(&digest_json("{}")));
        assert!(!is_sha256("sha256:ABC"));
        assert!(!is_sha256(&format!("sha256:{}", "A".repeat(64))));
        assert!(!is_sha256(&"a".repeat(64)));
    }
}
