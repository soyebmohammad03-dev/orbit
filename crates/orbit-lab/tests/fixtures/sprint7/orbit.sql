-- Event store created by orbit-api at commit a44e62f (Sprint 7), dumped with sqlite3 .dump.
PRAGMA application_id = 1330790996;
PRAGMA user_version = 3;
PRAGMA foreign_keys=OFF;
BEGIN TRANSACTION;
CREATE TABLE streams (
    stream_id     TEXT    NOT NULL PRIMARY KEY CHECK (length(stream_id) BETWEEN 1 AND 128),
    created_at    INTEGER NOT NULL CHECK (created_at >= 0),
    metadata      TEXT    NOT NULL CHECK (json_valid(metadata) AND json_type(metadata) = 'object'),
    head_sequence INTEGER NOT NULL DEFAULT 0 CHECK (head_sequence >= 0),
    head_digest   BLOB    NOT NULL CHECK (length(head_digest) = 32)
) STRICT;
INSERT INTO streams VALUES('legacy-counter',1792310728078471,'{}',5,x'f50f58845b8a5b51ce1757a4bb7b2abc7aa687796eecc768a35639c43020376a');
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
INSERT INTO events VALUES('legacy-counter',1,x'01a14e0b55a070a6b6a4d0f08344bdd7','counter.incremented',1,1792310728096231,'{"by":2}','{}',x'0000000000000000000000000000000000000000000000000000000000000000',x'67d1dd1c72a4250a3a145d2f5896be52c9e2fb9cc0db0aba0d9bd5bd46def6b2');
INSERT INTO events VALUES('legacy-counter',2,x'01a14e0b55af716896de243a3c82f0b7','counter.decremented',1,1792310728111095,'{"by":5}','{}',x'67d1dd1c72a4250a3a145d2f5896be52c9e2fb9cc0db0aba0d9bd5bd46def6b2',x'fda78bd51e78ce243a9a194a4788a79eb3e5edffb33aab1cc269e6e375b71f18');
INSERT INTO events VALUES('legacy-counter',3,x'01a14e0b55bd71408dda5dd18d0363bd','counter.reset',1,1792310728125822,'{}','{}',x'fda78bd51e78ce243a9a194a4788a79eb3e5edffb33aab1cc269e6e375b71f18',x'eb588ee7bc2e09cdbe9c32ffde6173f021b1fc1fd2b327d01a445d6f7229b879');
INSERT INTO events VALUES('legacy-counter',4,x'01a14e0b55ce7738b09b2fbb2f96b768','counter.incremented',1,1792310728142212,'{"by":1}','{}',x'eb588ee7bc2e09cdbe9c32ffde6173f021b1fc1fd2b327d01a445d6f7229b879',x'0d0ea90a79b3e0b9dc359832c931ad0ec5c7be53904d599aa47d179002075a94');
INSERT INTO events VALUES('legacy-counter',5,x'01a14e0b55de73a184cbb7948e7b3fcd','counter.decremented',1,1792310728158470,'{"by":4}','{}',x'0d0ea90a79b3e0b9dc359832c931ad0ec5c7be53904d599aa47d179002075a94',x'f50f58845b8a5b51ce1757a4bb7b2abc7aa687796eecc768a35639c43020376a');
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
CREATE TABLE replay_runs (
    run_id                BLOB    NOT NULL PRIMARY KEY CHECK (length(run_id) = 16),
    stream_id             TEXT    NOT NULL REFERENCES streams (stream_id) ON DELETE RESTRICT,
    projection_id         TEXT    NOT NULL CHECK (length(projection_id) BETWEEN 1 AND 128),
    projection_version    INTEGER NOT NULL CHECK (projection_version BETWEEN 1 AND 4294967295),
    from_sequence         INTEGER NOT NULL CHECK (from_sequence >= 0),
    to_sequence           INTEGER NOT NULL CHECK (to_sequence >= from_sequence),
    from_event_digest     BLOB    NOT NULL CHECK (length(from_event_digest) = 32),
    to_event_digest       BLOB    NOT NULL CHECK (length(to_event_digest) = 32),
    start_source          TEXT    NOT NULL CHECK (start_source IN ('initial', 'snapshot', 'reconstructed')),
    start_snapshot_id     BLOB    CHECK (start_snapshot_id IS NULL OR length(start_snapshot_id) = 16),
    start_state           TEXT    NOT NULL CHECK (json_valid(start_state)),
    start_state_digest    BLOB    NOT NULL CHECK (length(start_state_digest) = 32),
    expected_state        TEXT    NOT NULL CHECK (json_valid(expected_state)),
    expected_state_digest BLOB    NOT NULL CHECK (length(expected_state_digest) = 32),
    trace                 BLOB    NOT NULL CHECK (length(trace) = 32 * (to_sequence - from_sequence)),
    created_at            INTEGER NOT NULL CHECK (created_at >= 0),
    record_digest         BLOB    NOT NULL CHECK (length(record_digest) = 32),
    CHECK (start_source <> 'initial' OR (from_sequence = 0 AND start_snapshot_id IS NULL)),
    CHECK (start_source <> 'snapshot' OR start_snapshot_id IS NOT NULL)
) STRICT;
CREATE TABLE replay_executions (
    execution_id        BLOB    NOT NULL PRIMARY KEY CHECK (length(execution_id) = 16),
    run_id              BLOB    NOT NULL REFERENCES replay_runs (run_id) ON DELETE CASCADE,
    executed_at         INTEGER NOT NULL CHECK (executed_at >= 0),
    outcome             TEXT    NOT NULL CHECK (outcome IN ('exact_match', 'reconverged', 'diverged',
                                    'projection_failed', 'unavailable', 'invalid_input', 'integrity_failure')),
    actual_state_digest BLOB    CHECK (actual_state_digest IS NULL OR length(actual_state_digest) = 32),
    report              TEXT    NOT NULL CHECK (json_valid(report) AND json_type(report) = 'object')
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
CREATE TRIGGER replay_runs_no_update
BEFORE UPDATE ON replay_runs
BEGIN
    SELECT RAISE(ABORT, 'orbit: replay runs are immutable');
END;
CREATE TRIGGER replay_executions_no_update
BEFORE UPDATE ON replay_executions
BEGIN
    SELECT RAISE(ABORT, 'orbit: replay executions are immutable');
END;
CREATE INDEX replay_runs_by_stream ON replay_runs (stream_id, created_at);
CREATE INDEX replay_executions_by_run ON replay_executions (run_id, executed_at);
COMMIT;
