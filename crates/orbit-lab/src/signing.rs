//! Optional Ed25519 signatures on evidence bundles, and the explicit trust
//! configuration that decides whether a signature means anything.
//!
//! Integrity and authenticity are separate questions:
//!
//! - **Integrity** ([`crate::bundle::validate`]): every digest in the bundle
//!   matches its content. Anyone can produce an internally consistent
//!   bundle, so integrity alone says nothing about who produced it.
//! - **Authenticity** (this module): a bundle may carry signatures, each an
//!   Ed25519 signature (the `ed25519-dalek` implementation) over
//!   `ORBIT-EVIDENCE-SIGNATURE-V1 ‖ bundle digest`. A signature only counts
//!   if its key is in a [`TrustStore`] that the verifier configured
//!   explicitly. A correct signature by any other key is reported as
//!   `signed_untrusted`; keys are never trusted automatically, including
//!   the key embedded in the signature itself. A signature that does not
//!   verify makes the bundle invalid.
//!
//! Signing keys are files created with [`SigningKeyFile::generate`] (the
//! CLI's `keygen`), written with owner-only permissions on Unix. They are
//! never embedded in a bundle, the repository or the registry. Key
//! management (where keys live, who may sign, how trust stores are
//! distributed and rotated) is the user's responsibility; see
//! docs/EVIDENCE.md.

use std::path::Path;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use orbit_core::Digest;
use serde::{Deserialize, Serialize};

use crate::error::{LabError, Result};

/// Domain tag of the signed message.
pub const SIGNATURE_TAG: &str = "ORBIT-EVIDENCE-SIGNATURE-V1";
/// The only supported algorithm.
pub const ALGORITHM: &str = "ed25519";
/// Most signatures on one bundle.
pub const MAX_SIGNATURES: usize = 8;
/// Longest key id.
pub const MAX_KEY_ID_LEN: usize = 64;

fn invalid(field: &'static str, reason: impl Into<String>) -> LabError {
    LabError::InvalidSpec {
        field,
        reason: reason.into(),
    }
}

/// Whether `id` is an acceptable key id: 1–64 characters of
/// `[A-Za-z0-9._-]`, starting with a letter or digit.
#[must_use]
pub fn valid_key_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_KEY_ID_LEN
        && id.as_bytes()[0].is_ascii_alphanumeric()
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != 2 * N || !text.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let mut out = [0u8; N];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

/// The message a signature covers.
#[must_use]
pub fn signed_message(bundle_digest: &Digest) -> Vec<u8> {
    let mut m = SIGNATURE_TAG.as_bytes().to_vec();
    m.extend_from_slice(bundle_digest.as_bytes());
    m
}

/// One signature, as carried in a bundle's `signatures` field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleSignature {
    /// Key id chosen by the signer (see [`valid_key_id`]). Only meaningful
    /// together with a trust store.
    pub key_id: String,
    /// Always `ed25519`.
    pub algorithm: String,
    /// The signer's public key, 32 bytes as lowercase hex.
    pub public_key: String,
    /// The signature, 64 bytes as lowercase hex.
    pub signature: String,
}

impl BundleSignature {
    /// Checks the fields and verifies the signature over `bundle_digest`
    /// with the embedded public key (which says nothing about trust).
    ///
    /// # Errors
    /// `(code, message)`: `malformed_signature` or `invalid_signature`.
    pub fn verify(
        &self,
        bundle_digest: &Digest,
    ) -> std::result::Result<VerifyingKey, (&'static str, String)> {
        let malformed = |m: &str| {
            (
                "malformed_signature",
                format!("signature {:?}: {m}", self.key_id),
            )
        };
        if !valid_key_id(&self.key_id) {
            return Err((
                "malformed_signature",
                "a signature has an invalid key id".to_owned(),
            ));
        }
        if self.algorithm != ALGORITHM {
            return Err(malformed("unsupported algorithm"));
        }
        let key = unhex::<32>(&self.public_key)
            .and_then(|b| VerifyingKey::from_bytes(&b).ok())
            .ok_or_else(|| {
                malformed(
                    "the public key is not 32 bytes of lowercase hex, or not a valid Ed25519 key",
                )
            })?;
        let sig = unhex::<64>(&self.signature)
            .map(|b| Signature::from_bytes(&b))
            .ok_or_else(|| malformed("the signature is not 64 bytes of lowercase hex"))?;
        key.verify_strict(&signed_message(bundle_digest), &sig)
            .map_err(|_| {
                (
                    "invalid_signature",
                    format!(
                        "signature {:?} does not verify for this bundle's digest",
                        self.key_id
                    ),
                )
            })?;
        Ok(key)
    }
}

/// A private signing key file. Never part of a bundle or the repository.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SigningKeyFile {
    /// `orbit-signing-key`.
    pub format: String,
    /// `1`.
    pub version: u32,
    /// Key id.
    pub key_id: String,
    /// `ed25519`.
    pub algorithm: String,
    /// The 32-byte secret key as hex.
    pub secret_key: String,
}

impl SigningKeyFile {
    /// A new key from the operating system's random number generator.
    ///
    /// # Errors
    /// An invalid key id, or no randomness available.
    pub fn generate(key_id: &str) -> Result<Self> {
        if !valid_key_id(key_id) {
            return Err(invalid(
                "key_id",
                "1–64 characters of A–Z, a–z, 0–9, '.', '_', '-', starting with a letter or digit",
            ));
        }
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed)
            .map_err(|e| std::io::Error::other(format!("no randomness available: {e}")))?;
        let key = SigningKey::from_bytes(&seed);
        seed.fill(0);
        Ok(Self {
            format: "orbit-signing-key".to_owned(),
            version: 1,
            key_id: key_id.to_owned(),
            algorithm: ALGORITHM.to_owned(),
            secret_key: hex(&key.to_bytes()),
        })
    }

    fn key(&self) -> Result<SigningKey> {
        if self.format != "orbit-signing-key"
            || self.version != 1
            || self.algorithm != ALGORITHM
            || !valid_key_id(&self.key_id)
        {
            return Err(invalid(
                "signing_key",
                "not an ORBIT signing key file (version 1, ed25519)",
            ));
        }
        unhex::<32>(&self.secret_key)
            .map(|b| SigningKey::from_bytes(&b))
            .ok_or_else(|| invalid("signing_key", "the secret key is not 32 bytes of hex"))
    }

    /// The trust-store entry for this key (public part only).
    ///
    /// # Errors
    /// A malformed key file.
    pub fn public(&self) -> Result<TrustedKey> {
        Ok(TrustedKey {
            key_id: self.key_id.clone(),
            algorithm: ALGORITHM.to_owned(),
            public_key: hex(self.key()?.verifying_key().as_bytes()),
        })
    }

    /// Signs a bundle digest.
    ///
    /// # Errors
    /// A malformed key file.
    pub fn sign(&self, bundle_digest: &Digest) -> Result<BundleSignature> {
        let key = self.key()?;
        Ok(BundleSignature {
            key_id: self.key_id.clone(),
            algorithm: ALGORITHM.to_owned(),
            public_key: hex(key.verifying_key().as_bytes()),
            signature: hex(&key.sign(&signed_message(bundle_digest)).to_bytes()),
        })
    }

    /// Reads a key file.
    ///
    /// # Errors
    /// I/O or format errors (the secret is never included in a message).
    pub fn read(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let file: Self = serde_json::from_str(&text)
            .map_err(|_| invalid("signing_key", "not an ORBIT signing key file"))?;
        file.key()?;
        Ok(file)
    }

    /// Writes the key file, refusing to overwrite an existing file; on Unix
    /// it is created readable and writable by its owner only.
    ///
    /// # Errors
    /// I/O errors, including an existing file.
    pub fn write_new(&self, path: &Path) -> Result<()> {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        file.write_all(serde_json::to_string_pretty(self)?.as_bytes())?;
        file.sync_all()?;
        Ok(())
    }
}

/// One trusted public key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedKey {
    /// Key id.
    pub key_id: String,
    /// `ed25519`.
    pub algorithm: String,
    /// The public key as lowercase hex.
    pub public_key: String,
}

/// The keys a verifier trusts, configured explicitly (a JSON file:
/// `{"format": "orbit-trusted-keys", "version": 1, "keys": [...]}`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustStore {
    /// `orbit-trusted-keys`.
    pub format: String,
    /// `1`.
    pub version: u32,
    /// The keys.
    pub keys: Vec<TrustedKey>,
}

impl TrustStore {
    /// A store holding `keys`.
    ///
    /// # Errors
    /// An invalid or duplicate key.
    pub fn new(keys: Vec<TrustedKey>) -> Result<Self> {
        let store = Self {
            format: "orbit-trusted-keys".to_owned(),
            version: 1,
            keys,
        };
        store.check()?;
        Ok(store)
    }

    fn check(&self) -> Result<()> {
        if self.format != "orbit-trusted-keys" || self.version != 1 {
            return Err(invalid(
                "trusted_keys",
                "not an ORBIT trusted-keys file (version 1)",
            ));
        }
        for (i, k) in self.keys.iter().enumerate() {
            if !valid_key_id(&k.key_id)
                || k.algorithm != ALGORITHM
                || unhex::<32>(&k.public_key)
                    .and_then(|b| VerifyingKey::from_bytes(&b).ok())
                    .is_none()
            {
                return Err(invalid(
                    "trusted_keys",
                    format!("key {} is not a valid ed25519 entry", i + 1),
                ));
            }
            if self.keys[..i].iter().any(|o| o.key_id == k.key_id) {
                return Err(invalid(
                    "trusted_keys",
                    format!("key id {:?} is listed twice", k.key_id),
                ));
            }
        }
        Ok(())
    }

    /// Reads and checks a trusted-keys file.
    ///
    /// # Errors
    /// I/O or format errors.
    pub fn read(path: &Path) -> Result<Self> {
        let store: Self = serde_json::from_str(&std::fs::read_to_string(path)?)
            .map_err(|e| invalid("trusted_keys", e.to_string()))?;
        store.check()?;
        Ok(store)
    }

    /// Whether `key_id` names exactly `key` here.
    #[must_use]
    pub fn trusts(&self, key_id: &str, key: &VerifyingKey) -> bool {
        let hex_key = hex(key.as_bytes());
        self.keys
            .iter()
            .any(|k| k.key_id == key_id && k.public_key == hex_key)
    }
}

/// What a bundle's signatures establish.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Authenticity {
    /// `unsigned`, `signed_untrusted` (valid signatures, none by a trusted
    /// key, or no trust store configured), or `trusted` (at least one valid
    /// signature by a key in the configured trust store). Invalid
    /// signatures make the bundle invalid instead.
    pub status: &'static str,
    /// Key ids of valid signatures by trusted keys.
    pub trusted_keys: Vec<String>,
    /// Key ids of valid signatures by keys that are not trusted.
    pub untrusted_keys: Vec<String>,
    /// Whether a trust store was configured for this check.
    pub trust_store_configured: bool,
    /// What this does and does not establish.
    pub note: &'static str,
}

/// Classifies already verified signatures.
#[must_use]
pub fn authenticity(
    verified: &[(String, VerifyingKey)],
    trust: Option<&TrustStore>,
) -> Authenticity {
    let (mut trusted, mut untrusted) = (Vec::new(), Vec::new());
    for (id, key) in verified {
        if trust.is_some_and(|t| t.trusts(id, key)) {
            trusted.push(id.clone());
        } else {
            untrusted.push(id.clone());
        }
    }
    let status = if !trusted.is_empty() {
        "trusted"
    } else if !untrusted.is_empty() {
        "signed_untrusted"
    } else {
        "unsigned"
    };
    Authenticity {
        status,
        trusted_keys: trusted,
        untrusted_keys: untrusted,
        trust_store_configured: trust.is_some(),
        note: match status {
            "trusted" => {
                "signed by a key in the configured trust store: the holder of that key signed this exact bundle; it does not make the experiment correct"
            }
            "signed_untrusted" => {
                "the signatures are valid, but no signing key is trusted here; anyone can sign with a key of their own"
            }
            _ => "unsigned: who produced this bundle is not established",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_ids_and_hex_are_strict() {
        for ok in ["lab-1", "A.b_c", "x"] {
            assert!(valid_key_id(ok));
        }
        for bad in ["", "-x", "a b", "ä", &"a".repeat(65)] {
            assert!(!valid_key_id(bad), "{bad:?}");
        }
        assert_eq!(unhex::<2>("0aff"), Some([10, 255]));
        assert!(unhex::<2>("0AFF").is_none(), "upper case is not canonical");
        assert!(unhex::<2>("0af").is_none());
    }

    #[test]
    fn sign_verify_and_classify() {
        let key = SigningKeyFile::generate("lab-1").unwrap();
        let other = SigningKeyFile::generate("lab-1").unwrap();
        let digest = crate::digest::content_digest(b"bundle");
        let sig = key.sign(&digest).unwrap();
        let verified = sig.verify(&digest).unwrap();
        // Another digest: the signature does not verify.
        let wrong = crate::digest::content_digest(b"other");
        assert_eq!(sig.verify(&wrong).unwrap_err().0, "invalid_signature");
        // Trust is explicit: same key id, different key, is not trusted.
        let store = TrustStore::new(vec![key.public().unwrap()]).unwrap();
        let foreign = TrustStore::new(vec![other.public().unwrap()]).unwrap();
        let v = vec![("lab-1".to_owned(), verified)];
        assert_eq!(authenticity(&v, Some(&store)).status, "trusted");
        assert_eq!(authenticity(&v, Some(&foreign)).status, "signed_untrusted");
        assert_eq!(authenticity(&v, None).status, "signed_untrusted");
        assert_eq!(authenticity(&[], Some(&store)).status, "unsigned");
        // Malformed fields.
        for mutate in [
            |s: &mut BundleSignature| s.signature.truncate(10),
            |s: &mut BundleSignature| s.public_key = "zz".repeat(32),
            |s: &mut BundleSignature| s.algorithm = "rsa".to_owned(),
            |s: &mut BundleSignature| s.key_id = "../x".to_owned(),
        ] {
            let mut bad = sig.clone();
            mutate(&mut bad);
            assert_eq!(bad.verify(&digest).unwrap_err().0, "malformed_signature");
        }
        // A flipped signature bit.
        let mut bad = sig;
        let flipped = if bad.signature.starts_with('0') {
            "1"
        } else {
            "0"
        };
        bad.signature.replace_range(0..1, flipped);
        assert_eq!(bad.verify(&digest).unwrap_err().0, "invalid_signature");
    }

    #[test]
    fn trust_stores_reject_bad_entries() {
        let key = SigningKeyFile::generate("k").unwrap();
        let entry = key.public().unwrap();
        assert!(
            TrustStore::new(vec![entry.clone(), entry.clone()]).is_err(),
            "duplicate id"
        );
        let mut bad = entry;
        bad.public_key = "00".repeat(31);
        assert!(TrustStore::new(vec![bad]).is_err());
    }

    #[test]
    fn key_files_are_private_and_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key.json");
        let key = SigningKeyFile::generate("k").unwrap();
        key.write_new(&path).unwrap();
        assert!(
            key.write_new(&path).is_err(),
            "an existing key is not overwritten"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(
            SigningKeyFile::read(&path).unwrap().public().unwrap(),
            key.public().unwrap()
        );
        std::fs::write(&path, "{\"format\":\"x\"}").unwrap();
        let err = SigningKeyFile::read(&path).unwrap_err().to_string();
        assert!(!err.contains(&key.secret_key));
    }
}
