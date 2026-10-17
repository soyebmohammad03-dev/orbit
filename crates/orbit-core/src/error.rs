//! Typed errors returned by ORBIT.
//!
//! [`ValidationError`] describes input that was rejected before it could
//! reach storage. [`StoreError`] is the error type of every
//! [`EventStore`](crate::EventStore) operation and wraps validation,
//! configuration, domain and database failures.

use crate::config::ConfigError;
use crate::domain::{EventId, Sequence, StreamId};

/// Input rejected at a validation boundary.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    /// A required textual field was empty.
    #[error("{field} must not be empty")]
    Empty {
        /// Name of the rejected field.
        field: &'static str,
    },
    /// A textual field exceeded its maximum length in bytes.
    #[error("{field} is {actual} bytes long; the maximum is {max}")]
    TooLong {
        /// Name of the rejected field.
        field: &'static str,
        /// Maximum permitted length in bytes.
        max: usize,
        /// Actual length in bytes.
        actual: usize,
    },
    /// A textual field contained a character outside its permitted alphabet.
    #[error("{field} contains invalid character {character:?} at byte offset {offset}")]
    InvalidCharacter {
        /// Name of the rejected field.
        field: &'static str,
        /// The offending character.
        character: char,
        /// Byte offset of the offending character.
        offset: usize,
    },
    /// A textual field did not start with an ASCII letter or digit.
    #[error("{field} must start with an ASCII letter or digit")]
    InvalidStart {
        /// Name of the rejected field.
        field: &'static str,
    },
    /// Schema versions start at 1.
    #[error("schema version must be at least 1, got {0}")]
    InvalidSchemaVersion(u64),
    /// Sequence numbers must fit in a signed 64-bit SQLite integer.
    #[error("sequence number {0} is out of range (maximum is {max})", max = Sequence::MAX.get())]
    SequenceOutOfRange(u64),
    /// The nil UUID is not a valid event identifier.
    #[error("event id must not be the nil UUID")]
    NilEventId,
    /// The canonical payload exceeded the configured limit.
    #[error("payload is {actual} bytes in canonical form; the configured maximum is {max}")]
    PayloadTooLarge {
        /// Canonical payload size in bytes.
        actual: usize,
        /// Configured maximum in bytes.
        max: usize,
    },
    /// The canonical metadata exceeded the configured limit.
    #[error("metadata is {actual} bytes in canonical form; the configured maximum is {max}")]
    MetadataTooLarge {
        /// Canonical metadata size in bytes.
        actual: usize,
        /// Configured maximum in bytes.
        max: usize,
    },
    /// A JSON value could not be represented canonically.
    #[error("JSON value cannot be canonicalized: {0}")]
    InvalidJson(String),
    /// A timestamp was outside the supported range.
    #[error("timestamp {micros} µs since the Unix epoch is outside the supported range")]
    TimestampOutOfRange {
        /// Microseconds since the Unix epoch.
        micros: i64,
    },
    /// A timestamp string could not be parsed as RFC 3339.
    #[error("invalid RFC 3339 timestamp: {0}")]
    InvalidTimestamp(String),
    /// A digest did not have exactly 32 bytes.
    #[error("digest must be 32 bytes, got {0}")]
    InvalidDigestLength(usize),
    /// A digest string was not 64 lowercase hexadecimal characters.
    #[error("digest must be 64 lowercase hexadecimal characters")]
    InvalidDigestHex,
    /// A read limit of zero was requested.
    #[error("read limit must be at least 1")]
    ZeroLimit,
}

/// Opaque wrapper around the underlying database driver error.
///
/// The driver type is deliberately not exposed so that callers do not couple
/// to the persistence implementation. The message and source chain remain
/// available through [`std::fmt::Display`] and [`std::error::Error::source`].
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct DatabaseError(#[from] rusqlite::Error);

/// Error returned by event store operations.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The request failed validation and nothing was written.
    #[error("invalid input: {0}")]
    Validation(#[from] ValidationError),
    /// The store configuration was invalid.
    #[error("invalid configuration: {0}")]
    Config(#[from] ConfigError),
    /// A stream with this identifier already exists.
    #[error("stream {0} already exists")]
    StreamAlreadyExists(StreamId),
    /// No stream with this identifier exists.
    #[error("stream {0} does not exist")]
    StreamNotFound(StreamId),
    /// The stream exists but has no event at this sequence number.
    #[error("stream {stream} has no event at sequence {sequence}")]
    EventNotFound {
        /// Stream that was queried.
        stream: StreamId,
        /// Sequence number that was requested.
        sequence: Sequence,
    },
    /// Optimistic concurrency check failed; nothing was written.
    #[error("sequence conflict on stream {stream}: expected head {expected}, actual head {actual}")]
    SequenceConflict {
        /// Stream that was appended to.
        stream: StreamId,
        /// Head sequence the caller expected.
        expected: Sequence,
        /// Head sequence actually found inside the append transaction.
        actual: Sequence,
    },
    /// An event with this identifier already exists in the store.
    #[error("an event with id {0} already exists")]
    DuplicateEventId(EventId),
    /// The stream reached the maximum representable sequence number.
    #[error("stream {0} has reached the maximum sequence number")]
    SequenceExhausted(StreamId),
    /// The database stayed locked by another connection for longer than the
    /// configured busy timeout. The operation was not applied and may be retried.
    #[error("database is busy (locked by another connection beyond the busy timeout)")]
    Busy,
    /// A persisted record could not be decoded into a valid domain value.
    #[error("corrupt record in stream {stream} at sequence {sequence}: {reason}")]
    CorruptRecord {
        /// Stream containing the record.
        stream: String,
        /// Raw stored sequence number.
        sequence: i64,
        /// Description of the defect.
        reason: String,
    },
    /// The database file exists but was not created by ORBIT.
    #[error("not an ORBIT database: {0}")]
    NotAnOrbitDatabase(String),
    /// The database was created by a newer ORBIT version.
    #[error(
        "database schema version {found} is newer than the newest supported version {supported}"
    )]
    UnsupportedSchemaVersion {
        /// Version recorded in the database file.
        found: i64,
        /// Newest version this build understands.
        supported: i64,
    },
    /// An unexpected database failure.
    #[error("database error: {0}")]
    Database(#[from] DatabaseError),
}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        use rusqlite::ErrorCode;
        match error.sqlite_error_code() {
            Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => StoreError::Busy,
            _ => StoreError::Database(DatabaseError(error)),
        }
    }
}

/// Convenience alias for results of store operations.
pub type Result<T, E = StoreError> = std::result::Result<T, E>;
