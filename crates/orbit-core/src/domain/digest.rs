//! SHA-256 digest value type.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::ValidationError;

/// A 32-byte SHA-256 digest, rendered as 64 lowercase hex characters.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Digest([u8; 32]);

impl Digest {
    /// Previous-digest value of the first event in every stream, and the head
    /// digest of an empty stream: 32 zero bytes.
    pub const GENESIS: Self = Self([0; 32]);

    /// Wraps raw digest bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Converts a byte slice that must be exactly 32 bytes long.
    ///
    /// # Errors
    /// Returns [`ValidationError::InvalidDigestLength`] for any other length.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, ValidationError> {
        <[u8; 32]>::try_from(bytes)
            .map(Self)
            .map_err(|_| ValidationError::InvalidDigestLength(bytes.len()))
    }

    /// Parses 64 lowercase hexadecimal characters.
    ///
    /// # Errors
    /// Returns [`ValidationError::InvalidDigestHex`] for malformed input.
    pub fn from_hex(text: &str) -> Result<Self, ValidationError> {
        fn nibble(c: u8) -> Option<u8> {
            match c {
                b'0'..=b'9' => Some(c - b'0'),
                b'a'..=b'f' => Some(c - b'a' + 10),
                _ => None,
            }
        }
        let bytes = text.as_bytes();
        if bytes.len() != 64 {
            return Err(ValidationError::InvalidDigestHex);
        }
        let mut out = [0u8; 32];
        for (slot, pair) in out.iter_mut().zip(bytes.as_chunks::<2>().0) {
            let (Some(hi), Some(lo)) = (nibble(pair[0]), nibble(pair[1])) else {
                return Err(ValidationError::InvalidDigestHex);
            };
            *slot = (hi << 4) | lo;
        }
        Ok(Self(out))
    }

    /// Returns the raw bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lowercase hexadecimal rendering.
    #[must_use]
    pub fn to_hex(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(64);
        for byte in self.0 {
            out.push(char::from(HEX[usize::from(byte >> 4)]));
            out.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        out
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Digest({})", self.to_hex())
    }
}

impl Serialize for Digest {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::from_hex(&text).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip() {
        let mut bytes = [0u8; 32];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = u8::try_from(i * 8).unwrap();
        }
        let digest = Digest::from_bytes(bytes);
        let hex = digest.to_hex();
        assert_eq!(hex.len(), 64);
        assert!(hex.starts_with("0008101820"));
        assert_eq!(Digest::from_hex(&hex).unwrap(), digest);
        assert_eq!(
            serde_json::from_str::<Digest>(&serde_json::to_string(&digest).unwrap()).unwrap(),
            digest
        );
    }

    #[test]
    fn rejects_malformed() {
        assert_eq!(
            Digest::from_slice(&[0; 31]),
            Err(ValidationError::InvalidDigestLength(31))
        );
        assert!(Digest::from_hex("00").is_err());
        assert!(Digest::from_hex(&"G".repeat(64)).is_err());
        assert!(Digest::from_hex(&"A".repeat(64)).is_err());
        assert_eq!(Digest::GENESIS.to_hex(), "0".repeat(64));
    }
}
