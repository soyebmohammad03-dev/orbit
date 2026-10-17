//! Validated identifiers and scalar value types.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::ValidationError;

/// Maximum length, in bytes, of a [`StreamId`] or [`EventType`].
pub const MAX_NAME_LEN: usize = 128;

/// Validates the shared name grammar used by stream ids and event types:
/// 1–128 ASCII bytes from `[A-Za-z0-9._:-]`, starting with a letter or digit.
fn validate_name(field: &'static str, value: &str) -> Result<(), ValidationError> {
    if value.is_empty() {
        return Err(ValidationError::Empty { field });
    }
    if value.len() > MAX_NAME_LEN {
        return Err(ValidationError::TooLong {
            field,
            max: MAX_NAME_LEN,
            actual: value.len(),
        });
    }
    if let Some((offset, character)) = value
        .char_indices()
        .find(|&(_, c)| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-')))
    {
        return Err(ValidationError::InvalidCharacter {
            field,
            character,
            offset,
        });
    }
    if !value.as_bytes()[0].is_ascii_alphanumeric() {
        return Err(ValidationError::InvalidStart { field });
    }
    Ok(())
}

macro_rules! name_type {
    ($(#[$doc:meta])* $name:ident, $field:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// Validates and wraps `value`.
            ///
            /// # Errors
            /// Returns a [`ValidationError`] if `value` is empty, longer than
            #[doc = concat!("[`MAX_NAME_LEN`] bytes, or not of the form `[A-Za-z0-9][A-Za-z0-9._:-]*`.")]
            pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
                let value = value.into();
                validate_name($field, &value)?;
                Ok(Self(value))
            }

            /// Returns the identifier as a string slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = ValidationError;
            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = ValidationError;
            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

name_type!(
    /// Identifier of an event stream, e.g. `account-42` or `orders:2026:eu`.
    ///
    /// Stream ids are case-sensitive, 1–128 ASCII bytes from
    /// `[A-Za-z0-9._:-]`, and must start with a letter or digit.
    StreamId,
    "stream id"
);

name_type!(
    /// Name of the kind of fact an event records, e.g. `account.opened`.
    ///
    /// Uses the same grammar as [`StreamId`].
    EventType,
    "event type"
);

/// Globally unique event identifier (a UUID; ORBIT generates version 7).
///
/// Event ids identify events; they do **not** define order. Order is given
/// exclusively by the per-stream [`Sequence`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "Uuid", into = "Uuid")]
pub struct EventId(Uuid);

impl EventId {
    /// Generates a new time-based (UUIDv7) event id.
    #[must_use]
    pub fn generate() -> Self {
        Self(Uuid::now_v7())
    }

    /// Wraps a caller-supplied UUID.
    ///
    /// # Errors
    /// Returns [`ValidationError::NilEventId`] for the nil UUID.
    pub fn from_uuid(uuid: Uuid) -> Result<Self, ValidationError> {
        if uuid.is_nil() {
            Err(ValidationError::NilEventId)
        } else {
            Ok(Self(uuid))
        }
    }

    /// Returns the underlying UUID.
    #[must_use]
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }

    /// Returns the 16-byte big-endian representation used for storage and hashing.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

impl TryFrom<Uuid> for EventId {
    type Error = ValidationError;
    fn try_from(value: Uuid) -> Result<Self, Self::Error> {
        Self::from_uuid(value)
    }
}

impl From<EventId> for Uuid {
    fn from(value: EventId) -> Self {
        value.0
    }
}

impl fmt::Display for EventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Version of an event type's payload schema. Starts at 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct SchemaVersion(u32);

impl SchemaVersion {
    /// The first schema version, `1`.
    pub const INITIAL: Self = Self(1);

    /// Validates and wraps a schema version.
    ///
    /// # Errors
    /// Returns [`ValidationError::InvalidSchemaVersion`] for `0`.
    pub fn new(version: u32) -> Result<Self, ValidationError> {
        if version == 0 {
            Err(ValidationError::InvalidSchemaVersion(0))
        } else {
            Ok(Self(version))
        }
    }

    /// Returns the numeric version.
    #[must_use]
    pub fn get(self) -> u32 {
        self.0
    }
}

impl Default for SchemaVersion {
    fn default() -> Self {
        Self::INITIAL
    }
}

impl TryFrom<u32> for SchemaVersion {
    type Error = ValidationError;
    fn try_from(value: u32) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<SchemaVersion> for u32 {
    fn from(value: SchemaVersion) -> Self {
        value.0
    }
}

impl fmt::Display for SchemaVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Position within a stream.
///
/// The first event of every stream has sequence `1`. Sequence `0`
/// ([`Sequence::ZERO`]) denotes the position *before* the first event and is
/// the head of an empty stream. Sequences are dense: event `n + 1` can only be
/// appended after event `n`. The maximum is `i64::MAX` so that every value is
/// representable as a SQLite integer.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(try_from = "u64", into = "u64")]
pub struct Sequence(u64);

impl Sequence {
    /// Head position of an empty stream.
    pub const ZERO: Self = Self(0);
    /// Sequence number of the first event in a stream.
    pub const FIRST: Self = Self(1);
    /// Largest representable sequence number (`i64::MAX`).
    pub const MAX: Self = Self(i64::MAX as u64);

    /// Validates and wraps a sequence number.
    ///
    /// # Errors
    /// Returns [`ValidationError::SequenceOutOfRange`] above [`Sequence::MAX`].
    pub fn new(value: u64) -> Result<Self, ValidationError> {
        if value > Self::MAX.0 {
            Err(ValidationError::SequenceOutOfRange(value))
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the numeric value.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }

    /// Returns the following sequence number, or `None` at [`Sequence::MAX`].
    #[must_use]
    pub fn next(self) -> Option<Self> {
        if self.0 < Self::MAX.0 {
            Some(Self(self.0 + 1))
        } else {
            None
        }
    }

    /// Value as a SQLite integer. Lossless because `self <= i64::MAX`.
    pub(crate) fn to_i64(self) -> i64 {
        // Invariant upheld by every constructor.
        i64::try_from(self.0).unwrap_or(i64::MAX)
    }

    /// Converts a stored SQLite integer, rejecting negative values.
    pub(crate) fn from_i64(value: i64) -> Option<Self> {
        u64::try_from(value).ok().map(Self)
    }
}

impl TryFrom<u64> for Sequence {
    type Error = ValidationError;
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Sequence> for u64 {
    fn from(value: Sequence) -> Self {
        value.0
    }
}

impl fmt::Display for Sequence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_names() {
        for name in ["a", "account-42", "orders:2026:eu", "A.b_c-d:e", "9lives"] {
            assert_eq!(StreamId::new(name).unwrap().as_str(), name);
            assert_eq!(EventType::new(name).unwrap().as_str(), name);
        }
        assert!(StreamId::new("x".repeat(MAX_NAME_LEN)).is_ok());
    }

    #[test]
    fn rejects_invalid_names() {
        assert_eq!(
            StreamId::new(""),
            Err(ValidationError::Empty { field: "stream id" })
        );
        assert_eq!(
            StreamId::new("x".repeat(MAX_NAME_LEN + 1)),
            Err(ValidationError::TooLong {
                field: "stream id",
                max: MAX_NAME_LEN,
                actual: MAX_NAME_LEN + 1
            })
        );
        assert_eq!(
            EventType::new("has space"),
            Err(ValidationError::InvalidCharacter {
                field: "event type",
                character: ' ',
                offset: 3
            })
        );
        assert!(matches!(
            StreamId::new("é"),
            Err(ValidationError::InvalidCharacter { .. })
        ));
        assert_eq!(
            StreamId::new("-leading"),
            Err(ValidationError::InvalidStart { field: "stream id" })
        );
        assert!(StreamId::new("semi;colon").is_err());
        assert!(StreamId::new("quote'").is_err());
    }

    #[test]
    fn names_validate_on_deserialize() {
        let ok: StreamId = serde_json::from_str("\"s-1\"").unwrap();
        assert_eq!(ok.as_str(), "s-1");
        assert!(serde_json::from_str::<StreamId>("\"\"").is_err());
        assert!(serde_json::from_str::<EventType>("\"a b\"").is_err());
        assert_eq!(serde_json::to_string(&ok).unwrap(), "\"s-1\"");
    }

    #[test]
    fn event_ids() {
        assert_eq!(
            EventId::from_uuid(Uuid::nil()),
            Err(ValidationError::NilEventId)
        );
        let a = EventId::generate();
        let b = EventId::generate();
        assert_ne!(a, b);
        assert_eq!(a.as_uuid().get_version_num(), 7);
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(serde_json::from_str::<EventId>(&json).unwrap(), a);
        assert!(serde_json::from_str::<EventId>(&format!("\"{}\"", Uuid::nil())).is_err());
    }

    #[test]
    fn schema_versions() {
        assert_eq!(SchemaVersion::default(), SchemaVersion::INITIAL);
        assert_eq!(SchemaVersion::new(3).unwrap().get(), 3);
        assert_eq!(
            SchemaVersion::new(0),
            Err(ValidationError::InvalidSchemaVersion(0))
        );
        assert!(serde_json::from_str::<SchemaVersion>("0").is_err());
    }

    #[test]
    fn sequences() {
        assert_eq!(Sequence::ZERO.next(), Some(Sequence::FIRST));
        assert_eq!(Sequence::MAX.next(), None);
        assert_eq!(Sequence::MAX.to_i64(), i64::MAX);
        assert_eq!(
            Sequence::new(u64::MAX),
            Err(ValidationError::SequenceOutOfRange(u64::MAX))
        );
        assert_eq!(Sequence::from_i64(-1), None);
        assert_eq!(Sequence::from_i64(5), Some(Sequence::new(5).unwrap()));
        assert!(serde_json::from_str::<Sequence>(&u64::MAX.to_string()).is_err());
    }
}
