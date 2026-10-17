//! The database schema enforces the core invariants on its own, even for
//! writes that bypass ORBIT.

mod common;

use common::{TestDb, seed, sid};
use rusqlite::params;

fn assert_rejected(result: rusqlite::Result<usize>, needle: &str) {
    let err = result.expect_err("write should have been rejected");
    let message = err.to_string();
    assert!(message.contains(needle), "unexpected error: {message}");
}

#[test]
fn events_cannot_be_updated_or_deleted() {
    let db = TestDb::new();
    let store = db.open();
    seed(&store, &sid("s"), 3);
    let raw = db.raw();
    assert_rejected(
        raw.execute("UPDATE events SET payload = '{}' WHERE sequence = 2", []),
        "append-only",
    );
    assert_rejected(
        raw.execute("DELETE FROM events WHERE sequence = 3", []),
        "append-only",
    );
    assert_rejected(raw.execute("DELETE FROM streams", []), "cannot be deleted");
    assert_rejected(
        raw.execute("UPDATE streams SET metadata = '{\"x\":1}'", []),
        "immutable",
    );
}

#[test]
fn duplicate_or_skipped_sequences_are_rejected() {
    let db = TestDb::new();
    let store = db.open();
    seed(&store, &sid("s"), 2);
    let raw = db.raw();
    let (head_digest,): (Vec<u8>,) = raw
        .query_row(
            "SELECT head_digest FROM streams WHERE stream_id = 's'",
            [],
            |r| Ok((r.get(0)?,)),
        )
        .unwrap();
    let insert = |sequence: i64, event_id: [u8; 16], prev: &[u8]| {
        raw.execute(
            "INSERT INTO events VALUES ('s', ?1, ?2, 't', 1, 0, '{}', '{}', ?3, ?4)",
            params![sequence, event_id.as_slice(), prev, [0u8; 32].as_slice()],
        )
    };
    // Re-using sequence 2 (would also violate the primary key).
    assert_rejected(insert(2, [7; 16], &head_digest), "extend the stream head");
    // Skipping sequence 3.
    assert_rejected(insert(4, [8; 16], &head_digest), "extend the stream head");
    // Correct sequence but wrong chain link.
    assert_rejected(insert(3, [9; 16], &[1u8; 32]), "extend the stream head");
    // With the trigger removed, the primary key still forbids duplicates.
    raw.execute_batch("DROP TRIGGER events_insert_extends_head")
        .unwrap();
    assert_rejected(insert(2, [7; 16], &head_digest), "UNIQUE");
}

#[test]
fn head_cannot_jump_or_point_at_a_missing_event() {
    let db = TestDb::new();
    let store = db.open();
    seed(&store, &sid("s"), 2);
    let raw = db.raw();
    assert_rejected(
        raw.execute("UPDATE streams SET head_sequence = 5", []),
        "immutable",
    );
    assert_rejected(
        raw.execute("UPDATE streams SET head_sequence = 1", []),
        "immutable",
    );
    assert_rejected(
        raw.execute(
            "UPDATE streams SET head_digest = ?1",
            [[3u8; 32].as_slice()],
        ),
        "immutable",
    );
}

#[test]
fn events_require_an_existing_stream_and_valid_columns() {
    let db = TestDb::new();
    let store = db.open();
    seed(&store, &sid("s"), 0);
    let raw = db.raw();
    raw.execute_batch("PRAGMA foreign_keys = ON; DROP TRIGGER events_insert_extends_head;")
        .unwrap();
    let zeros = [0u8; 32];
    // Unknown stream.
    assert_rejected(
        raw.execute(
            "INSERT INTO events VALUES ('ghost', 1, ?1, 't', 1, 0, '{}', '{}', ?2, ?2)",
            params![[1u8; 16].as_slice(), zeros.as_slice()],
        ),
        "FOREIGN KEY",
    );
    // Invalid JSON payload.
    assert_rejected(
        raw.execute(
            "INSERT INTO events VALUES ('s', 1, ?1, 't', 1, 0, '{oops', '{}', ?2, ?2)",
            params![[2u8; 16].as_slice(), zeros.as_slice()],
        ),
        "CHECK",
    );
    // Metadata must be an object.
    assert_rejected(
        raw.execute(
            "INSERT INTO events VALUES ('s', 1, ?1, 't', 1, 0, '{}', '[]', ?2, ?2)",
            params![[3u8; 16].as_slice(), zeros.as_slice()],
        ),
        "CHECK",
    );
    // STRICT typing: sequence must be an integer.
    assert_rejected(
        raw.execute(
            "INSERT INTO events VALUES ('s', 'one', ?1, 't', 1, 0, '{}', '{}', ?2, ?2)",
            params![[4u8; 16].as_slice(), zeros.as_slice()],
        ),
        "cannot store",
    );
    // Digest length.
    assert_rejected(
        raw.execute(
            "INSERT INTO events VALUES ('s', 1, ?1, 't', 1, 0, '{}', '{}', ?2, X'00')",
            params![[5u8; 16].as_slice(), zeros.as_slice()],
        ),
        "CHECK",
    );
}
