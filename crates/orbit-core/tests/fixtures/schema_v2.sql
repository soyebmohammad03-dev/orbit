-- ORBIT schema v2 database produced by commit 956cc21 (Phase 2), via sqlite3 .dump.
-- 4 counter events (final value 16) and one counter@v1 snapshot at sequence 3.
-- Used by tests/schema.rs to verify the v2 -> v3 migration.
PRAGMA application_id = 1330790996;
PRAGMA user_version = 2;
PRAGMA foreign_keys=OFF;
BEGIN TRANSACTION;
CREATE TABLE streams (
    stream_id     TEXT    NOT NULL PRIMARY KEY CHECK (length(stream_id) BETWEEN 1 AND 128),
    created_at    INTEGER NOT NULL CHECK (created_at >= 0),
    metadata      TEXT    NOT NULL CHECK (json_valid(metadata) AND json_type(metadata) = 'object'),
    head_sequence INTEGER NOT NULL DEFAULT 0 CHECK (head_sequence >= 0),
    head_digest   BLOB    NOT NULL CHECK (length(head_digest) = 32)
) STRICT;
INSERT INTO streams VALUES('counter-1',1792267073006739,'{}',4,x'298a942232d6d3884f1f772aeebeb25a42de9aaacd400cc9d7a0f80e37a4c459');
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
INSERT INTO events VALUES('counter-1',1,x'01a14b7135f67326908a52841dc63566','counter.incremented',1,1792267073014211,'{"by":5}','{}',x'0000000000000000000000000000000000000000000000000000000000000000',x'f9484d612a97213ea85f1ddc1d636d8c4098865189b0f13c43a36c5d5e0e973c');
INSERT INTO events VALUES('counter-1',2,x'01a14b7135fa706f8633fe06bc99206b','counter.incremented',1,1792267073018077,'{"by":3}','{}',x'f9484d612a97213ea85f1ddc1d636d8c4098865189b0f13c43a36c5d5e0e973c',x'65a5e517c51cc1a65311163f27099d5566acf0adb8530672dd008f53638a223e');
INSERT INTO events VALUES('counter-1',3,x'01a14b7135fe760c8f363fd689df9aba','counter.decremented',1,1792267073022161,'{"by":2}','{}',x'65a5e517c51cc1a65311163f27099d5566acf0adb8530672dd008f53638a223e',x'de6e433406a90dc7f7a49cc51fb53226f67b9ab23ac42e8a92864583a644952e');
INSERT INTO events VALUES('counter-1',4,x'01a14b71360273a18ab87aafe5772ffa','counter.incremented',1,1792267073026048,'{"by":10}','{}',x'de6e433406a90dc7f7a49cc51fb53226f67b9ab23ac42e8a92864583a644952e',x'298a942232d6d3884f1f772aeebeb25a42de9aaacd400cc9d7a0f80e37a4c459');
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
INSERT INTO snapshots VALUES(x'01a14b71360671f59bc8fcf32badeb3d','counter-1',3,'counter',1,x'de6e433406a90dc7f7a49cc51fb53226f67b9ab23ac42e8a92864583a644952e','{"value":6}',x'03ee4aa8c352e9365c635a6f9c2223d340244b09ea53b741d3a72ca29574a69e',1792267073030442);
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
COMMIT;
