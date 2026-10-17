//! Streams, append requests and stored events.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{Digest, EventId, EventType, SchemaVersion, Sequence, StreamId, Timestamp};

/// Free-form JSON object attached to a stream or event.
pub type Metadata = Map<String, Value>;

/// Optimistic concurrency precondition for an append.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "sequence")]
pub enum ExpectedSequence {
    /// Append regardless of the current head.
    #[default]
    Any,
    /// Append only if the stream head is exactly this sequence
    /// ([`Sequence::ZERO`] means "the stream is empty"). Otherwise the
    /// append fails with [`StoreError::SequenceConflict`](crate::StoreError::SequenceConflict).
    Exact(Sequence),
}

/// A request to append one event to a stream.
///
/// The store assigns the sequence number, timestamp and digests; the caller
/// supplies the event content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppendRequest {
    /// Kind of event.
    pub event_type: EventType,
    /// Version of the payload schema for `event_type`. Defaults to 1.
    #[serde(default)]
    pub schema_version: SchemaVersion,
    /// Event body. Any JSON value is accepted; it is stored canonically.
    pub payload: Value,
    /// Optional event metadata (correlation ids, actor, …). Defaults to `{}`.
    #[serde(default)]
    pub metadata: Metadata,
    /// Concurrency precondition. Defaults to [`ExpectedSequence::Any`].
    #[serde(default)]
    pub expected_sequence: ExpectedSequence,
    /// Caller-chosen event id. When `None`, the store generates a UUIDv7.
    /// A supplied id that already exists is rejected with
    /// [`StoreError::DuplicateEventId`](crate::StoreError::DuplicateEventId).
    #[serde(default)]
    pub event_id: Option<EventId>,
}

impl AppendRequest {
    /// Creates a request with schema version 1, empty metadata, no
    /// concurrency precondition and a store-generated id.
    #[must_use]
    pub fn new(event_type: EventType, payload: Value) -> Self {
        Self {
            event_type,
            schema_version: SchemaVersion::INITIAL,
            payload,
            metadata: Metadata::new(),
            expected_sequence: ExpectedSequence::Any,
            event_id: None,
        }
    }

    /// Sets the payload schema version.
    #[must_use]
    pub fn with_schema_version(mut self, version: SchemaVersion) -> Self {
        self.schema_version = version;
        self
    }

    /// Sets the event metadata.
    #[must_use]
    pub fn with_metadata(mut self, metadata: Metadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// Requires the stream head to equal `sequence` at append time.
    #[must_use]
    pub fn expect_sequence(mut self, sequence: Sequence) -> Self {
        self.expected_sequence = ExpectedSequence::Exact(sequence);
        self
    }

    /// Uses a caller-chosen event id.
    #[must_use]
    pub fn with_event_id(mut self, event_id: EventId) -> Self {
        self.event_id = Some(event_id);
        self
    }
}

/// The latest position of a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamHead {
    /// Sequence of the last event, or [`Sequence::ZERO`] for an empty stream.
    pub sequence: Sequence,
    /// Digest of the last event, or [`Digest::GENESIS`] for an empty stream.
    pub digest: Digest,
}

impl StreamHead {
    /// Head of a stream that has no events.
    pub const EMPTY: Self = Self {
        sequence: Sequence::ZERO,
        digest: Digest::GENESIS,
    };

    /// Returns `true` if the stream has no events.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sequence == Sequence::ZERO
    }
}

/// An event stream: an ordered, append-only log of events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stream {
    /// Stream identifier.
    pub id: StreamId,
    /// When the stream was created (informational).
    pub created_at: Timestamp,
    /// Immutable metadata supplied at creation.
    pub metadata: Metadata,
    /// Current head.
    pub head: StreamHead,
}

/// An event as persisted by the store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredEvent {
    /// Globally unique event id.
    pub event_id: EventId,
    /// Stream the event belongs to.
    pub stream_id: StreamId,
    /// Position within the stream; the authoritative order.
    pub sequence: Sequence,
    /// Kind of event.
    pub event_type: EventType,
    /// Payload schema version.
    pub schema_version: SchemaVersion,
    /// When the store recorded the event (informational, not an ordering key).
    pub recorded_at: Timestamp,
    /// Event body.
    pub payload: Value,
    /// Event metadata.
    pub metadata: Metadata,
    /// Digest of the preceding event in the stream ([`Digest::GENESIS`] for sequence 1).
    pub prev_digest: Digest,
    /// Integrity digest of this event; see [`crate::integrity`].
    pub digest: Digest,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn append_request_defaults_and_builders() {
        let ty = EventType::new("thing.happened").unwrap();
        let request = AppendRequest::new(ty.clone(), json!({"n": 1}));
        assert_eq!(request.schema_version, SchemaVersion::INITIAL);
        assert!(request.metadata.is_empty());
        assert_eq!(request.expected_sequence, ExpectedSequence::Any);
        assert_eq!(request.event_id, None);

        let id = EventId::generate();
        let mut meta = Metadata::new();
        meta.insert("actor".into(), json!("alice"));
        let request = request
            .with_schema_version(SchemaVersion::new(2).unwrap())
            .with_metadata(meta.clone())
            .expect_sequence(Sequence::ZERO)
            .with_event_id(id);
        assert_eq!(request.schema_version.get(), 2);
        assert_eq!(request.metadata, meta);
        assert_eq!(
            request.expected_sequence,
            ExpectedSequence::Exact(Sequence::ZERO)
        );
        assert_eq!(request.event_id, Some(id));
    }

    #[test]
    fn append_request_deserializes_with_defaults_and_validation() {
        let request: AppendRequest =
            serde_json::from_value(json!({"event_type": "a.b", "payload": [1, 2]})).unwrap();
        assert_eq!(request.event_type.as_str(), "a.b");
        assert_eq!(request.payload, json!([1, 2]));
        assert_eq!(request.expected_sequence, ExpectedSequence::Any);

        let request: AppendRequest = serde_json::from_value(json!({
            "event_type": "a.b",
            "payload": null,
            "expected_sequence": {"kind": "exact", "sequence": 4}
        }))
        .unwrap();
        assert_eq!(
            request.expected_sequence,
            ExpectedSequence::Exact(Sequence::new(4).unwrap())
        );

        // Missing payload, bad event type, zero schema version, non-object metadata.
        for bad in [
            json!({"event_type": "a.b"}),
            json!({"event_type": "a b", "payload": 1}),
            json!({"event_type": "a.b", "payload": 1, "schema_version": 0}),
            json!({"event_type": "a.b", "payload": 1, "metadata": [1]}),
        ] {
            assert!(serde_json::from_value::<AppendRequest>(bad).is_err());
        }
    }

    #[test]
    fn stored_event_serde_round_trip() {
        let event = StoredEvent {
            event_id: EventId::generate(),
            stream_id: StreamId::new("s").unwrap(),
            sequence: Sequence::FIRST,
            event_type: EventType::new("t").unwrap(),
            schema_version: SchemaVersion::INITIAL,
            recorded_at: Timestamp::from_unix_micros(42).unwrap(),
            payload: json!({"x": [1, "two", null]}),
            metadata: Metadata::new(),
            prev_digest: Digest::GENESIS,
            digest: Digest::from_bytes([7; 32]),
        };
        let text = serde_json::to_string(&event).unwrap();
        assert_eq!(serde_json::from_str::<StoredEvent>(&text).unwrap(), event);
    }

    #[test]
    fn empty_head() {
        assert!(StreamHead::EMPTY.is_empty());
        assert_eq!(StreamHead::EMPTY.digest, Digest::GENESIS);
    }
}
