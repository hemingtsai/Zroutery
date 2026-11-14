//! Bounded JSON decoding before allocating a DTO. Duplicate/dangerous keys fail.
use serde::de::{DeserializeOwned, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::{PluginError, Result};

pub const MAX_FRAME_BYTES: usize = 256 * 1024;
pub const MAX_DEPTH: usize = 32;
pub const MAX_ITEMS: usize = 4096;
pub const MAX_STRING_BYTES: usize = 16 * 1024;

struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct StrictVisitor;
        impl<'de> Visitor<'de> for StrictVisitor {
            type Value = StrictValue;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("bounded JSON without duplicate or prototype keys")
            }
            fn visit_bool<E: serde::de::Error>(
                self,
                v: bool,
            ) -> std::result::Result<Self::Value, E> {
                Ok(StrictValue(Value::Bool(v)))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<Self::Value, E> {
                Ok(StrictValue(v.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<Self::Value, E> {
                Ok(StrictValue(v.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<Self::Value, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| StrictValue(Value::Number(n)))
                    .ok_or_else(|| E::custom("non-finite number"))
            }
            fn visit_str<E: serde::de::Error>(
                self,
                v: &str,
            ) -> std::result::Result<Self::Value, E> {
                if v.len() > MAX_FRAME_BYTES {
                    return Err(E::custom("string quota"));
                }
                Ok(StrictValue(Value::String(v.into())))
            }
            fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
                Ok(StrictValue(Value::Null))
            }
            fn visit_none<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
                self.visit_unit()
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = seq.next_element::<StrictValue>()? {
                    if values.len() >= MAX_ITEMS {
                        return Err(serde::de::Error::custom("collection quota"));
                    }
                    values.push(value.0);
                }
                Ok(StrictValue(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if values.len() >= MAX_ITEMS
                        || key.len() > 128
                        || matches!(key.as_str(), "__proto__" | "prototype" | "constructor")
                        || values.contains_key(&key)
                    {
                        return Err(serde::de::Error::custom("invalid object key"));
                    }
                    values.insert(key, map.next_value::<StrictValue>()?.0);
                }
                Ok(StrictValue(Value::Object(values)))
            }
        }
        d.deserialize_any(StrictVisitor)
    }
}

pub fn decode<T: DeserializeOwned>(bytes: &[u8], limit: usize) -> Result<T> {
    if bytes.len() > limit {
        return Err(PluginError::InvalidResult);
    }
    // Preflight counts nesting outside strings before serde's recursion/allocations.
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    for &byte in bytes {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else {
            match byte {
                b'"' => quoted = true,
                b'{' | b'[' => {
                    depth += 1;
                    if depth > MAX_DEPTH {
                        return Err(PluginError::InvalidResult);
                    }
                }
                b'}' | b']' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
    }
    let value: StrictValue =
        serde_json::from_slice(bytes).map_err(|_| PluginError::InvalidResult)?;
    serde_json::from_value(value.0).map_err(|_| PluginError::InvalidResult)
}

/// Same strict preflight, with package-resource strings permitted up to the
/// separately bounded file quota. Control DTOs still validate their text fields.
pub fn decode_package<T: DeserializeOwned>(bytes: &[u8], limit: usize) -> Result<T> {
    decode(bytes, limit)
}

pub fn text(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(|c| c.is_control() && c != '\n')
}

pub fn local_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
}

pub fn nonempty_ids(values: &[String]) -> bool {
    !values.is_empty()
        && values.len() <= 64
        && values.iter().all(|v| local_id(v))
        && values
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            == values.len()
}

/// Portable archive/module path, including Windows aliases even on Unix.
pub fn package_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 240
        && !path.starts_with('/')
        && !path.contains(['\\', ':', '\0'])
        && path.split('/').all(|part| {
            if part.is_empty() || matches!(part, "." | "..") || part.ends_with(['.', ' ']) {
                return false;
            }
            let stem = part
                .split('.')
                .next()
                .unwrap_or_default()
                .to_ascii_uppercase();
            !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$")
                && !(stem.len() == 4
                    && (stem.starts_with("COM") || stem.starts_with("LPT"))
                    && matches!(stem.as_bytes()[3], b'1'..=b'9'))
                && part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_duplicate_prototype_deep_and_oversize_json() {
        for bytes in [
            br#"{"x":1,"x":2}"#.as_slice(),
            br#"{"__proto__":{}}"#,
            br#"{"constructor":1}"#,
        ] {
            assert!(decode::<Value>(bytes, MAX_FRAME_BYTES).is_err());
        }
        let deep = format!("{}0{}", "[".repeat(33), "]".repeat(33));
        assert!(decode::<Value>(deep.as_bytes(), MAX_FRAME_BYTES).is_err());
        assert!(decode::<Value>(b"{}", 1).is_err());
    }
    #[test]
    fn windows_aliases_and_traversal_are_not_portable_package_paths() {
        for path in [
            "../a", "/a", "C:/a", "a:stream", "a\\b", "CON.mjs", "a./b", "a//b", "LPT1.txt",
        ] {
            assert!(!package_path(path), "{path}");
        }
        assert!(package_path("dist/ui.mjs"));
    }
}
