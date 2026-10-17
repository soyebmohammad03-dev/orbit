//! The SQLite-backed event store.
//!
//! [`EventStore`] is the application-facing API. It owns one SQLite
//! connection guarded by a mutex, so a single store can be shared between
//! threads (`EventStore: Send + Sync`). Several stores — in one process or
//! many — may open the same file; SQLite's locking serialises their writes.
//!
//! # Append semantics
//!
//! [`EventStore::append`] runs entirely inside one `BEGIN IMMEDIATE`
//! transaction, which takes SQLite's write lock up front:
//!
//! 1. validate and canonicalise the request (no lock held yet);
//! 2. begin the transaction, waiting up to the busy timeout for the lock;
//! 3. read the stream head (`StreamNotFound` if the stream is absent);
//! 4. check the [`ExpectedSequence`] precondition (`SequenceConflict`);
//! 5. reject a caller-supplied event id that already exists (`DuplicateEventId`);
//! 6. assign `sequence = head + 1`, compute the digest chained to the head digest;
//! 7. insert the event and advance the stream head;
//! 8. commit.
//!
//! Any failure before the commit rolls the whole transaction back. Because
//! the write lock is held from step 2, no other writer can observe or change
//! the head between reading and advancing it.

mod schema;

use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};

use crate::config::StoreConfig;
use crate::domain::canonical::{object_to_canonical_string, to_canonical_string};
use crate::domain::{
    AppendRequest, Digest, EventId, ExpectedSequence, Metadata, Sequence, StoredEvent, Stream,
    StreamHead, StreamId, Timestamp,
};
use crate::error::{Result, StoreError, ValidationError};
use crate::integrity::{ChainVerifier, EventDigestInput, IntegrityReport};
use crate::record::RawEventRecord;

/// Rows fetched per query during verification.
const VERIFY_PAGE_SIZE: usize = 1_000;

const EVENT_COLUMNS: &str = "stream_id, sequence, event_id, event_type, schema_version, \
                             recorded_at, payload, metadata, prev_digest, digest";

/// Persistent, append-only event store backed by a SQLite file.
#[derive(Debug)]
pub struct EventStore {
    conn: Mutex<Connection>,
    config: StoreConfig,
}

impl EventStore {
    /// Opens (creating if necessary) the database described by `config`,
    /// applying any pending schema migrations.
    ///
    /// # Errors
    /// Fails if the configuration is invalid, the file cannot be opened, it
    /// is not an ORBIT database, or it was created by a newer schema version.
    pub fn open(config: StoreConfig) -> Result<Self> {
        config.validate()?;
        let conn = schema::open(&config)?;
        tracing::debug!(path = %config.path.display(), "opened event store");
        Ok(Self {
            conn: Mutex::new(conn),
            config,
        })
    }

    /// The configuration this store was opened with.
    #[must_use]
    pub fn config(&self) -> &StoreConfig {
        &self.config
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock cannot leave a transaction open:
        // rusqlite rolls back uncommitted transactions when they are dropped
        // during unwinding. The connection is therefore safe to reuse.
        self.conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Creates an empty stream.
    ///
    /// # Errors
    /// [`StoreError::StreamAlreadyExists`] if `id` is taken;
    /// [`ValidationError::MetadataTooLarge`] if the metadata exceeds the limit.
    pub fn create_stream(&self, id: &StreamId, metadata: Metadata) -> Result<Stream> {
        let metadata_text = object_to_canonical_string(&metadata);
        check_size(
            metadata_text.len(),
            self.config.limits.max_metadata_bytes,
            |actual, max| ValidationError::MetadataTooLarge { actual, max },
        )?;
        let created_at = Timestamp::now()?;

        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let inserted = tx.execute(
            "INSERT INTO streams (stream_id, created_at, metadata, head_sequence, head_digest)
             VALUES (?1, ?2, ?3, 0, ?4)
             ON CONFLICT (stream_id) DO NOTHING",
            params![
                id.as_str(),
                created_at.unix_micros(),
                metadata_text,
                Digest::GENESIS.as_bytes().as_slice()
            ],
        )?;
        if inserted == 0 {
            return Err(StoreError::StreamAlreadyExists(id.clone()));
        }
        tx.commit()?;
        tracing::debug!(stream = %id, "created stream");

        Ok(Stream {
            id: id.clone(),
            created_at,
            metadata,
            head: StreamHead::EMPTY,
        })
    }

    /// Returns the stream's metadata and current head.
    ///
    /// # Errors
    /// [`StoreError::StreamNotFound`] if the stream does not exist.
    pub fn stream(&self, id: &StreamId) -> Result<Stream> {
        let conn = self.conn();
        load_stream(&conn, id)?.ok_or_else(|| StoreError::StreamNotFound(id.clone()))
    }

    /// Returns `true` if the stream exists.
    ///
    /// # Errors
    /// Only on database failure.
    pub fn stream_exists(&self, id: &StreamId) -> Result<bool> {
        let conn = self.conn();
        Ok(conn
            .query_row(
                "SELECT 1 FROM streams WHERE stream_id = ?1",
                [id.as_str()],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Returns the stream's head: its last sequence and digest.
    ///
    /// # Errors
    /// [`StoreError::StreamNotFound`] if the stream does not exist.
    pub fn head(&self, id: &StreamId) -> Result<StreamHead> {
        let conn = self.conn();
        load_head(&conn, id)?.ok_or_else(|| StoreError::StreamNotFound(id.clone()))
    }

    /// Atomically appends one event. See the [module docs](crate::store) for the
    /// exact transaction semantics.
    ///
    /// # Errors
    /// [`StoreError::Validation`], [`StoreError::StreamNotFound`],
    /// [`StoreError::SequenceConflict`], [`StoreError::DuplicateEventId`],
    /// [`StoreError::SequenceExhausted`] or [`StoreError::Busy`]. On any error
    /// nothing is written.
    pub fn append(&self, stream: &StreamId, request: AppendRequest) -> Result<StoredEvent> {
        let limits = self.config.limits;
        let payload_text = to_canonical_string(&request.payload);
        check_size(
            payload_text.len(),
            limits.max_payload_bytes,
            |actual, max| ValidationError::PayloadTooLarge { actual, max },
        )?;
        let metadata_text = object_to_canonical_string(&request.metadata);
        check_size(
            metadata_text.len(),
            limits.max_metadata_bytes,
            |actual, max| ValidationError::MetadataTooLarge { actual, max },
        )?;
        let event_id = request.event_id.unwrap_or_else(EventId::generate);
        let recorded_at = Timestamp::now()?;

        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let head =
            load_head(&tx, stream)?.ok_or_else(|| StoreError::StreamNotFound(stream.clone()))?;
        if let ExpectedSequence::Exact(expected) = request.expected_sequence
            && expected != head.sequence
        {
            return Err(StoreError::SequenceConflict {
                stream: stream.clone(),
                expected,
                actual: head.sequence,
            });
        }
        if request.event_id.is_some() {
            let exists = tx
                .query_row(
                    "SELECT 1 FROM events WHERE event_id = ?1",
                    [event_id.as_bytes().as_slice()],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if exists {
                return Err(StoreError::DuplicateEventId(event_id));
            }
        }
        let sequence = head
            .sequence
            .next()
            .ok_or_else(|| StoreError::SequenceExhausted(stream.clone()))?;

        let digest = EventDigestInput {
            stream_id: stream.as_str(),
            sequence,
            event_id,
            event_type: request.event_type.as_str(),
            schema_version: request.schema_version,
            recorded_at,
            payload: &payload_text,
            metadata: &metadata_text,
            prev_digest: head.digest,
        }
        .compute();

        tx.execute(
            "INSERT INTO events (stream_id, sequence, event_id, event_type, schema_version,
                                 recorded_at, payload, metadata, prev_digest, digest)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                stream.as_str(),
                sequence.to_i64(),
                event_id.as_bytes().as_slice(),
                request.event_type.as_str(),
                i64::from(request.schema_version.get()),
                recorded_at.unix_micros(),
                payload_text,
                metadata_text,
                head.digest.as_bytes().as_slice(),
                digest.as_bytes().as_slice(),
            ],
        )?;
        let advanced = tx.execute(
            "UPDATE streams SET head_sequence = ?1, head_digest = ?2
             WHERE stream_id = ?3 AND head_sequence = ?4",
            params![
                sequence.to_i64(),
                digest.as_bytes().as_slice(),
                stream.as_str(),
                head.sequence.to_i64()
            ],
        )?;
        if advanced != 1 {
            // Unreachable while the write lock is held; fail closed regardless.
            return Err(StoreError::SequenceConflict {
                stream: stream.clone(),
                expected: head.sequence,
                actual: load_head(&tx, stream)?.map_or(head.sequence, |h| h.sequence),
            });
        }
        tx.commit()?;
        tracing::trace!(stream = %stream, %sequence, "appended event");

        Ok(StoredEvent {
            event_id,
            stream_id: stream.clone(),
            sequence,
            event_type: request.event_type,
            schema_version: request.schema_version,
            recorded_at,
            payload: request.payload,
            metadata: request.metadata,
            prev_digest: head.digest,
            digest,
        })
    }

    /// Returns the event at `sequence`.
    ///
    /// # Errors
    /// [`StoreError::StreamNotFound`], [`StoreError::EventNotFound`], or
    /// [`StoreError::CorruptRecord`] if the stored row is malformed.
    pub fn event(&self, stream: &StreamId, sequence: Sequence) -> Result<StoredEvent> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        if load_head(&tx, stream)?.is_none() {
            return Err(StoreError::StreamNotFound(stream.clone()));
        }
        let raw = tx
            .query_row(
                &format!(
                    "SELECT {EVENT_COLUMNS} FROM events WHERE stream_id = ?1 AND sequence = ?2"
                ),
                params![stream.as_str(), sequence.to_i64()],
                read_raw,
            )
            .optional()?
            .ok_or_else(|| StoreError::EventNotFound {
                stream: stream.clone(),
                sequence,
            })?;
        decode(&raw)
    }

    /// Returns every event in the stream in ascending sequence order.
    ///
    /// Loads the whole stream into memory; prefer [`read_after`](Self::read_after)
    /// for large streams.
    ///
    /// # Errors
    /// [`StoreError::StreamNotFound`] or [`StoreError::CorruptRecord`].
    pub fn read_stream(&self, stream: &StreamId) -> Result<Vec<StoredEvent>> {
        self.read_page(stream, Sequence::ZERO, None)
    }

    /// Returns up to `limit` events with sequence strictly greater than
    /// `after`, in ascending sequence order. Pass [`Sequence::ZERO`] to read
    /// from the beginning; pass the last returned sequence to continue.
    ///
    /// # Errors
    /// [`ValidationError::ZeroLimit`], [`StoreError::StreamNotFound`] or
    /// [`StoreError::CorruptRecord`].
    pub fn read_after(
        &self,
        stream: &StreamId,
        after: Sequence,
        limit: usize,
    ) -> Result<Vec<StoredEvent>> {
        if limit == 0 {
            return Err(ValidationError::ZeroLimit.into());
        }
        self.read_page(stream, after, Some(limit))
    }

    fn read_page(
        &self,
        stream: &StreamId,
        after: Sequence,
        limit: Option<usize>,
    ) -> Result<Vec<StoredEvent>> {
        let mut conn = self.conn();
        // A read transaction gives a consistent snapshot of the stream row and its events.
        let tx = conn.transaction()?;
        if load_head(&tx, stream)?.is_none() {
            return Err(StoreError::StreamNotFound(stream.clone()));
        }
        let raws = select_after(&tx, stream, after, limit)?;
        raws.iter().map(decode).collect()
    }

    /// Verifies the integrity of every stored event in the stream and the
    /// stream head. See [`crate::integrity`] for what is and is not detected.
    ///
    /// Runs in a single read transaction, so it checks one consistent
    /// snapshot even while other connections append.
    ///
    /// # Errors
    /// [`StoreError::StreamNotFound`], or a database error if verification
    /// could not be performed. Integrity *defects* are reported in the
    /// returned [`IntegrityReport`], not as errors.
    pub fn verify_stream(&self, stream: &StreamId) -> Result<IntegrityReport> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let head =
            load_head(&tx, stream)?.ok_or_else(|| StoreError::StreamNotFound(stream.clone()))?;
        let mut verifier = ChainVerifier::new(stream.clone(), head);
        let mut after = Sequence::ZERO;
        loop {
            let page = select_after(&tx, stream, after, Some(VERIFY_PAGE_SIZE))?;
            for raw in &page {
                verifier.push(raw);
            }
            match page.last() {
                Some(last) if page.len() == VERIFY_PAGE_SIZE => {
                    // Stored sequences are >= 1 (CHECK constraint); a negative
                    // value would already have been reported as malformed.
                    match Sequence::from_i64(last.sequence) {
                        Some(next_after) if next_after > after => after = next_after,
                        _ => break,
                    }
                }
                _ => break,
            }
        }
        let report = verifier.finish();
        if report.is_valid() {
            tracing::debug!(stream = %stream, events = report.events_checked, "stream verified");
        } else {
            tracing::warn!(stream = %stream, issues = report.total_issues, "integrity issues found");
        }
        Ok(report)
    }
}

fn check_size(
    actual: usize,
    max: usize,
    error: impl FnOnce(usize, usize) -> ValidationError,
) -> Result<(), ValidationError> {
    if actual > max {
        Err(error(actual, max))
    } else {
        Ok(())
    }
}

fn load_head(conn: &Connection, id: &StreamId) -> Result<Option<StreamHead>> {
    let row = conn
        .query_row(
            "SELECT head_sequence, head_digest FROM streams WHERE stream_id = ?1",
            [id.as_str()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .optional()?;
    row.map(|(sequence, digest)| {
        let corrupt = |reason: String| StoreError::CorruptRecord {
            stream: id.to_string(),
            sequence,
            reason,
        };
        Ok(StreamHead {
            sequence: Sequence::from_i64(sequence)
                .ok_or_else(|| corrupt("negative head sequence".to_owned()))?,
            digest: Digest::from_slice(&digest)
                .map_err(|e| corrupt(format!("head digest: {e}")))?,
        })
    })
    .transpose()
}

fn load_stream(conn: &Connection, id: &StreamId) -> Result<Option<Stream>> {
    let row = conn
        .query_row(
            "SELECT created_at, metadata FROM streams WHERE stream_id = ?1",
            [id.as_str()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((created_at, metadata)) = row else {
        return Ok(None);
    };
    let corrupt = |reason: String| StoreError::CorruptRecord {
        stream: id.to_string(),
        sequence: 0,
        reason,
    };
    let created_at =
        Timestamp::from_unix_micros(created_at).map_err(|e| corrupt(format!("created_at: {e}")))?;
    let metadata: Metadata =
        serde_json::from_str(&metadata).map_err(|e| corrupt(format!("stream metadata: {e}")))?;
    let head = load_head(conn, id)?.ok_or_else(|| StoreError::StreamNotFound(id.clone()))?;
    Ok(Some(Stream {
        id: id.clone(),
        created_at,
        metadata,
        head,
    }))
}

fn select_after(
    conn: &Connection,
    stream: &StreamId,
    after: Sequence,
    limit: Option<usize>,
) -> Result<Vec<RawEventRecord>> {
    // SQLite treats a negative LIMIT as "no limit".
    let limit = limit.map_or(-1, |l| i64::try_from(l).unwrap_or(i64::MAX));
    let mut statement = conn.prepare_cached(&format!(
        "SELECT {EVENT_COLUMNS} FROM events
         WHERE stream_id = ?1 AND sequence > ?2
         ORDER BY sequence ASC
         LIMIT ?3"
    ))?;
    let rows = statement.query_map(params![stream.as_str(), after.to_i64(), limit], read_raw)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn read_raw(row: &Row<'_>) -> rusqlite::Result<RawEventRecord> {
    Ok(RawEventRecord {
        stream_id: row.get(0)?,
        sequence: row.get(1)?,
        event_id: row.get(2)?,
        event_type: row.get(3)?,
        schema_version: row.get(4)?,
        recorded_at: row.get(5)?,
        payload: row.get(6)?,
        metadata: row.get(7)?,
        prev_digest: row.get(8)?,
        digest: row.get(9)?,
    })
}

fn decode(raw: &RawEventRecord) -> Result<StoredEvent> {
    raw.decode().map_err(|reason| StoreError::CorruptRecord {
        stream: raw.stream_id.clone(),
        sequence: raw.sequence,
        reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_store_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<EventStore>();
    }
}
