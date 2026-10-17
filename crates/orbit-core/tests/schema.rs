//! Schema creation, migration bookkeeping and refusal of foreign files.

mod common;

use common::{TestDb, seed, sid};
use orbit_core::{EventStore, StoreError};

#[test]
fn fresh_database_is_marked_and_versioned() {
    let db = TestDb::new();
    drop(db.open());
    let raw = db.raw();
    let version: i64 = raw
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    let app_id: i64 = raw
        .query_row("PRAGMA application_id", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 1);
    assert_eq!(app_id, 0x4F52_4254);
    let tables: Vec<String> = raw
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(tables, vec!["events", "streams"]);
}

#[test]
fn reopening_does_not_reapply_migrations() {
    let db = TestDb::new();
    seed(&db.open(), &sid("s"), 3);
    for _ in 0..3 {
        let store = db.open();
        assert_eq!(store.read_stream(&sid("s")).unwrap().len(), 3);
    }
}

#[test]
fn foreign_sqlite_database_is_refused() {
    let db = TestDb::new();
    db.raw()
        .execute_batch("CREATE TABLE customers (id INTEGER PRIMARY KEY)")
        .unwrap();
    let err = EventStore::open(db.config()).unwrap_err();
    assert!(matches!(err, StoreError::NotAnOrbitDatabase(_)), "{err}");
}

#[test]
fn wrong_application_id_is_refused() {
    let db = TestDb::new();
    drop(db.open());
    db.raw()
        .execute_batch("PRAGMA application_id = 42")
        .unwrap();
    let err = EventStore::open(db.config()).unwrap_err();
    assert!(matches!(err, StoreError::NotAnOrbitDatabase(_)), "{err}");
}

#[test]
fn newer_schema_version_is_refused() {
    let db = TestDb::new();
    drop(db.open());
    db.raw().execute_batch("PRAGMA user_version = 99").unwrap();
    let err = EventStore::open(db.config()).unwrap_err();
    assert!(
        matches!(
            err,
            StoreError::UnsupportedSchemaVersion {
                found: 99,
                supported: 1
            }
        ),
        "{err}"
    );
}

#[test]
fn non_database_file_is_an_error_not_a_panic() {
    let db = TestDb::new();
    std::fs::write(
        &db.path,
        b"this is definitely not a sqlite database file....",
    )
    .unwrap();
    assert!(EventStore::open(db.config()).is_err());
}

#[test]
fn invalid_config_is_rejected_before_touching_disk() {
    let db = TestDb::new();
    let err = EventStore::open(orbit_core::StoreConfig::new(":memory:")).unwrap_err();
    assert!(matches!(err, StoreError::Config(_)), "{err}");
    assert!(!db.path.exists());
}
