//! SQLite connection setup and schema migrations.
//!
//! The schema version is tracked in `PRAGMA user_version` and ORBIT files are
//! marked with `PRAGMA application_id`. Migrations are applied in order, each
//! inside its own `BEGIN IMMEDIATE` transaction, so concurrent openers cannot
//! apply the same migration twice and a failed migration leaves no trace.

use rusqlite::{Connection, OpenFlags, TransactionBehavior};

use crate::config::{Durability, StoreConfig};
use crate::error::{Result, StoreError};

/// `PRAGMA application_id` value identifying ORBIT databases (ASCII "ORBT").
pub(crate) const APPLICATION_ID: i64 = 0x4F52_4254;

/// Ordered migrations; index `i` upgrades the schema from version `i` to `i + 1`.
const MIGRATIONS: &[&str] = &[MIGRATION_1, MIGRATION_2];

/// Newest schema version known to this build.
pub(crate) const LATEST_VERSION: i64 = MIGRATIONS.len() as i64;

/// Initial schema.
///
/// Invariants enforced by the database itself, independently of the Rust code:
/// * `STRICT` tables reject values of the wrong storage class;
/// * `(stream_id, sequence)` is the primary key of `events`, so a sequence
///   number can never be assigned twice within a stream;
/// * an inserted event must have `sequence = head_sequence + 1` and
///   `prev_digest = head_digest` of its stream (dense, chained sequences);
/// * a stream head can only advance by one, and only to an existing event
///   with a matching digest;
/// * events and streams cannot be updated (other than the head) or deleted;
/// * payload and metadata must be valid JSON; metadata must be an object.
const MIGRATION_1: &str = r"
CREATE TABLE streams (
    stream_id     TEXT    NOT NULL PRIMARY KEY CHECK (length(stream_id) BETWEEN 1 AND 128),
    created_at    INTEGER NOT NULL CHECK (created_at >= 0),
    metadata      TEXT    NOT NULL CHECK (json_valid(metadata) AND json_type(metadata) = 'object'),
    head_sequence INTEGER NOT NULL DEFAULT 0 CHECK (head_sequence >= 0),
    head_digest   BLOB    NOT NULL CHECK (length(head_digest) = 32)
) STRICT;

CREATE TABLE events (
    stream_id      TEXT    NOT NULL REFERENCES streams (stream_id) ON DELETE RESTRICT,
    sequence       INTEGER NOT NULL CHECK (sequence >= 1),
    event_id       BLOB    NOT NULL UNIQUE CHECK (length(event_id) = 16),
    event_type     TEXT    NOT NULL CHECK (length(event_type) BETWEEN 1 AND 128),
    schema_version INTEGER NOT NULL CHECK (schema_version BETWEEN 1 AND 4294967295),
    recorded_at    INTEGER NOT NULL CHECK (recorded_at >= 0),
    payload        TEXT    NOT NULL CHECK (json_valid(payload)),
    metadata       TEXT    NOT NULL CHECK (json_valid(metadata) AND json_type(metadata) = 'object'),
    prev_digest    BLOB    NOT NULL CHECK (length(prev_digest) = 32),
    digest         BLOB    NOT NULL CHECK (length(digest) = 32),
    PRIMARY KEY (stream_id, sequence)
) STRICT;

CREATE TRIGGER events_insert_extends_head
BEFORE INSERT ON events
WHEN NEW.sequence IS NOT (SELECT head_sequence + 1 FROM streams WHERE stream_id = NEW.stream_id)
  OR NEW.prev_digest IS NOT (SELECT head_digest FROM streams WHERE stream_id = NEW.stream_id)
BEGIN
    SELECT RAISE(ABORT, 'orbit: event must extend the stream head');
END;

CREATE TRIGGER events_no_update
BEFORE UPDATE ON events
BEGIN
    SELECT RAISE(ABORT, 'orbit: events are append-only');
END;

CREATE TRIGGER events_no_delete
BEFORE DELETE ON events
BEGIN
    SELECT RAISE(ABORT, 'orbit: events are append-only');
END;

CREATE TRIGGER streams_head_only_advances
BEFORE UPDATE ON streams
WHEN NEW.stream_id IS NOT OLD.stream_id
  OR NEW.created_at IS NOT OLD.created_at
  OR NEW.metadata IS NOT OLD.metadata
  OR NEW.head_sequence IS NOT OLD.head_sequence + 1
  OR NOT EXISTS (
        SELECT 1 FROM events
        WHERE stream_id = NEW.stream_id
          AND sequence = NEW.head_sequence
          AND digest = NEW.head_digest)
BEGIN
    SELECT RAISE(ABORT, 'orbit: stream rows are immutable except for head advancement');
END;

CREATE TRIGGER streams_no_delete
BEFORE DELETE ON streams
BEGIN
    SELECT RAISE(ABORT, 'orbit: streams cannot be deleted');
END;
";

/// Snapshots (Phase 2).
///
/// * a snapshot can only be inserted if the event it claims to be built on
///   exists with exactly the recorded digest, so a snapshot can never claim
///   history that is not in the store;
/// * at most one snapshot per (stream, projection id, projection version, sequence);
/// * snapshots are immutable. They are derived data and may be deleted;
///   ORBIT never depends on a particular snapshot existing.
///
/// The unique constraint's index also serves "newest compatible snapshot at
/// or before sequence N" lookups.
const MIGRATION_2: &str = r"
CREATE TABLE snapshots (
    snapshot_id        BLOB    NOT NULL PRIMARY KEY CHECK (length(snapshot_id) = 16),
    stream_id          TEXT    NOT NULL REFERENCES streams (stream_id) ON DELETE RESTRICT,
    sequence           INTEGER NOT NULL CHECK (sequence >= 1),
    projection_id      TEXT    NOT NULL CHECK (length(projection_id) BETWEEN 1 AND 128),
    projection_version INTEGER NOT NULL CHECK (projection_version BETWEEN 1 AND 4294967295),
    event_digest       BLOB    NOT NULL CHECK (length(event_digest) = 32),
    state              TEXT    NOT NULL CHECK (json_valid(state)),
    state_digest       BLOB    NOT NULL CHECK (length(state_digest) = 32),
    created_at         INTEGER NOT NULL CHECK (created_at >= 0),
    UNIQUE (stream_id, projection_id, projection_version, sequence)
) STRICT;

CREATE TRIGGER snapshots_insert_matches_history
BEFORE INSERT ON snapshots
WHEN NOT EXISTS (
        SELECT 1 FROM events
        WHERE stream_id = NEW.stream_id
          AND sequence = NEW.sequence
          AND digest = NEW.event_digest)
BEGIN
    SELECT RAISE(ABORT, 'orbit: snapshot must reference an existing event and its digest');
END;

CREATE TRIGGER snapshots_no_update
BEFORE UPDATE ON snapshots
BEGIN
    SELECT RAISE(ABORT, 'orbit: snapshots are immutable');
END;
";

/// Opens the database file, applies connection settings and migrations.
pub(crate) fn open(config: &StoreConfig) -> Result<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_EXRESCODE;
    let mut conn = Connection::open_with_flags(&config.path, flags)?;
    conn.busy_timeout(config.busy_timeout)?;
    configure(&conn, config.durability)?;
    migrate(&mut conn)?;
    Ok(conn)
}

fn configure(conn: &Connection, durability: Durability) -> Result<()> {
    let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(StoreError::NotAnOrbitDatabase(format!(
            "could not enable WAL journal mode (got {mode})"
        )));
    }
    let (synchronous, fullfsync) = match durability {
        Durability::Full => ("FULL", true),
        Durability::Normal => ("NORMAL", false),
    };
    conn.pragma_update(None, "synchronous", synchronous)?;
    // Only has an effect on macOS, where plain fsync does not flush the drive cache.
    conn.pragma_update(None, "fullfsync", fullfsync)?;
    conn.pragma_update(None, "foreign_keys", true)?;
    // Refuse to run if a file was opened read-only or foreign keys are unsupported.
    let fk: bool = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    if !fk {
        return Err(StoreError::NotAnOrbitDatabase(
            "foreign key enforcement is unavailable".to_owned(),
        ));
    }
    Ok(())
}

fn migrate(conn: &mut Connection) -> Result<()> {
    loop {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let app_id: i64 = tx.query_row("PRAGMA application_id", [], |row| row.get(0))?;
        let version: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;

        if version == 0 {
            let objects: i64 =
                tx.query_row("SELECT count(*) FROM sqlite_schema", [], |row| row.get(0))?;
            if app_id != 0 || objects != 0 {
                return Err(StoreError::NotAnOrbitDatabase(
                    "file contains a schema that was not created by ORBIT".to_owned(),
                ));
            }
        } else if app_id != APPLICATION_ID {
            return Err(StoreError::NotAnOrbitDatabase(format!(
                "unexpected application_id {app_id:#x}"
            )));
        }
        if version > LATEST_VERSION {
            return Err(StoreError::UnsupportedSchemaVersion {
                found: version,
                supported: LATEST_VERSION,
            });
        }
        if version == LATEST_VERSION {
            return Ok(());
        }

        let index = usize::try_from(version).map_err(|_| StoreError::UnsupportedSchemaVersion {
            found: version,
            supported: LATEST_VERSION,
        })?;
        tracing::info!(
            from = version,
            to = version + 1,
            "applying schema migration"
        );
        tx.execute_batch(MIGRATIONS[index])?;
        tx.pragma_update(None, "application_id", APPLICATION_ID)?;
        tx.pragma_update(None, "user_version", version + 1)?;
        tx.commit()?;
    }
}
