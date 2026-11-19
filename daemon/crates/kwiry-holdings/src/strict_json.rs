//! JSON parsing that refuses duplicate object keys and non-integer numbers.
//!
//! Signed artefacts are parsed with this so that two parsers can never read
//! one signed body differently (last-key-wins against first-key-wins), and so
//! that every number on the wire is an exact integer.

use serde::Deserialize;
use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};
use std::fmt;

struct Strict(Value);

impl<'de> Deserialize<'de> for Strict {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(StrictVisitor).map(Strict)
    }
}

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("strict JSON")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Value, E> {
        Ok(Value::from(value))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Value, E> {
        Ok(Value::from(value))
    }

    fn visit_f64<E: de::Error>(self, _value: f64) -> Result<Value, E> {
        Err(E::custom("non-integer number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Value, E> {
        Ok(Value::String(value))
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(Strict(item)) = seq.next_element()? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Value, A::Error> {
        let mut map = Map::new();
        while let Some(key) = access.next_key::<String>()? {
            let Strict(value) = access.next_value()?;
            if map.insert(key.clone(), value).is_some() {
                return Err(de::Error::custom(format!("duplicate key {key:?}")));
            }
        }
        Ok(Value::Object(map))
    }
}

/// Parses `bytes` as one strict JSON value with nothing after it.
pub fn parse(bytes: &[u8]) -> Result<Value, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let Strict(value) = Strict::deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    #[test]
    fn duplicates_floats_and_trailing_data_are_refused() {
        assert!(super::parse(br#"{"a":1,"b":[1,{"c":null}]}"#).is_ok());
        assert!(super::parse(br#"{"a":1,"a":2}"#).is_err());
        assert!(super::parse(br#"{"x":{"a":1,"a":1}}"#).is_err());
        assert!(super::parse(br#"{"a":1.5}"#).is_err());
        assert!(super::parse(br#"{"a":1e3}"#).is_err());
        assert!(super::parse(br#"{"a":1} {}"#).is_err());
    }
}
