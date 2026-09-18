//! Deterministic canonical JSON + a stable FNV-1a 64 hash, used for retry-loop
//! detection. `std`'s `DefaultHasher` is explicitly not stable across releases,
//! so we hash a canonical (sorted-key) serialization ourselves.

use serde_json::Value;
use std::collections::BTreeMap;

/// Reserialize a JSON value with object keys sorted, producing byte-stable output.
pub fn canonicalize(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let sorted: BTreeMap<&String, Value> =
                map.iter().map(|(k, val)| (k, canonicalize(val))).collect();
            Value::Object(
                sorted
                    .into_iter()
                    .map(|(k, val)| (k.clone(), val))
                    .collect(),
            )
        }
        Value::Array(arr) => Value::Array(arr.iter().map(canonicalize).collect()),
        other => other.clone(),
    }
}

pub fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// FNV-1a keyed by a salt: the salt is mixed in before the payload, so identical inputs
/// hash differently under different salts. Used by the `fingerprint` privacy profile to make
/// content hashes non-brute-forceable from a small known plaintext space.
pub fn keyed_fnv1a_64(salt: &[u8], bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in salt.iter().chain(std::iter::once(&0u8)).chain(bytes) {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Stable hash of a request value, ignoring object key ordering.
pub fn request_hash(v: &Value) -> u64 {
    let canon = canonicalize(v);
    let s = serde_json::to_string(&canon).unwrap_or_default();
    fnv1a_64(s.as_bytes())
}

/// As [`request_hash`], salted (see [`keyed_fnv1a_64`]).
pub fn request_hash_salted(salt: &[u8], v: &Value) -> u64 {
    let canon = canonicalize(v);
    let s = serde_json::to_string(&canon).unwrap_or_default();
    keyed_fnv1a_64(salt, s.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn key_order_does_not_change_hash() {
        let a = json!({"b": 1, "a": [{"y": 2, "x": 1}]});
        let b = json!({"a": [{"x": 1, "y": 2}], "b": 1});
        assert_eq!(request_hash(&a), request_hash(&b));
    }

    #[test]
    fn different_content_changes_hash() {
        let a = json!({"a": 1});
        let b = json!({"a": 2});
        assert_ne!(request_hash(&a), request_hash(&b));
    }
}
