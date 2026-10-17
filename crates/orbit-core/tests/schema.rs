//! Schema creation, migration bookkeeping and refusal of foreign files.

mod common;

use common::{TestDb, seed, sid};
use orbit_core::projection::examples::Counter;
use orbit_core::{EventStore, ReconstructOptions, Reconstructor, StoreError, Target};

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
    assert_eq!(version, 3);
    assert_eq!(app_id, 0x4F52_4254);
    let tables: Vec<String> = raw
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        tables,
        vec![
            "events",
            "replay_executions",
            "replay_runs",
            "snapshots",
            "streams"
        ]
    );
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
                supported: 3
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

/// A database written by Phase 1 (schema v1, see `tests/fixtures/schema_v1.sql`)
/// is upgraded in place to v2 with its events intact and usable.
#[test]
fn version_1_database_is_migrated_to_version_2() {
    let db = TestDb::new();
    db.raw()
        .execute_batch(include_str!("fixtures/schema_v1.sql"))
        .unwrap();
    let version: i64 = db
        .raw()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 1);

    let store = db.open();
    let version: i64 = db
        .raw()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    // Migrations 2 and 3 are both applied in order.
    assert_eq!(version, 3);

    let id = sid("counter-1");
    assert_eq!(store.read_stream(&id).unwrap().len(), 3);
    assert!(store.verify_stream(&id).unwrap().is_valid());
    assert!(store.list_snapshots(&id).unwrap().is_empty());

    let counter = Reconstructor::new(&store, Counter);
    let created = counter
        .create_snapshot(&id, Target::Latest, ReconstructOptions::default())
        .unwrap();
    assert_eq!(
        created.saved.snapshot.state,
        serde_json::json!({"value": 6})
    );
    assert_eq!(store.list_snapshots(&id).unwrap().len(), 1);
}

/// A database written by Phase 2 (schema v2, see `tests/fixtures/schema_v2.sql`,
/// with events and a snapshot) is upgraded to v3 with everything intact.
#[test]
fn version_2_database_is_migrated_to_version_3() {
    use orbit_core::projection::examples::CounterState;
    use orbit_core::reconstruct::StartPoint;
    use orbit_core::replay::{ReplayLab, ReplayOutcome, ReplayStart};

    let db = TestDb::new();
    db.raw()
        .execute_batch(include_str!("fixtures/schema_v2.sql"))
        .unwrap();
    let store = db.open();
    let version: i64 = db
        .raw()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 3);

    let report = store.verify_store().unwrap();
    assert!(report.is_valid(), "{report:?}");
    assert_eq!(report.snapshots_checked, 1);

    let id = sid("counter-1");
    let counter = Reconstructor::new(&store, Counter);
    let latest = counter
        .reconstruct(&id, Target::Latest, ReconstructOptions::default())
        .unwrap();
    assert_eq!(latest.state, CounterState { value: 16 });
    // The Phase 2 snapshot at sequence 3 is still valid and used.
    assert!(
        matches!(latest.report.start, StartPoint::Snapshot { sequence, .. } if sequence.get() == 3)
    );

    let lab = ReplayLab::new(&store, Counter);
    let run = lab
        .record(&id, ReplayStart::Initial, Target::Latest)
        .unwrap();
    assert_eq!(
        lab.execute(run.id).unwrap().outcome,
        ReplayOutcome::ExactMatch
    );
}
