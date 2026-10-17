//! Store-wide verification and trusted checkpoints.

use std::time::Instant;

use rusqlite::params;
use serde::Serialize;

use super::{EventStore, feed_verifier, load_head, schema};
use crate::domain::{Sequence, StreamId};
use crate::error::{Result, StoreError};
use crate::integrity::{ChainVerifier, IntegrityIssue, IntegrityReport, StreamCheckpoint};
use crate::snapshot::{SnapshotCheck, SnapshotProblem};

/// A snapshot that failed storage-level validation during [`EventStore::verify_store`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SnapshotIntegrityIssue {
    /// Snapshot id as stored.
    pub snapshot: String,
    /// Stream as stored.
    pub stream: String,
    /// Sequence as stored.
    pub sequence: i64,
    /// What is wrong.
    pub problem: SnapshotProblem,
}

/// Result of [`EventStore::verify_store`]: one consistent snapshot of the whole database.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StoreIntegrityReport {
    /// Schema version (`PRAGMA user_version`).
    pub schema_version: i64,
    /// Output of SQLite's `PRAGMA integrity_check`; `["ok"]` when the file
    /// structure (b-trees, indexes, constraints) is sound.
    pub sqlite_integrity: Vec<String>,
    /// Rows violating foreign keys (`PRAGMA foreign_key_check`).
    pub foreign_key_violations: u64,
    /// Protective triggers that are missing (the file was modified outside ORBIT).
    pub missing_safeguards: Vec<String>,
    /// One report per stream, ordered by stream id.
    pub streams: Vec<IntegrityReport>,
    /// Snapshots examined.
    pub snapshots_checked: u64,
    /// Snapshots whose digest or history binding is broken.
    pub snapshot_issues: Vec<SnapshotIntegrityIssue>,
    /// Replay runs examined.
    pub replay_runs_checked: u64,
    /// Replay runs whose record digest or contents are invalid.
    pub replay_run_issues: Vec<ReplayRecordIssue>,
    /// Wall-clock duration in microseconds.
    pub elapsed_micros: u64,
}

/// A replay run that failed validation during [`EventStore::verify_store`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReplayRecordIssue {
    /// Run id as stored.
    pub run: String,
    /// What is wrong.
    pub problem: String,
}

impl StoreIntegrityReport {
    /// `true` if every check passed.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.sqlite_integrity == ["ok"]
            && self.foreign_key_violations == 0
            && self.missing_safeguards.is_empty()
            && self.snapshot_issues.is_empty()
            && self.replay_run_issues.is_empty()
            && self.streams.iter().all(IntegrityReport::is_valid)
    }

    /// Streams with at least one issue.
    pub fn invalid_streams(&self) -> impl Iterator<Item = &IntegrityReport> {
        self.streams.iter().filter(|r| !r.is_valid())
    }
}

impl EventStore {
    /// Lists every stream id in ascending order.
    ///
    /// # Errors
    /// Database failures, or [`StoreError::CorruptRecord`] for an invalid stored id.
    pub fn list_streams(&self) -> Result<Vec<StreamId>> {
        let conn = self.conn();
        let mut statement =
            conn.prepare_cached("SELECT stream_id FROM streams ORDER BY stream_id")?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter()
            .map(|id| {
                StreamId::new(id.as_str()).map_err(|e| StoreError::CorruptRecord {
                    stream: id.clone(),
                    sequence: 0,
                    reason: format!("stream id: {e}"),
                })
            })
            .collect()
    }

    /// Records the current head of a stream as a [`StreamCheckpoint`] to be
    /// stored outside the database.
    ///
    /// # Errors
    /// [`StoreError::StreamNotFound`], or [`StoreError::EventNotFound`] for an empty stream
    /// (there is nothing to checkpoint).
    pub fn checkpoint(&self, stream: &StreamId) -> Result<StreamCheckpoint> {
        let head = self.head(stream)?;
        if head.is_empty() {
            return Err(StoreError::EventNotFound {
                stream: stream.clone(),
                sequence: Sequence::FIRST,
            });
        }
        Ok(StreamCheckpoint {
            sequence: head.sequence,
            digest: head.digest,
        })
    }

    /// Like [`verify_stream`](Self::verify_stream), and additionally checks
    /// that every checkpoint still matches the stored history
    /// ([`IntegrityIssue::CheckpointMismatch`] otherwise).
    ///
    /// # Errors
    /// As [`verify_stream`](Self::verify_stream).
    pub fn verify_stream_against(
        &self,
        stream: &StreamId,
        checkpoints: &[StreamCheckpoint],
    ) -> Result<IntegrityReport> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let head =
            load_head(&tx, stream)?.ok_or_else(|| StoreError::StreamNotFound(stream.clone()))?;
        let mut verifier = ChainVerifier::new(stream.clone(), head);
        feed_verifier(&tx, stream, &mut verifier, Sequence::ZERO, None)?;
        for checkpoint in checkpoints {
            let actual: Option<Vec<u8>> = tx
                .query_row(
                    "SELECT digest FROM events WHERE stream_id = ?1 AND sequence = ?2",
                    params![stream.as_str(), checkpoint.sequence.to_i64()],
                    |row| row.get(0),
                )
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    e => Err(e),
                })?;
            let actual = actual.and_then(|b| crate::domain::Digest::from_slice(&b).ok());
            if actual != Some(checkpoint.digest) {
                verifier.report_checkpoint(IntegrityIssue::CheckpointMismatch {
                    sequence: checkpoint.sequence,
                    expected: checkpoint.digest,
                    actual,
                });
            }
        }
        Ok(verifier.finish())
    }

    /// Verifies only the events after `start` up to and including `through`,
    /// trusting `start` (sequence and digest) as the chain's starting point.
    /// Used by replay to validate a historical interval.
    pub(crate) fn verify_range(
        &self,
        stream: &StreamId,
        start: crate::domain::StreamHead,
        through: Sequence,
    ) -> Result<(IntegrityReport, crate::domain::StreamHead)> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let head =
            load_head(&tx, stream)?.ok_or_else(|| StoreError::StreamNotFound(stream.clone()))?;
        let mut verifier = ChainVerifier::starting_at(stream.clone(), head, start);
        feed_verifier(&tx, stream, &mut verifier, start.sequence, Some(through))?;
        let last = verifier.last_seen();
        Ok((verifier.finish_range(), last))
    }

    /// Verifies the whole database in one read transaction: SQLite's own
    /// structural check, foreign keys, the presence of ORBIT's protective
    /// triggers, every stream's event chain and head, and every snapshot's
    /// digest and history binding.
    ///
    /// This is the recommended check after recovering from a crash. It reads
    /// every row, so its cost is linear in the database size.
    ///
    /// # Errors
    /// Only if verification could not be performed (I/O, locking). Defects
    /// are reported in the returned [`StoreIntegrityReport`].
    pub fn verify_store(&self) -> Result<StoreIntegrityReport> {
        let started = Instant::now();
        let mut conn = self.conn();
        let tx = conn.transaction()?;

        let schema_version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let sqlite_integrity = tx
            .prepare("PRAGMA integrity_check")?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let foreign_key_violations = tx
            .prepare("PRAGMA foreign_key_check")?
            .query_map([], |_| Ok(()))?
            .count() as u64;
        let present = tx
            .prepare("SELECT name FROM sqlite_schema WHERE type = 'trigger'")?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let missing_safeguards = schema::SAFEGUARD_TRIGGERS
            .iter()
            .filter(|t| !present.iter().any(|p| p == *t))
            .map(|t| (*t).to_owned())
            .collect();

        let ids = tx
            .prepare("SELECT stream_id FROM streams ORDER BY stream_id")?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut streams = Vec::with_capacity(ids.len());
        for raw_id in ids {
            let Ok(id) = StreamId::new(raw_id.as_str()) else {
                return Err(StoreError::CorruptRecord {
                    stream: raw_id,
                    sequence: 0,
                    reason: "invalid stored stream id".to_owned(),
                });
            };
            let head =
                load_head(&tx, &id)?.ok_or_else(|| StoreError::StreamNotFound(id.clone()))?;
            let mut verifier = ChainVerifier::new(id.clone(), head);
            feed_verifier(&tx, &id, &mut verifier, Sequence::ZERO, None)?;
            streams.push(verifier.finish());
        }

        let raws = super::snapshots::all_raw_snapshots(&tx)?;
        let mut snapshot_issues = Vec::new();
        for raw in &raws {
            if let SnapshotCheck::Unusable { sequence, problem } =
                super::snapshots::check_raw(&tx, raw)?
            {
                snapshot_issues.push(SnapshotIntegrityIssue {
                    snapshot: raw.display_id(),
                    stream: raw.stream_id.clone(),
                    sequence,
                    problem,
                });
            }
        }

        let runs = super::replay::all_raw_runs(&tx)?;
        let mut replay_run_issues = Vec::new();
        for run in &runs {
            let computed = run.computed_digest();
            let problem = if run.record_digest.as_slice() != computed.as_bytes() {
                Some(format!("record digest mismatch (computed {computed})"))
            } else {
                run.decode().err()
            };
            if let Some(problem) = problem {
                replay_run_issues.push(ReplayRecordIssue {
                    run: run.display_id(),
                    problem,
                });
            }
        }

        let report = StoreIntegrityReport {
            schema_version,
            sqlite_integrity,
            foreign_key_violations,
            missing_safeguards,
            streams,
            snapshots_checked: raws.len() as u64,
            snapshot_issues,
            replay_runs_checked: runs.len() as u64,
            replay_run_issues,
            elapsed_micros: u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
        };
        if report.is_valid() {
            tracing::debug!(streams = report.streams.len(), "store verified");
        } else {
            tracing::warn!("store verification found issues");
        }
        Ok(report)
    }
}
