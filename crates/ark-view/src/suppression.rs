//! User-close suppression support. This tier owns [`ParamsHash`] and
//! the canonical hash function; T-013 adds `SceneHandleName` +
//! `SuppressionPolicy` contract doc type.
//!
//! Per cavekit-soul-phase-2-ark-view.md R8 and phase-2-design-
//! decisions.md §R-6 (algorithm locked to blake3).
//!
//! The hash is session-scoped: two reconcile ticks on the same
//! resolved scene params produce identical hashes, letting the
//! supervisor skip respawn of a user-closed pane until the author
//! materially changes the view's params.

/// 32-byte deterministic hash of a view's resolved scene params.
///
/// Computed as `blake3(canonical_json(params))`, where
/// `canonical_json` sorts object keys, drops whitespace, and emits
/// numbers in their shortest round-trippable form. The canonical-JSON
/// step is what makes cosmetic scene-source edits (reordering
/// properties, whitespace, comments) produce the same hash as the
/// original — only *material* param changes lift suppression.
///
/// ## Algorithm
///
/// 1. Serialise the params to a [`serde_json::Value`].
/// 2. Emit that value in canonical form (recursively: sort object
///    keys byte-wise, no whitespace, numbers in shortest
///    round-trippable encoding).
/// 3. `blake3::hash(canonical.as_bytes())` — take all 32 output bytes.
///
/// The algorithm is stable across ark versions within a major
/// release; crossing a major may change canonicalisation rules but
/// MUST change `CURRENT_PROTOCOL_VERSION` in lockstep.
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub struct ParamsHash([u8; 32]);

impl ParamsHash {
    /// Construct from raw 32 bytes — intended for tests and the
    /// supervisor's suppression-map storage path; callers computing
    /// a hash from params should use [`hash_params`].
    pub const fn from_bytes(bytes: [u8; 32]) -> Self { Self(bytes) }

    /// Borrow the raw 32 bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] { &self.0 }
}

impl std::fmt::Display for ParamsHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl serde::Serialize for ParamsHash {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for ParamsHash {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = <String as serde::Deserialize>::deserialize(d)?;
        if s.len() != 64 {
            return Err(serde::de::Error::custom(format!(
                "ParamsHash wire form: expected 64 hex chars, got {}",
                s.len()
            )));
        }
        let mut bytes = [0u8; 32];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            let hi = hex_nibble(chunk[0]).map_err(serde::de::Error::custom)?;
            let lo = hex_nibble(chunk[1]).map_err(serde::de::Error::custom)?;
            bytes[i] = (hi << 4) | lo;
        }
        Ok(Self(bytes))
    }
}

fn hex_nibble(b: u8) -> Result<u8, &'static str> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(10 + b - b'a'),
        b'A'..=b'F' => Ok(10 + b - b'A'),
        _ => Err("invalid hex nibble in ParamsHash"),
    }
}

/// Compute the canonical [`ParamsHash`] for a view's resolved scene
/// params. Implements blake3(canonical_json(params)) per R8.
///
/// Accepts any `serde::Serialize` input so callers can pass a full
/// struct (the typical case) or a pre-built `serde_json::Value`.
pub fn hash_params<T: serde::Serialize>(params: &T) -> ParamsHash {
    let value = serde_json::to_value(params).expect("params serialisation must not fail");
    let canonical = canonical_json(&value);
    let hash = blake3::hash(canonical.as_bytes());
    ParamsHash(*hash.as_bytes())
}

/// Emit a [`serde_json::Value`] in canonical form: sorted object keys,
/// no whitespace, shortest round-trippable number encoding.
fn canonical_json(value: &serde_json::Value) -> String {
    let mut out = String::new();
    write_canonical(&mut out, value);
    out
}

fn write_canonical(out: &mut String, value: &serde_json::Value) {
    use serde_json::Value;
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => {
            // Delegate string escaping to serde_json (handles quotes,
            // backslashes, control chars, Unicode) by serialising a
            // one-element holder.
            out.push_str(&serde_json::to_string(s).expect("string ser"));
        }
        Value::Array(arr) => {
            out.push('[');
            for (i, item) in arr.iter().enumerate() {
                if i > 0 { out.push(','); }
                write_canonical(out, item);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 { out.push(','); }
                out.push_str(&serde_json::to_string(k.as_str()).expect("key ser"));
                out.push(':');
                write_canonical(out, &map[*k]);
            }
            out.push('}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use serde_json::{json, Map, Value};

    #[derive(Serialize)]
    struct Simple {
        a: i32,
        b: i32,
    }

    #[derive(Serialize)]
    struct Nested {
        name: String,
        tags: Vec<String>,
        inner: Simple,
    }

    #[test]
    fn hash_params_deterministic() {
        let p = Simple { a: 1, b: 2 };
        let h1 = hash_params(&p);
        let h2 = hash_params(&p);
        assert_eq!(h1, h2);
    }

    #[test]
    fn hash_params_key_order_independent() {
        // Forward order
        let mut m1 = Map::new();
        m1.insert("a".to_string(), json!(1));
        m1.insert("b".to_string(), json!(2));
        let v1 = Value::Object(m1);

        // Reverse order
        let mut m2 = Map::new();
        m2.insert("b".to_string(), json!(2));
        m2.insert("a".to_string(), json!(1));
        let v2 = Value::Object(m2);

        assert_eq!(hash_params(&v1), hash_params(&v2));
    }

    #[test]
    fn hash_params_different_values_differ() {
        let p1 = Simple { a: 1, b: 2 };
        let p2 = Simple { a: 1, b: 3 };
        assert_ne!(hash_params(&p1), hash_params(&p2));
    }

    #[test]
    fn hash_params_whitespace_invariant() {
        let v1 = json!({"x": 1});
        let v2: Value = serde_json::from_str("{ \"x\" : 1 }").unwrap();
        assert_eq!(hash_params(&v1), hash_params(&v2));
    }

    #[test]
    fn params_hash_display_is_64_hex_chars() {
        let p = Simple { a: 1, b: 2 };
        let h = hash_params(&p);
        let s = h.to_string();
        assert_eq!(s.len(), 64);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn params_hash_serde_roundtrip() {
        let p = Simple { a: 42, b: -7 };
        let h = hash_params(&p);
        let json_str = serde_json::to_string(&h).unwrap();
        let h2: ParamsHash = serde_json::from_str(&json_str).unwrap();
        assert_eq!(h.as_bytes(), h2.as_bytes());
    }

    #[test]
    fn params_hash_serde_rejects_wrong_length() {
        let res: Result<ParamsHash, _> = serde_json::from_str("\"abcd\"");
        assert!(res.is_err());
    }

    #[test]
    fn hash_params_of_nested_structures() {
        let p = Nested {
            name: "view".to_string(),
            tags: vec!["a".to_string(), "b".to_string()],
            inner: Simple { a: 1, b: 2 },
        };
        let h1 = hash_params(&p);
        let h2 = hash_params(&p);
        assert_eq!(h1, h2);
    }

    // Silence unused-import warning when Deserialize isn't otherwise
    // exercised in this test module.
    #[allow(dead_code)]
    #[derive(Deserialize)]
    struct _DeserCheck {
        _x: Option<i32>,
    }
}
