//! RFC 8785 canonical JSON for the integer-only values this wire carries,
//! and the SHA-256 forms holder-local digests use.

use serde_json::Value;
use sha2::{Digest, Sha256};

pub fn canonical(value: &Value) -> String {
    let mut out = String::new();
    write(value, &mut out);
    out
}

fn write(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        // Every number on this wire is an integer (`strict_json` refuses
        // anything else), so its decimal form is already canonical.
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => out.push_str(&serde_json::to_string(s).unwrap_or_default()),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key).unwrap_or_default());
                out.push(':');
                write(&map[key], out);
            }
            out.push('}');
        }
    }
}

/// `sha256:` followed by the lowercase hex SHA-256 of `bytes`: the form of
/// `report_digest` (v2 §4.3) and of report evidence (§4.1).
pub fn sha256_tagged(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(71);
    out.push_str("sha256:");
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// SHA-256 over `label ‖ 0x00 ‖ JCS(value)`: a domain-separated digest.
pub fn labelled_digest(label: &str, value: &Value) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(label.as_bytes());
    hasher.update([0]);
    hasher.update(canonical(value).as_bytes());
    hasher.finalize().into()
}

/// Whether `text` is `sha256:` and 64 lowercase hex digits.
pub fn is_sha256_tagged(text: &str) -> bool {
    text.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn sorts_keys_by_utf16_and_escapes_like_ecmascript() {
        let value = serde_json::json!({"b": [1, "x\u{1f}\n"], "a": null, "\u{e9}": true, "Z": -3});
        assert_eq!(
            super::canonical(&value),
            "{\"Z\":-3,\"a\":null,\"b\":[1,\"x\\u001f\\n\"],\"\u{e9}\":true}"
        );
    }

    #[test]
    fn tagged_sha256_shape() {
        let tagged = super::sha256_tagged(b"abc");
        assert!(super::is_sha256_tagged(&tagged));
        assert_eq!(
            tagged,
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(!super::is_sha256_tagged("sha256:ABC"));
    }
}
