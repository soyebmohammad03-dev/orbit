//! Persistence of replay runs and executions. Pure storage: interpretation
//! lives in [`crate::replay`].

use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};

use super::{EventStore, load_head};
use crate::domain::{StreamId, Timestamp};
use crate::error::{Result, StoreError};
use crate::fault::FaultPoint;
use crate::replay::{RawReplayRun, ReplayExecutionId, ReplayRunId, StoredExecution};

const RUN_COLUMNS: &str = "run_id, stream_id, projection_id, projection_version, from_sequence, \
     to_sequence, from_event_digest, to_event_digest, start_source, start_snapshot_id, start_state, \
     start_state_digest, expected_state, expected_state_digest, trace, created_at, record_digest";

impl EventStore {
    pub(crate) fn insert_replay_run(&self, run: &RawReplayRun) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stream = StreamId::new(run.stream_id.as_str())?;
        if load_head(&tx, &stream)?.is_none() {
            return Err(StoreError::StreamNotFound(stream));
        }
        tx.execute(
            &format!(
                "INSERT INTO replay_runs ({RUN_COLUMNS})
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)"
            ),
            params![
                run.run_id,
                run.stream_id,
                run.projection_id,
                run.projection_version,
                run.from_sequence,
                run.to_sequence,
                run.from_event_digest,
                run.to_event_digest,
                run.start_source,
                run.start_snapshot_id,
                run.start_state,
                run.start_state_digest,
                run.expected_state,
                run.expected_state_digest,
                run.trace,
                run.created_at,
                run.record_digest,
            ],
        )?;
        self.fault(FaultPoint::ReplayBeforeCommit)?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn load_replay_run(&self, id: ReplayRunId) -> Result<RawReplayRun> {
        let conn = self.conn();
        conn.query_row(
            &format!("SELECT {RUN_COLUMNS} FROM replay_runs WHERE run_id = ?1"),
            [id.as_bytes().as_slice()],
            read_raw_run,
        )
        .optional()?
        .ok_or(StoreError::ReplayRunNotFound(id))
    }

    pub(crate) fn replay_runs_of(&self, stream: &StreamId) -> Result<Vec<RawReplayRun>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        if load_head(&tx, stream)?.is_none() {
            return Err(StoreError::StreamNotFound(stream.clone()));
        }
        let mut statement = tx.prepare_cached(&format!(
            "SELECT {RUN_COLUMNS} FROM replay_runs WHERE stream_id = ?1 ORDER BY created_at, run_id"
        ))?;
        let rows = statement
            .query_map([stream.as_str()], read_raw_run)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub(crate) fn insert_replay_execution(
        &self,
        id: ReplayExecutionId,
        run: ReplayRunId,
        executed_at: Timestamp,
        outcome: &str,
        actual_state_digest: Option<&[u8; 32]>,
        report: &str,
    ) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO replay_executions
                 (execution_id, run_id, executed_at, outcome, actual_state_digest, report)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id.as_bytes().as_slice(),
                run.as_bytes().as_slice(),
                executed_at.unix_micros(),
                outcome,
                actual_state_digest.map(<[u8; 32]>::as_slice),
                report,
            ],
        )?;
        self.fault(FaultPoint::ReplayBeforeCommit)?;
        tx.commit()?;
        Ok(())
    }

    /// Every execution of a run, oldest first.
    ///
    /// # Errors
    /// [`StoreError::ReplayRunNotFound`], [`StoreError::CorruptReplayRecord`],
    /// or a database error.
    pub fn replay_executions(&self, run: ReplayRunId) -> Result<Vec<StoredExecution>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let exists = tx
            .query_row(
                "SELECT 1 FROM replay_runs WHERE run_id = ?1",
                [run.as_bytes().as_slice()],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !exists {
            return Err(StoreError::ReplayRunNotFound(run));
        }
        let mut statement = tx.prepare_cached(
            "SELECT execution_id, executed_at, outcome, actual_state_digest, report
             FROM replay_executions WHERE run_id = ?1 ORDER BY executed_at, execution_id",
        )?;
        let rows = statement
            .query_map([run.as_bytes().as_slice()], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<Vec<u8>>>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|(id, at, outcome, digest, report)| {
                StoredExecution::decode(run, &id, at, outcome, digest.as_deref(), &report)
            })
            .collect()
    }
}

/// Every replay run row (for store verification).
pub(super) fn all_raw_runs(conn: &Connection) -> Result<Vec<RawReplayRun>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {RUN_COLUMNS} FROM replay_runs ORDER BY run_id"
    ))?;
    let rows = statement.query_map([], read_raw_run)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn read_raw_run(row: &Row<'_>) -> rusqlite::Result<RawReplayRun> {
    Ok(RawReplayRun {
        run_id: row.get(0)?,
        stream_id: row.get(1)?,
        projection_id: row.get(2)?,
        projection_version: row.get(3)?,
        from_sequence: row.get(4)?,
        to_sequence: row.get(5)?,
        from_event_digest: row.get(6)?,
        to_event_digest: row.get(7)?,
        start_source: row.get(8)?,
        start_snapshot_id: row.get(9)?,
        start_state: row.get(10)?,
        start_state_digest: row.get(11)?,
        expected_state: row.get(12)?,
        expected_state_digest: row.get(13)?,
        trace: row.get(14)?,
        created_at: row.get(15)?,
        record_digest: row.get(16)?,
    })
}
