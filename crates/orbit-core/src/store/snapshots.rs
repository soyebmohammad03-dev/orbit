//! Snapshot persistence.
//!
//! These methods store and load snapshots as opaque JSON states bound to a
//! history position. They know nothing about projections; building states
//! and deciding which snapshot to trust is the job of
//! [`Reconstructor`](crate::reconstruct::Reconstructor).

use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde_json::Value;

use super::{EventStore, load_head};
use crate::domain::canonical::to_canonical_string;
use crate::domain::{Digest, Sequence, StreamId, Timestamp};
use crate::error::{Result, SnapshotConflict, StoreError, ValidationError};
use crate::snapshot::{
    MAX_SNAPSHOT_STATE_BYTES, ProjectionKey, RawSnapshotRecord, Snapshot, SnapshotCheck,
    SnapshotDigestInput, SnapshotId, SnapshotProblem,
};

const SNAPSHOT_COLUMNS: &str = "snapshot_id, stream_id, sequence, projection_id, \
     projection_version, event_digest, state, state_digest, created_at";

/// A snapshot to be stored.
#[derive(Debug, Clone, PartialEq)]
pub struct NewSnapshot {
    /// Stream the state was reconstructed from.
    pub stream_id: StreamId,
    /// Last event applied to produce `state` (≥ 1).
    pub sequence: Sequence,
    /// Digest of the event at `sequence` that was actually applied. The store
    /// rejects the snapshot unless the stored event still has this digest.
    pub event_digest: Digest,
    /// Projection that produced `state`.
    pub projection: ProjectionKey,
    /// The state.
    pub state: Value,
}

/// Result of [`EventStore::save_snapshot`].
#[derive(Debug, Clone, PartialEq)]
pub struct SavedSnapshot {
    /// The stored snapshot (the pre-existing one if `created` is `false`).
    pub snapshot: Snapshot,
    /// `false` if an identical snapshot already existed and was returned instead.
    pub created: bool,
}

impl EventStore {
    /// Stores a snapshot in one `BEGIN IMMEDIATE` transaction.
    ///
    /// Saving is idempotent: if a snapshot for the same stream, projection
    /// key and sequence already exists with the same state, that snapshot is
    /// returned with `created == false`.
    ///
    /// # Errors
    /// * [`ValidationError::SnapshotAtZero`] / [`ValidationError::SnapshotTooLarge`];
    /// * [`StoreError::StreamNotFound`];
    /// * [`StoreError::SnapshotHistoryMismatch`] if `sequence` is beyond the
    ///   head or the stored event's digest differs from `event_digest`;
    /// * [`StoreError::SnapshotConflict`] if a snapshot with the same key and
    ///   sequence but a different state exists.
    pub fn save_snapshot(&self, new: NewSnapshot) -> Result<SavedSnapshot> {
        if new.sequence == Sequence::ZERO {
            return Err(ValidationError::SnapshotAtZero.into());
        }
        let state_text = to_canonical_string(&new.state);
        if state_text.len() > MAX_SNAPSHOT_STATE_BYTES {
            return Err(ValidationError::SnapshotTooLarge {
                actual: state_text.len(),
                max: MAX_SNAPSHOT_STATE_BYTES,
            }
            .into());
        }
        let id = SnapshotId::generate();
        let created_at = Timestamp::now()?;
        let state_digest = SnapshotDigestInput {
            snapshot_id: id,
            stream_id: new.stream_id.as_str(),
            sequence: new.sequence,
            projection_id: new.projection.id.as_str(),
            projection_version: new.projection.version,
            event_digest: new.event_digest,
            created_at,
            state: &state_text,
        }
        .compute();

        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let head = load_head(&tx, &new.stream_id)?
            .ok_or_else(|| StoreError::StreamNotFound(new.stream_id.clone()))?;
        let mismatch = |reason: String| StoreError::SnapshotHistoryMismatch {
            stream: new.stream_id.clone(),
            sequence: new.sequence,
            reason,
        };
        if new.sequence > head.sequence {
            return Err(mismatch(format!("stream head is {}", head.sequence)));
        }
        let stored = event_digest_at(&tx, &new.stream_id, new.sequence)?;
        if stored != Some(new.event_digest) {
            return Err(mismatch(format!(
                "snapshot built on event digest {}, stored event has {}",
                new.event_digest,
                stored.map_or_else(|| "none".to_owned(), |d| d.to_hex())
            )));
        }

        let inserted = tx.execute(
            "INSERT INTO snapshots (snapshot_id, stream_id, sequence, projection_id,
                                    projection_version, event_digest, state, state_digest, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT (stream_id, projection_id, projection_version, sequence) DO NOTHING",
            params![
                id.as_bytes().as_slice(),
                new.stream_id.as_str(),
                new.sequence.to_i64(),
                new.projection.id.as_str(),
                i64::from(new.projection.version),
                new.event_digest.as_bytes().as_slice(),
                state_text,
                state_digest.as_bytes().as_slice(),
                created_at.unix_micros(),
            ],
        )?;
        if inserted == 1 {
            tx.commit()?;
            tracing::debug!(stream = %new.stream_id, sequence = %new.sequence,
                projection = %new.projection, snapshot = %id, "saved snapshot");
            return Ok(SavedSnapshot {
                snapshot: Snapshot {
                    id,
                    stream_id: new.stream_id,
                    sequence: new.sequence,
                    projection: new.projection,
                    event_digest: new.event_digest,
                    state: new.state,
                    state_digest,
                    created_at,
                },
                created: true,
            });
        }

        // A snapshot with this key already exists: idempotent if the state is identical.
        let raw = tx.query_row(
            &format!(
                "SELECT {SNAPSHOT_COLUMNS} FROM snapshots
                     WHERE stream_id = ?1 AND projection_id = ?2
                       AND projection_version = ?3 AND sequence = ?4"
            ),
            params![
                new.stream_id.as_str(),
                new.projection.id.as_str(),
                i64::from(new.projection.version),
                new.sequence.to_i64()
            ],
            read_raw_snapshot,
        )?;
        let existing = decode_snapshot(&raw)?;
        tx.commit()?;
        if raw.state == state_text {
            Ok(SavedSnapshot {
                snapshot: existing,
                created: false,
            })
        } else {
            Err(StoreError::SnapshotConflict(Box::new(SnapshotConflict {
                stream: new.stream_id,
                projection: new.projection,
                sequence: new.sequence,
                existing: existing.id,
                existing_digest: existing.state_digest,
                offered_digest: state_digest,
            })))
        }
    }

    /// Loads a snapshot by id. Does not verify it; see [`check_snapshot`](Self::check_snapshot).
    ///
    /// # Errors
    /// [`StoreError::SnapshotNotFound`] or [`StoreError::CorruptSnapshot`].
    pub fn snapshot(&self, id: SnapshotId) -> Result<Snapshot> {
        let conn = self.conn();
        let raw = load_raw_snapshot(&conn, id)?.ok_or(StoreError::SnapshotNotFound(id))?;
        decode_snapshot(&raw)
    }

    /// Lists every snapshot of a stream, ordered by sequence, then
    /// projection id, then projection version.
    ///
    /// # Errors
    /// [`StoreError::StreamNotFound`] or [`StoreError::CorruptSnapshot`].
    pub fn list_snapshots(&self, stream: &StreamId) -> Result<Vec<Snapshot>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        if load_head(&tx, stream)?.is_none() {
            return Err(StoreError::StreamNotFound(stream.clone()));
        }
        let mut statement = tx.prepare_cached(&format!(
            "SELECT {SNAPSHOT_COLUMNS} FROM snapshots WHERE stream_id = ?1
             ORDER BY sequence ASC, projection_id ASC, projection_version ASC"
        ))?;
        let raws = statement
            .query_map([stream.as_str()], read_raw_snapshot)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        raws.iter().map(decode_snapshot).collect()
    }

    /// Checks a snapshot's integrity digest and its binding to the stored
    /// history (the event at its sequence must still have the recorded digest).
    ///
    /// # Errors
    /// [`StoreError::SnapshotNotFound`], or a database error. Defects are
    /// reported as [`SnapshotCheck::Unusable`], not as errors.
    pub fn check_snapshot(&self, id: SnapshotId) -> Result<SnapshotCheck> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let raw = load_raw_snapshot(&tx, id)?.ok_or(StoreError::SnapshotNotFound(id))?;
        check_raw(&tx, &raw)
    }

    /// Up to `limit` raw snapshot rows for `projection` on `stream` with
    /// sequence at most `at_most`, newest first. Raw rows, so that the caller
    /// can report malformed ones without aborting selection.
    pub(crate) fn snapshot_candidates(
        &self,
        stream: &StreamId,
        projection: &ProjectionKey,
        at_most: Sequence,
        limit: usize,
    ) -> Result<Vec<RawSnapshotRecord>> {
        let conn = self.conn();
        let mut statement = conn.prepare_cached(&format!(
            "SELECT {SNAPSHOT_COLUMNS} FROM snapshots
             WHERE stream_id = ?1 AND projection_id = ?2 AND projection_version = ?3
               AND sequence <= ?4
             ORDER BY sequence DESC
             LIMIT ?5"
        ))?;
        let rows = statement.query_map(
            params![
                stream.as_str(),
                projection.id.as_str(),
                i64::from(projection.version),
                at_most.to_i64(),
                i64::try_from(limit).unwrap_or(i64::MAX)
            ],
            read_raw_snapshot,
        )?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Storage-level validation of a raw snapshot row.
    pub(crate) fn check_snapshot_record(&self, raw: &RawSnapshotRecord) -> Result<SnapshotCheck> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        check_raw(&tx, raw)
    }

    pub(crate) fn load_snapshot_record(&self, id: SnapshotId) -> Result<RawSnapshotRecord> {
        let conn = self.conn();
        load_raw_snapshot(&conn, id)?.ok_or(StoreError::SnapshotNotFound(id))
    }
}

fn check_raw(conn: &Connection, raw: &RawSnapshotRecord) -> Result<SnapshotCheck> {
    let unusable = |problem| SnapshotCheck::Unusable {
        sequence: raw.sequence,
        problem,
    };
    let snapshot = match raw.decode() {
        Ok(snapshot) => snapshot,
        Err(reason) => return Ok(unusable(SnapshotProblem::Malformed { reason })),
    };
    let computed = raw.computed_digest(&snapshot);
    if computed != snapshot.state_digest {
        return Ok(unusable(SnapshotProblem::IntegrityMismatch {
            stored: snapshot.state_digest,
            computed,
        }));
    }
    let actual = event_digest_at(conn, &snapshot.stream_id, snapshot.sequence)?;
    if actual != Some(snapshot.event_digest) {
        return Ok(unusable(SnapshotProblem::HistoryMismatch {
            expected: snapshot.event_digest,
            actual,
        }));
    }
    Ok(SnapshotCheck::Intact { snapshot })
}

fn event_digest_at(
    conn: &Connection,
    stream: &StreamId,
    sequence: Sequence,
) -> Result<Option<Digest>> {
    let bytes: Option<Vec<u8>> = conn
        .query_row(
            "SELECT digest FROM events WHERE stream_id = ?1 AND sequence = ?2",
            params![stream.as_str(), sequence.to_i64()],
            |row| row.get(0),
        )
        .optional()?;
    // A malformed stored digest matches nothing: treat it as no usable event.
    Ok(bytes.and_then(|b| Digest::from_slice(&b).ok()))
}

fn load_raw_snapshot(conn: &Connection, id: SnapshotId) -> Result<Option<RawSnapshotRecord>> {
    Ok(conn
        .query_row(
            &format!("SELECT {SNAPSHOT_COLUMNS} FROM snapshots WHERE snapshot_id = ?1"),
            [id.as_bytes().as_slice()],
            read_raw_snapshot,
        )
        .optional()?)
}

fn read_raw_snapshot(row: &Row<'_>) -> rusqlite::Result<RawSnapshotRecord> {
    Ok(RawSnapshotRecord {
        snapshot_id: row.get(0)?,
        stream_id: row.get(1)?,
        sequence: row.get(2)?,
        projection_id: row.get(3)?,
        projection_version: row.get(4)?,
        event_digest: row.get(5)?,
        state: row.get(6)?,
        state_digest: row.get(7)?,
        created_at: row.get(8)?,
    })
}

fn decode_snapshot(raw: &RawSnapshotRecord) -> Result<Snapshot> {
    raw.decode().map_err(|reason| StoreError::CorruptSnapshot {
        snapshot: raw.display_id(),
        reason,
    })
}
