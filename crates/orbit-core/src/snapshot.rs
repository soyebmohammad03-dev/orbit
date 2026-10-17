//! Snapshot domain types, digest construction and validation outcomes.
//!
//! A snapshot is a persisted projection state together with the exact
//! event-history position it represents. It is *derived* data: it can always
//! be rebuilt by replaying the stream, and it is only trusted after
//! validation. Persistence lives in [`crate::store`]; selection and use live
//! in [`crate::reconstruct`].
//!
//! # Snapshot digest (`ORBIT-SNAPSHOT-DIGEST-V1`)
//!
//! ```text
//! state_digest = SHA-256(
//!     lp("ORBIT-SNAPSHOT-DIGEST-V1")
//!  || snapshot_id                   16 bytes
//!  || lp(stream_id)                 UTF-8
//!  || be_u64(sequence)
//!  || lp(projection_id)             UTF-8
//!  || be_u32(projection_version)
//!  || event_digest                  32 bytes: digest of the event at `sequence`
//!  || be_i64(created_at)            µs since the Unix epoch
//!  || lp(state)                     canonical JSON, as stored
//! )
//! lp(x) = be_u64(len(x)) || x
//! ```
//!
//! `event_digest` binds the snapshot to one specific history: the snapshot is
//! only usable while the event stored at `sequence` still has that digest.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use crate::domain::{Digest, ProjectionId, Sequence, StreamId, Timestamp};
use crate::error::ValidationError;

/// Domain-separation tag of the snapshot digest construction.
pub const SNAPSHOT_DIGEST_DOMAIN_TAG: &str = "ORBIT-SNAPSHOT-DIGEST-V1";

/// Maximum canonical size of a snapshot state: 16 MiB.
pub const MAX_SNAPSHOT_STATE_BYTES: usize = crate::config::HARD_MAX_BYTES;

/// Unique snapshot identifier (UUIDv7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "Uuid", into = "Uuid")]
pub struct SnapshotId(Uuid);

impl SnapshotId {
    /// Generates a new time-based (UUIDv7) snapshot id.
    #[must_use]
    pub fn generate() -> Self {
        Self(Uuid::now_v7())
    }

    /// Wraps a UUID.
    ///
    /// # Errors
    /// [`ValidationError::NilSnapshotId`] for the nil UUID.
    pub fn from_uuid(uuid: Uuid) -> Result<Self, ValidationError> {
        if uuid.is_nil() {
            Err(ValidationError::NilSnapshotId)
        } else {
            Ok(Self(uuid))
        }
    }

    /// The underlying UUID.
    #[must_use]
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }

    /// The 16-byte representation used for storage and hashing.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

impl TryFrom<Uuid> for SnapshotId {
    type Error = ValidationError;
    fn try_from(value: Uuid) -> Result<Self, Self::Error> {
        Self::from_uuid(value)
    }
}

impl From<SnapshotId> for Uuid {
    fn from(value: SnapshotId) -> Self {
        value.0
    }
}

impl fmt::Display for SnapshotId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Identifies the code that produced a state: a projection name and version.
///
/// Snapshots are only compatible with a projection whose key is *equal*.
/// Bump the version whenever a change to the projection could produce a
/// different state from the same events, or changes the state's shape.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProjectionKey {
    /// Projection name.
    pub id: ProjectionId,
    /// Projection version, at least 1.
    pub version: u32,
}

impl ProjectionKey {
    /// Builds a key.
    ///
    /// # Errors
    /// A [`ValidationError`] if `id` is not a valid name or `version` is 0.
    pub fn new(id: &str, version: u32) -> Result<Self, ValidationError> {
        if version == 0 {
            return Err(ValidationError::InvalidProjectionVersion(0));
        }
        Ok(Self {
            id: ProjectionId::new(id)?,
            version,
        })
    }
}

impl fmt::Display for ProjectionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@v{}", self.id, self.version)
    }
}

/// A persisted snapshot, decoded from storage.
///
/// Decoding checks that every field is well-formed but does **not** verify
/// the digest or the binding to history; use
/// [`EventStore::check_snapshot`](crate::EventStore::check_snapshot) or let the
/// [`Reconstructor`](crate::reconstruct::Reconstructor) validate it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Snapshot identifier.
    pub id: SnapshotId,
    /// Stream the state was reconstructed from.
    pub stream_id: StreamId,
    /// Last event applied to produce `state` (≥ 1).
    pub sequence: Sequence,
    /// Projection that produced `state`.
    pub projection: ProjectionKey,
    /// Digest of the event at `sequence` when the snapshot was taken.
    pub event_digest: Digest,
    /// The projection state, as JSON.
    pub state: Value,
    /// Integrity digest over all of the above; see the module docs.
    pub state_digest: Digest,
    /// When the snapshot was stored (informational).
    pub created_at: Timestamp,
}

/// Fields covered by a snapshot digest. `state` is the canonical JSON text.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SnapshotDigestInput<'a> {
    pub(crate) snapshot_id: SnapshotId,
    pub(crate) stream_id: &'a str,
    pub(crate) sequence: Sequence,
    pub(crate) projection_id: &'a str,
    pub(crate) projection_version: u32,
    pub(crate) event_digest: Digest,
    pub(crate) created_at: Timestamp,
    pub(crate) state: &'a str,
}

impl SnapshotDigestInput<'_> {
    pub(crate) fn compute(&self) -> Digest {
        fn lp(hasher: &mut Sha256, bytes: &[u8]) {
            hasher.update((bytes.len() as u64).to_be_bytes());
            hasher.update(bytes);
        }
        let mut hasher = Sha256::new();
        lp(&mut hasher, SNAPSHOT_DIGEST_DOMAIN_TAG.as_bytes());
        hasher.update(self.snapshot_id.as_bytes());
        lp(&mut hasher, self.stream_id.as_bytes());
        hasher.update(self.sequence.get().to_be_bytes());
        lp(&mut hasher, self.projection_id.as_bytes());
        hasher.update(self.projection_version.to_be_bytes());
        hasher.update(self.event_digest.as_bytes());
        hasher.update(self.created_at.unix_micros().to_be_bytes());
        lp(&mut hasher, self.state.as_bytes());
        Digest::from_bytes(hasher.finalize().into())
    }
}

/// A snapshot row exactly as read from storage.
#[derive(Debug, Clone)]
pub(crate) struct RawSnapshotRecord {
    pub(crate) snapshot_id: Vec<u8>,
    pub(crate) stream_id: String,
    pub(crate) sequence: i64,
    pub(crate) projection_id: String,
    pub(crate) projection_version: i64,
    pub(crate) event_digest: Vec<u8>,
    pub(crate) state: String,
    pub(crate) state_digest: Vec<u8>,
    pub(crate) created_at: i64,
}

impl RawSnapshotRecord {
    /// Printable identity for diagnostics, even when the id is malformed.
    pub(crate) fn display_id(&self) -> String {
        match Uuid::from_slice(&self.snapshot_id) {
            Ok(uuid) => uuid.to_string(),
            Err(_) => self
                .snapshot_id
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
        }
    }

    /// Validates every field and builds the domain representation.
    pub(crate) fn decode(&self) -> Result<Snapshot, String> {
        let id = Uuid::from_slice(&self.snapshot_id)
            .map_err(|_| {
                format!(
                    "snapshot id is {} bytes, expected 16",
                    self.snapshot_id.len()
                )
            })
            .and_then(|u| SnapshotId::from_uuid(u).map_err(|e| e.to_string()))?;
        let stream_id =
            StreamId::new(self.stream_id.as_str()).map_err(|e| format!("stream id: {e}"))?;
        let sequence = Sequence::from_i64(self.sequence)
            .filter(|s| *s >= Sequence::FIRST)
            .ok_or_else(|| format!("sequence {} is not positive", self.sequence))?;
        let version = u32::try_from(self.projection_version)
            .map_err(|_| format!("projection version {} is invalid", self.projection_version))?;
        let projection = ProjectionKey::new(&self.projection_id, version)
            .map_err(|e| format!("projection: {e}"))?;
        let event_digest =
            Digest::from_slice(&self.event_digest).map_err(|e| format!("event_digest: {e}"))?;
        let state: Value =
            serde_json::from_str(&self.state).map_err(|e| format!("state is not JSON: {e}"))?;
        let state_digest =
            Digest::from_slice(&self.state_digest).map_err(|e| format!("state_digest: {e}"))?;
        let created_at =
            Timestamp::from_unix_micros(self.created_at).map_err(|e| format!("created_at: {e}"))?;
        Ok(Snapshot {
            id,
            stream_id,
            sequence,
            projection,
            event_digest,
            state,
            state_digest,
            created_at,
        })
    }

    /// Recomputes the digest over the stored bytes. Requires a successful decode.
    pub(crate) fn computed_digest(&self, snapshot: &Snapshot) -> Digest {
        SnapshotDigestInput {
            snapshot_id: snapshot.id,
            stream_id: &self.stream_id,
            sequence: snapshot.sequence,
            projection_id: &self.projection_id,
            projection_version: snapshot.projection.version,
            event_digest: snapshot.event_digest,
            created_at: snapshot.created_at,
            state: &self.state,
        }
        .compute()
    }
}

/// Why a stored snapshot cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SnapshotProblem {
    /// The stored row is malformed (wrong lengths, invalid JSON, …).
    Malformed {
        /// Description of the defect.
        reason: String,
    },
    /// The stored digest does not match the digest recomputed from the row.
    IntegrityMismatch {
        /// Digest found in storage.
        stored: Digest,
        /// Digest recomputed from the stored fields.
        computed: Digest,
    },
    /// The event at the snapshot's sequence is missing or has a different
    /// digest than the one the snapshot was built on.
    HistoryMismatch {
        /// Event digest recorded in the snapshot.
        expected: Digest,
        /// Digest of the stored event, or `None` if no event exists there.
        actual: Option<Digest>,
    },
    /// The snapshot was produced by a different projection or version.
    Incompatible {
        /// Key recorded in the snapshot.
        found: ProjectionKey,
        /// Key of the projection doing the reconstruction.
        expected: ProjectionKey,
    },
    /// The snapshot belongs to a different stream than requested.
    WrongStream {
        /// Stream recorded in the snapshot.
        found: StreamId,
    },
    /// The snapshot is past the requested target sequence.
    BeyondTarget {
        /// Snapshot sequence.
        sequence: Sequence,
        /// Requested target.
        target: Sequence,
    },
    /// The state is valid JSON but does not deserialize into the projection's state type.
    Undecodable {
        /// Deserialization error.
        reason: String,
    },
}

impl fmt::Display for SnapshotProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed { reason } => write!(f, "malformed record: {reason}"),
            Self::IntegrityMismatch { stored, computed } => {
                write!(f, "digest mismatch (stored {stored}, computed {computed})")
            }
            Self::HistoryMismatch { expected, actual } => match actual {
                Some(actual) => write!(
                    f,
                    "history changed: snapshot built on event digest {expected}, stored event has {actual}"
                ),
                None => write!(f, "history changed: event {expected} no longer exists"),
            },
            Self::Incompatible { found, expected } => {
                write!(f, "incompatible projection {found}, expected {expected}")
            }
            Self::WrongStream { found } => write!(f, "snapshot belongs to stream {found}"),
            Self::BeyondTarget { sequence, target } => {
                write!(
                    f,
                    "snapshot at sequence {sequence} is beyond target {target}"
                )
            }
            Self::Undecodable { reason } => write!(f, "state cannot be decoded: {reason}"),
        }
    }
}

/// Storage-level validation outcome of one snapshot (see
/// [`EventStore::check_snapshot`](crate::EventStore::check_snapshot)).
/// Projection compatibility and state decoding are checked by the
/// [`Reconstructor`](crate::reconstruct::Reconstructor).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SnapshotCheck {
    /// Digest and history binding are intact.
    Intact {
        /// The decoded snapshot.
        snapshot: Snapshot,
    },
    /// The snapshot must not be used.
    Unusable {
        /// Snapshot sequence as stored.
        sequence: i64,
        /// Why.
        problem: SnapshotProblem,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_ids_validate() {
        assert!(ProjectionKey::new("inventory", 1).is_ok());
        assert_eq!(
            ProjectionKey::new("inventory", 0),
            Err(ValidationError::InvalidProjectionVersion(0))
        );
        assert!(ProjectionKey::new("bad name", 1).is_err());
        assert_eq!(
            ProjectionKey::new("counter", 3).unwrap().to_string(),
            "counter@v3"
        );
        assert_eq!(
            SnapshotId::from_uuid(Uuid::nil()),
            Err(ValidationError::NilSnapshotId)
        );
        assert_eq!(SnapshotId::generate().as_uuid().get_version_num(), 7);
    }

    /// Golden vector, cross-checked against an independent Python `hashlib`
    /// implementation of the construction documented above.
    #[test]
    fn golden_snapshot_digest() {
        let digest = SnapshotDigestInput {
            snapshot_id: SnapshotId::from_uuid(Uuid::from_u128(
                0x0192_0000_0000_7000_8000_0000_0000_0002,
            ))
            .unwrap(),
            stream_id: "counter-1",
            sequence: Sequence::new(3).unwrap(),
            projection_id: "counter",
            projection_version: 1,
            event_digest: Digest::from_bytes([0x11; 32]),
            created_at: Timestamp::from_unix_micros(1_700_000_000_000_000).unwrap(),
            state: r#"{"value":6}"#,
        }
        .compute();
        assert_eq!(
            digest.to_hex(),
            "43b3fc4d5cbd82b57f54c64fd146ee738a5b50eee6856d577f7f62aa43a3ffc1"
        );
    }
}
