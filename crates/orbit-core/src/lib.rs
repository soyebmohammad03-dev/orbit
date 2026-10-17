//! # orbit-core
//!
//! The core of ORBIT: a persistent, append-only event store with ordered
//! streams and hash-chained integrity verification, backed by SQLite, plus
//! historical state reconstruction from those streams.
//!
//! ```no_run
//! use orbit_core::{AppendRequest, EventStore, EventType, Metadata, StoreConfig, StreamId};
//! use serde_json::json;
//!
//! # fn main() -> Result<(), orbit_core::StoreError> {
//! let store = EventStore::open(StoreConfig::new("orbit.db"))?;
//! let stream = StreamId::new("account-42")?;
//! store.create_stream(&stream, Metadata::new())?;
//!
//! let opened = EventType::new("account.opened")?;
//! let event = store.append(&stream, AppendRequest::new(opened, json!({"owner": "alice"})))?;
//! assert_eq!(event.sequence.get(), 1);
//!
//! assert!(store.verify_stream(&stream)?.is_valid());
//! # Ok(())
//! # }
//! ```
//!
//! Module overview:
//!
//! * [`domain`] — validated identifiers, streams, events, canonical JSON;
//! * [`store`] — [`EventStore`], the SQLite persistence layer (events and snapshots);
//! * [`integrity`] — digest construction and verification reports;
//! * [`projection`] — the [`Projection`] trait and example projections;
//! * [`reconstruct`] — [`Reconstructor`]: historical state, snapshots, diffs, provenance;
//! * [`snapshot`] — snapshot types, digest and validation outcomes;
//! * [`diff`] — structural JSON diff;
//! * [`config`] — [`StoreConfig`] and environment loading;
//! * [`error`] — [`StoreError`] and [`ValidationError`].

pub mod config;
pub mod diff;
pub mod domain;
pub mod error;
pub mod integrity;
pub mod projection;
pub mod reconstruct;
mod record;
pub mod snapshot;
pub mod store;

pub use config::{Durability, Limits, StoreConfig};
pub use diff::{Change, ChangeKind};
pub use domain::{
    AppendRequest, Digest, EventId, EventType, ExpectedSequence, Metadata, ProjectionId,
    SchemaVersion, Sequence, StoredEvent, Stream, StreamHead, StreamId, Timestamp,
};
pub use error::{SnapshotConflict, StoreError, ValidationError};
pub use integrity::{IntegrityIssue, IntegrityReport};
pub use projection::{Projection, ProjectionError, ProjectionErrorKind};
pub use reconstruct::{
    ProjectionFailure, ReconstructOptions, Reconstruction, ReconstructionError,
    ReconstructionReport, Reconstructor, SnapshotPolicy, Target,
};
pub use snapshot::{ProjectionKey, Snapshot, SnapshotId};
pub use store::{EventStore, NewSnapshot, SavedSnapshot};

/// Compiles the README example as a doctest so it cannot drift from the API.
#[cfg(doctest)]
#[doc = include_str!("../../../README.md")]
struct ReadmeDoctests;
