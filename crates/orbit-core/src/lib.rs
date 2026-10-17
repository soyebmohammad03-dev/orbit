//! # orbit-core
//!
//! The core of ORBIT: a persistent, append-only event store with ordered
//! streams and hash-chained integrity verification, backed by SQLite.
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
//! * [`store`] — [`EventStore`], the SQLite persistence layer;
//! * [`integrity`] — digest construction and verification reports;
//! * [`config`] — [`StoreConfig`] and environment loading;
//! * [`error`] — [`StoreError`] and [`ValidationError`].

pub mod config;
pub mod domain;
pub mod error;
pub mod integrity;
mod record;
pub mod store;

pub use config::{Durability, Limits, StoreConfig};
pub use domain::{
    AppendRequest, Digest, EventId, EventType, ExpectedSequence, Metadata, SchemaVersion, Sequence,
    StoredEvent, Stream, StreamHead, StreamId, Timestamp,
};
pub use error::{StoreError, ValidationError};
pub use integrity::{IntegrityIssue, IntegrityReport};
pub use store::EventStore;

/// Compiles the README example as a doctest so it cannot drift from the API.
#[cfg(doctest)]
#[doc = include_str!("../../../README.md")]
struct ReadmeDoctests;
