//! Errors of the lab.

use orbit_core::{Digest, StoreError};

use crate::registry::{ExperimentId, Status};

/// A lab operation failed.
///
/// Problems *found by* an experiment (a projection rejecting an event, a
/// divergence, a damaged history) are results, not errors: they are recorded
/// in the experiment. These errors mean the operation itself could not be
/// carried out.
#[derive(Debug, thiserror::Error)]
pub enum LabError {
    /// The experiment specification is invalid.
    #[error("invalid experiment specification: {field}: {reason}")]
    InvalidSpec {
        /// Offending field.
        field: &'static str,
        /// Why.
        reason: String,
    },
    /// No implementation with this identifier is registered.
    #[error("no implementation {0:?} is registered")]
    UnknownImplementation(String),
    /// Two implementations were registered under the same identifier.
    #[error("implementation {0:?} is already registered")]
    DuplicateImplementation(String),
    /// An implementation descriptor is invalid.
    #[error("invalid implementation descriptor: {0}")]
    InvalidDescriptor(String),
    /// No experiment has this identifier.
    #[error("experiment {0} not found")]
    ExperimentNotFound(ExperimentId),
    /// The requested lifecycle transition is not allowed.
    #[error("experiment {id}: cannot go from {from} to {to}")]
    InvalidTransition {
        /// Experiment.
        id: ExperimentId,
        /// Current status.
        from: Status,
        /// Requested status.
        to: Status,
    },
    /// The experiment is in a state that does not allow the operation.
    #[error("experiment {id}: {reason}")]
    InvalidState {
        /// Experiment.
        id: ExperimentId,
        /// Why.
        reason: String,
    },
    /// A stored registry row cannot be decoded.
    #[error("registry record {record} is corrupt: {reason}")]
    CorruptRecord {
        /// Which record.
        record: String,
        /// Why.
        reason: String,
    },
    /// An artifact file does not exist.
    #[error("artifact {0} is missing")]
    ArtifactMissing(Digest),
    /// An artifact file's content does not match its digest.
    #[error("artifact {0} is corrupted (content does not match its digest)")]
    ArtifactCorrupted(Digest),
    /// The registry file was not created by the ORBIT lab.
    #[error("not an ORBIT lab registry: {0}")]
    NotALabRegistry(String),
    /// The registry was created by a newer version of the lab.
    #[error("registry schema version {found} is newer than the supported version {supported}")]
    UnsupportedSchemaVersion {
        /// Version in the file.
        found: i64,
        /// Newest version this build supports.
        supported: i64,
    },
    /// The registry database failed.
    #[error("registry database error: {0}")]
    Registry(#[from] rusqlite::Error),
    /// The event store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// File-system I/O failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON encoding failed.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Result alias for lab operations.
pub type Result<T, E = LabError> = std::result::Result<T, E>;
