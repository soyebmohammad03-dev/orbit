//! Event digests and stream integrity verification.
//!
//! # Digest construction (`ORBIT-EVENT-DIGEST-V1`)
//!
//! Every stored event carries a SHA-256 digest over an unambiguous binary
//! encoding of its fields, and the digest of the event before it. Variable
//! length fields are prefixed with their length as a big-endian `u64`;
//! fixed-width fields are written as-is:
//!
//! ```text
//! digest = SHA-256(
//!     lp("ORBIT-EVENT-DIGEST-V1")
//!  || lp(stream_id)                 UTF-8
//!  || be_u64(sequence)
//!  || event_id                      16 bytes, RFC 4122 byte order
//!  || lp(event_type)                UTF-8
//!  || be_u32(schema_version)
//!  || be_i64(recorded_at)           microseconds since the Unix epoch
//!  || lp(payload)                   canonical JSON, UTF-8
//!  || lp(metadata)                  canonical JSON, UTF-8
//!  || prev_digest                   32 bytes
//! )
//! lp(x) = be_u64(len(x)) || x
//! ```
//!
//! `prev_digest` is [`Digest::GENESIS`] (32 zero bytes) for sequence 1 and the
//! previous event's `digest` otherwise, forming a hash chain per stream. The
//! stream row stores the head sequence and head digest, so truncating the
//! tail of a stream is also detectable.
//!
//! # What this detects, and what it does not
//!
//! Verification detects accidental corruption and naive modification of any
//! hashed field, removal or reordering of events inside a stream, and tail
//! truncation that leaves the stream head untouched. The digests are
//! **unkeyed**: anyone able to write the database file can rewrite events and
//! recompute every digest and the head consistently. This is not a defence
//! against such an attacker; it is an integrity check, not authentication.

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::domain::{Digest, EventId, SchemaVersion, Sequence, StreamHead, StreamId, Timestamp};
use crate::record::RawEventRecord;

/// Domain-separation tag; changes whenever the construction changes.
pub const DIGEST_DOMAIN_TAG: &str = "ORBIT-EVENT-DIGEST-V1";

/// Maximum number of issues retained in an [`IntegrityReport`].
pub const MAX_REPORTED_ISSUES: usize = 1_000;

/// The fields covered by an event digest. `payload` and `metadata` are the
/// canonical JSON texts exactly as stored.
#[derive(Debug, Clone, Copy)]
pub struct EventDigestInput<'a> {
    /// Stream identifier.
    pub stream_id: &'a str,
    /// Sequence number of the event.
    pub sequence: Sequence,
    /// Event identifier.
    pub event_id: EventId,
    /// Event type.
    pub event_type: &'a str,
    /// Payload schema version.
    pub schema_version: SchemaVersion,
    /// Recording timestamp.
    pub recorded_at: Timestamp,
    /// Canonical payload text.
    pub payload: &'a str,
    /// Canonical metadata text.
    pub metadata: &'a str,
    /// Digest of the preceding event, or [`Digest::GENESIS`].
    pub prev_digest: Digest,
}

impl EventDigestInput<'_> {
    /// Computes the `ORBIT-EVENT-DIGEST-V1` digest.
    #[must_use]
    pub fn compute(&self) -> Digest {
        fn lp(hasher: &mut Sha256, bytes: &[u8]) {
            // usize -> u64 is lossless on every supported platform.
            hasher.update((bytes.len() as u64).to_be_bytes());
            hasher.update(bytes);
        }
        let mut hasher = Sha256::new();
        lp(&mut hasher, DIGEST_DOMAIN_TAG.as_bytes());
        lp(&mut hasher, self.stream_id.as_bytes());
        hasher.update(self.sequence.get().to_be_bytes());
        hasher.update(self.event_id.as_bytes());
        lp(&mut hasher, self.event_type.as_bytes());
        hasher.update(self.schema_version.get().to_be_bytes());
        hasher.update(self.recorded_at.unix_micros().to_be_bytes());
        lp(&mut hasher, self.payload.as_bytes());
        lp(&mut hasher, self.metadata.as_bytes());
        hasher.update(self.prev_digest.as_bytes());
        Digest::from_bytes(hasher.finalize().into())
    }
}

/// A single integrity defect found in a stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IntegrityIssue {
    /// The stored digest does not match the digest recomputed from the
    /// stored fields: a hashed field (or the digest itself) was altered.
    DigestMismatch {
        /// Affected event.
        sequence: Sequence,
        /// Digest found in storage.
        stored: Digest,
        /// Digest recomputed from the stored fields.
        computed: Digest,
    },
    /// The event's `prev_digest` does not equal the stored digest of the
    /// event before it (or the genesis value for sequence 1).
    ChainBroken {
        /// Affected event.
        sequence: Sequence,
        /// The previous event's stored digest (or genesis).
        expected_prev: Digest,
        /// The `prev_digest` found in storage.
        stored_prev: Digest,
    },
    /// Sequence numbers are not dense: one or more events are missing.
    SequenceGap {
        /// The sequence that should have come next.
        expected: Sequence,
        /// The sequence that was found instead (raw stored value).
        found: i64,
    },
    /// A stored row could not be decoded into a valid event.
    MalformedRecord {
        /// Raw stored sequence number.
        sequence: i64,
        /// Description of the defect.
        reason: String,
    },
    /// The first event's `prev_digest` is not the genesis value (32 zero
    /// bytes). Reported instead of `ChainBroken` for sequence 1.
    InvalidGenesis {
        /// The `prev_digest` stored on event 1.
        stored_prev: Digest,
    },
    /// A trusted checkpoint (see [`StreamCheckpoint`]) does not match the
    /// stored history: the event at the checkpoint's sequence is missing or
    /// has a different digest. This also catches a history that was
    /// rewritten with consistently recomputed digests.
    CheckpointMismatch {
        /// Checkpoint sequence.
        sequence: Sequence,
        /// Digest recorded in the checkpoint.
        expected: Digest,
        /// Digest of the stored event, or `None` if it does not exist.
        actual: Option<Digest>,
    },
    /// The head recorded on the stream does not match its last event.
    HeadMismatch {
        /// Head sequence recorded on the stream row.
        recorded_sequence: Sequence,
        /// Head digest recorded on the stream row.
        recorded_digest: Digest,
        /// Sequence of the last event actually present.
        observed_sequence: Sequence,
        /// Stored digest of the last event actually present.
        observed_digest: Digest,
    },
}

/// Coarse classification of an [`IntegrityIssue`], stable for API consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrityCategory {
    /// A hashed field (payload, metadata, type, version, timestamp, id,
    /// sequence) or the stored digest itself was altered.
    ModifiedEvent,
    /// `prev_digest` links do not form an unbroken chain.
    BrokenChain,
    /// Event 1 does not link to the genesis value.
    InvalidGenesis,
    /// Missing, duplicated or reordered sequence numbers.
    SequenceContinuity,
    /// The stream head disagrees with the last event (e.g. a truncated tail).
    HeadInconsistency,
    /// The history disagrees with an externally trusted checkpoint.
    CheckpointMismatch,
    /// A row cannot be decoded at all.
    MalformedRecord,
}

impl IntegrityIssue {
    /// The issue's category.
    #[must_use]
    pub fn category(&self) -> IntegrityCategory {
        match self {
            Self::DigestMismatch { .. } => IntegrityCategory::ModifiedEvent,
            Self::ChainBroken { .. } => IntegrityCategory::BrokenChain,
            Self::InvalidGenesis { .. } => IntegrityCategory::InvalidGenesis,
            Self::SequenceGap { .. } => IntegrityCategory::SequenceContinuity,
            Self::MalformedRecord { .. } => IntegrityCategory::MalformedRecord,
            Self::CheckpointMismatch { .. } => IntegrityCategory::CheckpointMismatch,
            Self::HeadMismatch { .. } => IntegrityCategory::HeadInconsistency,
        }
    }

    /// The (raw) sequence number the issue is attached to, if any.
    #[must_use]
    pub fn sequence(&self) -> Option<i64> {
        let seq = |s: &Sequence| i64::try_from(s.get()).ok();
        match self {
            Self::DigestMismatch { sequence, .. }
            | Self::ChainBroken { sequence, .. }
            | Self::CheckpointMismatch { sequence, .. } => seq(sequence),
            Self::InvalidGenesis { .. } => Some(1),
            Self::SequenceGap { found, .. } => Some(*found),
            Self::MalformedRecord { sequence, .. } => Some(*sequence),
            Self::HeadMismatch {
                recorded_sequence, ..
            } => seq(recorded_sequence),
        }
    }
}

/// A trusted reference point in a stream's history: "event `sequence` had
/// digest `digest`".
///
/// Unkeyed digests only prove that a history is *self-consistent*. A
/// checkpoint stored somewhere the database writer cannot modify (another
/// system, a signed log, printed in a release note) turns verification into
/// "consistent **and** identical up to `sequence` to what was observed
/// then". Obtain one with [`EventStore::checkpoint`](crate::EventStore::checkpoint),
/// and check it with
/// [`EventStore::verify_stream_against`](crate::EventStore::verify_stream_against).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct StreamCheckpoint {
    /// Sequence of the checkpointed event (≥ 1).
    pub sequence: Sequence,
    /// Its digest.
    pub digest: Digest,
}

/// An event referenced by an integrity issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AffectedEvent {
    /// Sequence number.
    pub sequence: Sequence,
    /// Event id, when the row was decodable.
    pub event_id: EventId,
}

/// Outcome of verifying one stream.
///
/// A report is only produced when verification itself ran to completion;
/// failures to *perform* verification (I/O, locking) are returned as
/// [`StoreError`](crate::StoreError) instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IntegrityReport {
    /// Stream that was verified.
    pub stream_id: StreamId,
    /// Number of event rows examined.
    pub events_checked: u64,
    /// Head recorded on the stream row at verification time.
    pub head: StreamHead,
    /// Defects found, in stream order, capped at [`MAX_REPORTED_ISSUES`].
    pub issues: Vec<IntegrityIssue>,
    /// Total defects found, including any beyond the cap.
    pub total_issues: u64,
    /// Decodable events mentioned by `issues`, in sequence order.
    pub affected_events: Vec<AffectedEvent>,
}

impl IntegrityReport {
    /// `true` if no defect was found.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.total_issues == 0
    }

    /// Distinct categories of the reported issues, in a stable order.
    #[must_use]
    pub fn categories(&self) -> Vec<IntegrityCategory> {
        let set: std::collections::BTreeSet<_> =
            self.issues.iter().map(IntegrityIssue::category).collect();
        set.into_iter().collect()
    }
}

/// Incremental verifier fed with a stream's rows in ascending sequence order.
pub(crate) struct ChainVerifier {
    stream_id: StreamId,
    head: StreamHead,
    events_checked: u64,
    expected_sequence: Option<Sequence>,
    /// Stored digest of the previous row; `None` if that row's digest was unreadable.
    prev_digest: Option<Digest>,
    last_seen: StreamHead,
    issues: Vec<IntegrityIssue>,
    total_issues: u64,
    affected: std::collections::BTreeMap<Sequence, EventId>,
    /// Event ids seen so far, to resolve `affected` (bounded by the issue cap).
    ids: Vec<(Sequence, EventId)>,
}

impl ChainVerifier {
    pub(crate) fn new(stream_id: StreamId, head: StreamHead) -> Self {
        Self::starting_at(stream_id, head, StreamHead::EMPTY)
    }

    /// A verifier for the rows *after* `start`, whose digest is trusted.
    pub(crate) fn starting_at(stream_id: StreamId, head: StreamHead, start: StreamHead) -> Self {
        Self {
            stream_id,
            head,
            events_checked: 0,
            expected_sequence: start.sequence.next(),
            prev_digest: Some(start.digest),
            last_seen: start,
            issues: Vec::new(),
            total_issues: 0,
            affected: std::collections::BTreeMap::new(),
            ids: Vec::new(),
        }
    }

    fn report(&mut self, issue: IntegrityIssue) {
        self.total_issues += 1;
        if self.issues.len() < MAX_REPORTED_ISSUES {
            if let Some(sequence) = issue.sequence().and_then(Sequence::from_i64)
                && let Some((_, id)) = self.ids.iter().rev().find(|(s, _)| *s == sequence)
            {
                self.affected.insert(sequence, *id);
            }
            self.issues.push(issue);
        }
    }

    /// Adds a checkpoint finding (computed by the caller).
    pub(crate) fn report_checkpoint(&mut self, issue: IntegrityIssue) {
        self.report(issue);
    }

    pub(crate) fn last_seen(&self) -> StreamHead {
        self.last_seen
    }

    pub(crate) fn push(&mut self, record: &RawEventRecord) {
        self.events_checked += 1;

        if let Some(expected) = self.expected_sequence
            && i64::try_from(expected.get()).ok() != Some(record.sequence)
        {
            self.report(IntegrityIssue::SequenceGap {
                expected,
                found: record.sequence,
            });
        }
        self.expected_sequence = Sequence::from_i64(record.sequence).and_then(Sequence::next);

        let event = match record.decode() {
            Ok(event) => event,
            Err(reason) => {
                self.report(IntegrityIssue::MalformedRecord {
                    sequence: record.sequence,
                    reason,
                });
                self.prev_digest = Digest::from_slice(&record.digest).ok();
                return;
            }
        };

        // Remember only the current id: issues are reported while handling
        // this row, so that is all `report` needs to resolve.
        self.ids.clear();
        self.ids.push((event.sequence, event.event_id));
        let computed = record.digest_input(&event).compute();
        if computed != event.digest {
            self.report(IntegrityIssue::DigestMismatch {
                sequence: event.sequence,
                stored: event.digest,
                computed,
            });
        }
        if event.sequence == Sequence::FIRST && event.prev_digest != Digest::GENESIS {
            self.report(IntegrityIssue::InvalidGenesis {
                stored_prev: event.prev_digest,
            });
        } else if let Some(expected_prev) = self.prev_digest
            && expected_prev != event.prev_digest
        {
            self.report(IntegrityIssue::ChainBroken {
                sequence: event.sequence,
                expected_prev,
                stored_prev: event.prev_digest,
            });
        }
        // Chain from the *stored* digest so that one altered event yields one
        // digest mismatch rather than a cascade of chain breaks.
        self.prev_digest = Some(event.digest);
        self.last_seen = StreamHead {
            sequence: event.sequence,
            digest: event.digest,
        };
    }

    pub(crate) fn finish(mut self) -> IntegrityReport {
        if self.last_seen != self.head {
            let issue = IntegrityIssue::HeadMismatch {
                recorded_sequence: self.head.sequence,
                recorded_digest: self.head.digest,
                observed_sequence: self.last_seen.sequence,
                observed_digest: self.last_seen.digest,
            };
            self.report(issue);
        }
        self.into_report()
    }

    /// Finishes without comparing against the head (range verification).
    pub(crate) fn finish_range(self) -> IntegrityReport {
        self.into_report()
    }

    fn into_report(self) -> IntegrityReport {
        IntegrityReport {
            stream_id: self.stream_id,
            events_checked: self.events_checked,
            head: self.head,
            issues: self.issues,
            total_issues: self.total_issues,
            affected_events: self
                .affected
                .into_iter()
                .map(|(sequence, event_id)| AffectedEvent { sequence, event_id })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::canonical::to_canonical_string;
    use serde_json::json;

    fn input<'a>(payload: &'a str, metadata: &'a str) -> EventDigestInput<'a> {
        EventDigestInput {
            stream_id: "account-1",
            sequence: Sequence::FIRST,
            event_id: EventId::from_uuid(uuid::Uuid::from_u128(
                0x0192_0000_0000_7000_8000_0000_0000_0001,
            ))
            .unwrap(),
            event_type: "account.opened",
            schema_version: SchemaVersion::INITIAL,
            recorded_at: Timestamp::from_unix_micros(1_700_000_000_000_000).unwrap(),
            payload,
            metadata,
            prev_digest: Digest::GENESIS,
        }
    }

    /// Golden vector, cross-checked against an independent Python `hashlib`
    /// implementation of the construction. If this changes, the digest
    /// domain tag must be bumped and a migration path provided.
    #[test]
    fn golden_digest() {
        let payload = to_canonical_string(&json!({"owner": "alice", "balance": 0}));
        assert_eq!(payload, r#"{"balance":0,"owner":"alice"}"#);
        let digest = input(&payload, "{}").compute();
        assert_eq!(
            digest.to_hex(),
            "152b68932aef18f46f0f343f6ed38061c0fc216f498364223225687c6e48e464"
        );
    }

    #[test]
    fn every_field_affects_the_digest() {
        let base = input("{}", "{}");
        let reference = base.compute();
        let mut variants = Vec::new();
        variants.push(EventDigestInput {
            stream_id: "account-2",
            ..base
        });
        variants.push(EventDigestInput {
            sequence: Sequence::new(2).unwrap(),
            ..base
        });
        variants.push(EventDigestInput {
            event_id: EventId::generate(),
            ..base
        });
        variants.push(EventDigestInput {
            event_type: "account.closed",
            ..base
        });
        variants.push(EventDigestInput {
            schema_version: SchemaVersion::new(2).unwrap(),
            ..base
        });
        variants.push(EventDigestInput {
            recorded_at: Timestamp::from_unix_micros(1).unwrap(),
            ..base
        });
        variants.push(EventDigestInput {
            payload: "[]",
            ..base
        });
        variants.push(EventDigestInput {
            metadata: r#"{"a":1}"#,
            ..base
        });
        variants.push(EventDigestInput {
            prev_digest: Digest::from_bytes([1; 32]),
            ..base
        });
        for variant in variants {
            assert_ne!(variant.compute(), reference, "{variant:?}");
        }
    }

    #[test]
    fn length_prefixing_prevents_field_boundary_ambiguity() {
        // Moving bytes between adjacent variable-length fields must change the digest.
        let a = EventDigestInput {
            payload: "1",
            metadata: "{}",
            ..input("", "")
        };
        let b = EventDigestInput {
            payload: "1{",
            metadata: "}",
            ..input("", "")
        };
        assert_ne!(a.compute(), b.compute());
    }
}
