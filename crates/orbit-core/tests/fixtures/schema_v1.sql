-- ORBIT schema v1 database produced by commit e60d571 (Phase 1), via sqlite3 .dump.
-- Used by tests/schema.rs to verify the v1 -> v2 migration.
PRAGMA application_id = 1330790996;
PRAGMA user_version = 1;
PRAGMA foreign_keys=OFF;
BEGIN TRANSACTION;
CREATE TABLE streams (
    stream_id     TEXT    NOT NULL PRIMARY KEY CHECK (length(stream_id) BETWEEN 1 AND 128),
    created_at    INTEGER NOT NULL CHECK (created_at >= 0),
    metadata      TEXT    NOT NULL CHECK (json_valid(metadata) AND json_type(metadata) = 'object'),
    head_sequence INTEGER NOT NULL DEFAULT 0 CHECK (head_sequence >= 0),
    head_digest   BLOB    NOT NULL CHECK (length(head_digest) = 32)
) STRICT;
INSERT INTO streams VALUES('counter-1',1792265055603049,'{}',3,x'b3417774115e5d39c23407ecdd34b4d6c80c8cbaacc11acc20faab8bb394186a');
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
INSERT INTO events VALUES('counter-1',1,x'01a14b526d7573ebaf7942b20ddea12c','counter.incremented',1,1792265055605071,'{"by":5}','{}',x'0000000000000000000000000000000000000000000000000000000000000000',x'347d2cf45c263cdb371a6e307d2b6d0698996cfe46cef1d2b2efc061ecb0f950');
INSERT INTO events VALUES('counter-1',2,x'01a14b526d7772eeb1f6e027b00e6981','counter.incremented',1,1792265055607593,'{"by":3}','{}',x'347d2cf45c263cdb371a6e307d2b6d0698996cfe46cef1d2b2efc061ecb0f950',x'2618b359d0f2c8eced8e7201a2e7cd922f92852a5b0a82f6fd3adaee5b854f63');
INSERT INTO events VALUES('counter-1',3,x'01a14b526d7a75dfae326655277b26e2','counter.decremented',1,1792265055610997,'{"by":2}','{}',x'2618b359d0f2c8eced8e7201a2e7cd922f92852a5b0a82f6fd3adaee5b854f63',x'b3417774115e5d39c23407ecdd34b4d6c80c8cbaacc11acc20faab8bb394186a');
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
COMMIT;
