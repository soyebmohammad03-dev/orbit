//! Listing and counting queries for observability tooling.

use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use super::{EventStore, load_head};
use crate::domain::{Sequence, StreamHead, StreamId, Timestamp};
use crate::error::{Result, StoreError};

/// One stream with summary information, from [`EventStore::stream_summaries`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StreamSummary {
    /// Stream id.
    pub stream_id: StreamId,
    /// Creation time (informational).
    pub created_at: Timestamp,
    /// Current head; `head.sequence` is also the number of events.
    pub head: StreamHead,
    /// `recorded_at` of the head event, if any (informational).
    pub last_recorded_at: Option<Timestamp>,
    /// Number of snapshots stored for the stream.
    pub snapshot_count: u64,
    /// Number of replay runs recorded for the stream.
    pub replay_run_count: u64,
}

/// Row counts for the whole store, from [`EventStore::statistics`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct StoreStatistics {
    /// Streams.
    pub streams: u64,
    /// Events across all streams.
    pub events: u64,
    /// Snapshots.
    pub snapshots: u64,
    /// Replay runs.
    pub replay_runs: u64,
    /// Replay executions.
    pub replay_executions: u64,
    /// Schema version of the database file.
    pub schema_version: i64,
}

impl EventStore {
    /// Up to `limit` stream summaries with id greater than `after`, ordered by
    /// stream id. Pass the last returned id to continue.
    ///
    /// # Errors
    /// Database failures, or [`StoreError::CorruptRecord`] for damaged rows.
    pub fn stream_summaries(
        &self,
        after: Option<&StreamId>,
        limit: usize,
    ) -> Result<Vec<StreamSummary>> {
        self.summaries(after, None, limit)
    }

    /// The summary of one stream.
    ///
    /// # Errors
    /// [`StoreError::StreamNotFound`], database failures or corrupt rows.
    pub fn stream_summary(&self, id: &StreamId) -> Result<StreamSummary> {
        self.summaries(None, Some(id), 1)?
            .pop()
            .ok_or_else(|| StoreError::StreamNotFound(id.clone()))
    }

    fn summaries(
        &self,
        after: Option<&StreamId>,
        exact: Option<&StreamId>,
        limit: usize,
    ) -> Result<Vec<StreamSummary>> {
        let conn = self.conn();
        let mut statement = conn.prepare_cached(
            "SELECT s.stream_id, s.created_at,
                    (SELECT e.recorded_at FROM events e
                      WHERE e.stream_id = s.stream_id AND e.sequence = s.head_sequence),
                    (SELECT count(*) FROM snapshots n WHERE n.stream_id = s.stream_id),
                    (SELECT count(*) FROM replay_runs r WHERE r.stream_id = s.stream_id)
             FROM streams s
             WHERE (?1 IS NULL OR s.stream_id > ?1) AND (?3 IS NULL OR s.stream_id = ?3)
             ORDER BY s.stream_id
             LIMIT ?2",
        )?;
        let rows = statement
            .query_map(
                params![
                    after.map(StreamId::as_str),
                    i64::try_from(limit).unwrap_or(i64::MAX),
                    exact.map(StreamId::as_str)
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|(id, created_at, last, snapshots, runs)| {
                let corrupt = |reason: String| StoreError::CorruptRecord {
                    stream: id.clone(),
                    sequence: 0,
                    reason,
                };
                let stream_id = StreamId::new(id.as_str()).map_err(|e| corrupt(e.to_string()))?;
                let head = load_head(&conn, &stream_id)?
                    .ok_or_else(|| StoreError::StreamNotFound(stream_id.clone()))?;
                Ok(StreamSummary {
                    created_at: Timestamp::from_unix_micros(created_at)
                        .map_err(|e| corrupt(e.to_string()))?,
                    last_recorded_at: last
                        .map(Timestamp::from_unix_micros)
                        .transpose()
                        .map_err(|e| corrupt(e.to_string()))?,
                    snapshot_count: u64::try_from(snapshots).unwrap_or(0),
                    replay_run_count: u64::try_from(runs).unwrap_or(0),
                    stream_id,
                    head,
                })
            })
            .collect()
    }

    /// Row counts for the whole store, read in one transaction.
    ///
    /// # Errors
    /// Database failures.
    pub fn statistics(&self) -> Result<StoreStatistics> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let count = |table: &str| -> Result<u64> {
            let n: i64 =
                tx.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
            Ok(u64::try_from(n).unwrap_or(0))
        };
        let statistics = StoreStatistics {
            streams: count("streams")?,
            events: count("events")?,
            snapshots: count("snapshots")?,
            replay_runs: count("replay_runs")?,
            replay_executions: count("replay_executions")?,
            schema_version: tx.query_row("PRAGMA user_version", [], |r| r.get(0))?,
        };
        Ok(statistics)
    }

    /// Up to `limit` events across all streams, most recently recorded first
    /// (by `recorded_at`, then stream id and sequence descending). Timestamps
    /// are informational, so this is an activity feed, not an ordering
    /// guarantee.
    ///
    /// # Errors
    /// Database failures or corrupt rows.
    pub fn recent_events(&self, limit: usize) -> Result<Vec<crate::domain::StoredEvent>> {
        let conn = self.conn();
        let mut statement = conn.prepare_cached(&format!(
            "SELECT {} FROM events ORDER BY recorded_at DESC, stream_id DESC, sequence DESC LIMIT ?1",
            super::EVENT_COLUMNS
        ))?;
        let raws = statement
            .query_map([i64::try_from(limit).unwrap_or(i64::MAX)], super::read_raw)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        raws.iter().map(super::decode).collect()
    }

    /// The sequence of the newest snapshot of `stream` for any projection, if any.
    ///
    /// # Errors
    /// Database failures.
    pub fn latest_snapshot_sequence(&self, stream: &StreamId) -> Result<Option<Sequence>> {
        let conn = self.conn();
        let n: Option<i64> = conn
            .query_row(
                "SELECT max(sequence) FROM snapshots WHERE stream_id = ?1",
                [stream.as_str()],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        Ok(n.and_then(Sequence::from_i64))
    }
}
