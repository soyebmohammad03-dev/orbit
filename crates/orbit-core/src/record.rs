//! Raw persisted event rows and their validated decoding.
//!
//! The storage layer reads rows into [`RawEventRecord`] without interpreting
//! them. Decoding into a [`StoredEvent`] re-validates every field so that a
//! damaged database surfaces as a typed error instead of a panic or a
//! silently malformed value. Integrity verification works on the raw form so
//! that it hashes exactly the bytes that are on disk.

use serde_json::Value;

use crate::domain::{
    Digest, EventId, EventType, Metadata, SchemaVersion, Sequence, StoredEvent, StreamId, Timestamp,
};
use crate::integrity::EventDigestInput;

/// An event row exactly as read from storage.
#[derive(Debug, Clone)]
pub(crate) struct RawEventRecord {
    pub(crate) stream_id: String,
    pub(crate) sequence: i64,
    pub(crate) event_id: Vec<u8>,
    pub(crate) event_type: String,
    pub(crate) schema_version: i64,
    pub(crate) recorded_at: i64,
    pub(crate) payload: String,
    pub(crate) metadata: String,
    pub(crate) prev_digest: Vec<u8>,
    pub(crate) digest: Vec<u8>,
}

impl RawEventRecord {
    /// Validates every field and builds the domain representation.
    pub(crate) fn decode(&self) -> Result<StoredEvent, String> {
        let stream_id =
            StreamId::new(self.stream_id.as_str()).map_err(|e| format!("stream id: {e}"))?;
        let sequence = Sequence::from_i64(self.sequence)
            .filter(|s| *s >= Sequence::FIRST)
            .ok_or_else(|| format!("sequence {} is not positive", self.sequence))?;
        let event_id = <[u8; 16]>::try_from(self.event_id.as_slice())
            .map_err(|_| format!("event id is {} bytes, expected 16", self.event_id.len()))
            .and_then(|bytes| {
                EventId::from_uuid(uuid::Uuid::from_bytes(bytes)).map_err(|e| e.to_string())
            })?;
        let event_type =
            EventType::new(self.event_type.as_str()).map_err(|e| format!("event type: {e}"))?;
        let schema_version = u32::try_from(self.schema_version)
            .ok()
            .and_then(|v| SchemaVersion::new(v).ok())
            .ok_or_else(|| format!("schema version {} is invalid", self.schema_version))?;
        let recorded_at = Timestamp::from_unix_micros(self.recorded_at)
            .map_err(|e| format!("recorded_at: {e}"))?;
        let payload: Value =
            serde_json::from_str(&self.payload).map_err(|e| format!("payload: {e}"))?;
        let metadata: Metadata =
            serde_json::from_str(&self.metadata).map_err(|e| format!("metadata: {e}"))?;
        let prev_digest =
            Digest::from_slice(&self.prev_digest).map_err(|e| format!("prev_digest: {e}"))?;
        let digest = Digest::from_slice(&self.digest).map_err(|e| format!("digest: {e}"))?;
        Ok(StoredEvent {
            event_id,
            stream_id,
            sequence,
            event_type,
            schema_version,
            recorded_at,
            payload,
            metadata,
            prev_digest,
            digest,
        })
    }

    /// The digest input over the stored bytes. Only meaningful after
    /// [`decode`](Self::decode) succeeded, which guarantees field validity.
    pub(crate) fn digest_input<'a>(&'a self, event: &'a StoredEvent) -> EventDigestInput<'a> {
        EventDigestInput {
            stream_id: &self.stream_id,
            sequence: event.sequence,
            event_id: event.event_id,
            event_type: &self.event_type,
            schema_version: event.schema_version,
            recorded_at: event.recorded_at,
            payload: &self.payload,
            metadata: &self.metadata,
            prev_digest: event.prev_digest,
        }
    }
}
