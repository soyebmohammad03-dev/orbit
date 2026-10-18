//! Domain-separated SHA-256 digests.
//!
//! Every lab digest uses the same construction as `orbit-core`:
//! `SHA-256(lp(tag) || lp(field_1) || … )`, where `lp(x)` is the 8-byte
//! big-endian length of `x` followed by `x`. Distinct tags keep digests of
//! different kinds of objects from ever being confused. JSON values are
//! hashed in their canonical form
//! ([`to_canonical_string`]: sorted keys, no whitespace), so a digest
//! never depends on the order in which a client wrote object members.
//!
//! Artifact *content* digests are plain SHA-256 of the bytes (no tag), so
//! `shasum -a 256` verifies an artifact file directly.

use orbit_core::Digest;
use orbit_core::domain::canonical::to_canonical_string;
use serde_json::Value;
use sha2::{Digest as _, Sha256};

/// Tag of experiment-specification digests.
pub const SPEC_TAG: &str = "ORBIT-LAB-SPEC-V1";
/// Tag of implementation-identity fingerprints.
pub const IMPLEMENTATION_TAG: &str = "ORBIT-LAB-IMPLEMENTATION-V1";
/// Tag of implementation-configuration digests.
pub const CONFIG_TAG: &str = "ORBIT-LAB-CONFIG-V1";
/// Tag of input-history fingerprints.
pub const HISTORY_TAG: &str = "ORBIT-LAB-HISTORY-V1";
/// Tag of the digest over the ordered event digests fed to both implementations.
pub const EVENTS_TAG: &str = "ORBIT-LAB-EVENTS-V1";
/// Tag of experiment-result digests.
pub const RESULT_TAG: &str = "ORBIT-LAB-RESULT-V1";

/// Incremental domain-tagged hasher.
#[derive(Debug, Clone)]
pub struct TaggedHasher(Sha256);

impl TaggedHasher {
    /// Starts a digest under `tag`.
    #[must_use]
    pub fn new(tag: &str) -> Self {
        let mut hasher = Self(Sha256::new());
        hasher.field(tag.as_bytes());
        hasher
    }

    /// Adds one length-prefixed field.
    pub fn field(&mut self, bytes: &[u8]) -> &mut Self {
        self.0.update((bytes.len() as u64).to_be_bytes());
        self.0.update(bytes);
        self
    }

    /// Finishes the digest.
    #[must_use]
    pub fn finish(self) -> Digest {
        Digest::from_bytes(self.0.finalize().into())
    }
}

/// Digest of a JSON value's canonical form under `tag`.
#[must_use]
pub fn value_digest(tag: &str, value: &Value) -> Digest {
    let mut hasher = TaggedHasher::new(tag);
    hasher.field(to_canonical_string(value).as_bytes());
    hasher.finish()
}

/// Plain SHA-256 of `bytes` (artifact content digests).
#[must_use]
pub fn content_digest(bytes: &[u8]) -> Digest {
    Digest::from_bytes(Sha256::digest(bytes).into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn value_digests_ignore_member_order_and_whitespace() {
        let a: Value = serde_json::from_str(r#"{"b": 1, "a": {"y": [1, 2], "x": null}}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"a":{"x":null,"y":[1,2]},"b":1}"#).unwrap();
        assert_eq!(value_digest(SPEC_TAG, &a), value_digest(SPEC_TAG, &b));
        assert_ne!(value_digest(SPEC_TAG, &a), value_digest(RESULT_TAG, &a));
        assert_ne!(
            value_digest(SPEC_TAG, &a),
            value_digest(SPEC_TAG, &json!({"b": 2, "a": {"x": null, "y": [1, 2]}}))
        );
    }

    #[test]
    fn content_digest_is_plain_sha256() {
        assert_eq!(
            content_digest(b"abc").to_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn fields_are_length_prefixed() {
        let mut one = TaggedHasher::new("T");
        one.field(b"ab").field(b"c");
        let mut two = TaggedHasher::new("T");
        two.field(b"a").field(b"bc");
        assert_ne!(one.finish(), two.finish());
    }
}
