//! Domain model: identifiers, streams, events and their value types.
//!
//! Everything in this module is storage-agnostic and validated on
//! construction, so a value of these types is always well-formed.

pub mod canonical;
mod digest;
mod event;
mod ids;
mod time;

pub use digest::Digest;
pub use event::{AppendRequest, ExpectedSequence, Metadata, StoredEvent, Stream, StreamHead};
pub use ids::{EventId, EventType, MAX_NAME_LEN, SchemaVersion, Sequence, StreamId};
pub use time::Timestamp;
