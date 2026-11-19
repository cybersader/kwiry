//! RFC 8785 canonical JSON for the integer-only values this wire carries.

use serde_json::Value;

pub fn canonical(value: &Value) -> String {
    let mut out = String::new();
    write(value, &mut out);
    out
}

fn write(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            assert!(n.is_i64() || n.is_u64(), "JCS here supports integers only");
            out.push_str(&n.to_string());
        }
        Value::String(s) => out.push_str(&serde_json::to_string(s).expect("string")),
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
                out.push_str(&serde_json::to_string(key).expect("key"));
                out.push(':');
                write(&map[key], out);
            }
            out.push('}');
        }
    }
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
}
