//! Real process crashes, followed by recovery on reopen.
//!
//! Each scenario runs in a child process (this test binary, re-invoked with
//! `ORBIT_CRASH_CHILD` set). The child either aborts itself at a chosen
//! fault point, with no destructors and no rollback (`std::process::abort`),
//! or is killed with `SIGKILL` by the parent while it appends. The parent
//! then reopens the database, which runs SQLite's WAL recovery, and checks
//! that every committed event is present, nothing uncommitted is, and the
//! whole store verifies.
//!
//! This simulates **process crashes**. It does not simulate operating-system
//! crashes or power loss, which would need a VM or fault-injecting block
//! device.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;

use common::{TestDb, counter_stream, inc, seqn, sid, triangle};
use orbit_core::fault::FaultPoint;
use orbit_core::projection::examples::Counter;
use orbit_core::{
    Durability, EventStore, Metadata, ReconstructOptions, Reconstructor, StoreConfig, Target,
};

const CHILD_ENV: &str = "ORBIT_CRASH_CHILD";

/// Child-process entry point. A no-op (and an immediate pass) when run as an
/// ordinary test.
#[test]
fn crash_child_entrypoint() {
    let Ok(spec) = std::env::var(CHILD_ENV) else {
        return;
    };
    let (scenario, path) = spec.split_once('|').expect("scenario|path");
    let store = EventStore::open(StoreConfig::new(path).with_durability(Durability::Full)).unwrap();
    let id = sid("crash");
    match scenario {
        "abort_append_before_commit" => abort_at(&store, FaultPoint::AppendBeforeCommit),
        "abort_append_after_insert" => abort_at(&store, FaultPoint::AppendAfterInsert),
        "abort_append_after_begin" => abort_at(&store, FaultPoint::AppendAfterBegin),
        "abort_snapshot_before_commit" => {
            store.set_fault_hook(Some(Arc::new(|p| {
                if p == FaultPoint::SnapshotBeforeCommit {
                    std::process::abort();
                }
                false
            })));
            let _ = Reconstructor::new(&store, Counter).create_snapshot(
                &id,
                Target::Latest,
                ReconstructOptions::default(),
            );
            unreachable!("the child must abort inside create_snapshot");
        }
        "append_forever" => {
            let mut out = std::io::stdout().lock();
            loop {
                let event = store.append(&id, inc(1)).unwrap();
                // Reported only after `append` returned, i.e. after COMMIT.
                writeln!(out, "committed {}", event.sequence).unwrap();
                out.flush().unwrap();
            }
        }
        other => panic!("unknown scenario {other}"),
    }
}

fn abort_at(store: &EventStore, point: FaultPoint) -> ! {
    // Two ordinary commits first, then a crash inside the third append.
    store.append(&sid("crash"), inc(100)).unwrap();
    store.append(&sid("crash"), inc(200)).unwrap();
    store.set_fault_hook(Some(Arc::new(move |p| {
        if p == point {
            std::process::abort();
        }
        false
    })));
    let _ = store.append(&sid("crash"), inc(300));
    unreachable!("the child must abort inside append");
}

fn child(scenario: &str, path: &Path) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "crash_child_entrypoint",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, format!("{scenario}|{}", path.display()));
    command
}

fn prepared_db(events: u64) -> TestDb {
    let db = TestDb::new();
    let store = EventStore::open(StoreConfig::new(&db.path)).unwrap();
    counter_stream(&store, &sid("crash"), events);
    db
}

fn assert_recovered(db: &TestDb, expected_events: u64) -> EventStore {
    let store = db.open();
    let report = store.verify_store().unwrap();
    assert!(report.is_valid(), "{report:#?}");
    let head = store.head(&sid("crash")).unwrap();
    assert_eq!(head.sequence, seqn(expected_events));
    store
}

#[test]
fn process_abort_inside_append_loses_only_the_uncommitted_event() {
    for scenario in [
        "abort_append_after_begin",
        "abort_append_after_insert",
        "abort_append_before_commit",
    ] {
        let db = prepared_db(3);
        let status = child(scenario, &db.path)
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(
            !status.success(),
            "{scenario}: the child should have crashed"
        );
        // The crash left a write-ahead log behind for recovery to process.
        assert!(db.path.with_extension("db-wal").exists(), "{scenario}");

        let store = assert_recovered(&db, 5);
        let payloads: Vec<u64> = store
            .read_stream(&sid("crash"))
            .unwrap()
            .iter()
            .map(|e| e.payload["by"].as_u64().unwrap())
            .collect();
        assert_eq!(payloads, vec![1, 2, 3, 100, 200], "{scenario}");
        // The recovered store accepts the next append at the right position.
        assert_eq!(
            store.append(&sid("crash"), inc(300)).unwrap().sequence,
            seqn(6)
        );
    }
}

#[test]
fn process_abort_inside_snapshot_save_leaves_no_snapshot() {
    let db = prepared_db(4);
    let status = child("abort_snapshot_before_commit", &db.path)
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success());
    let store = assert_recovered(&db, 4);
    assert!(store.list_snapshots(&sid("crash")).unwrap().is_empty());
    let r = Reconstructor::new(&store, Counter)
        .create_snapshot(&sid("crash"), Target::Latest, ReconstructOptions::default())
        .unwrap();
    assert_eq!(r.saved.snapshot.state["value"], triangle(4));
}

#[test]
fn sigkill_during_continuous_appends_preserves_every_acknowledged_event() {
    const KILL_AFTER: u64 = 150;
    let db = TestDb::new();
    EventStore::open(StoreConfig::new(&db.path))
        .unwrap()
        .create_stream(&sid("crash"), Metadata::new())
        .unwrap();

    let mut process = child("append_forever", &db.path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut acknowledged = 0;
    for line in BufReader::new(process.stdout.take().unwrap()).lines() {
        let line = line.unwrap();
        if let Some(n) = line.strip_prefix("committed ") {
            acknowledged = n.parse::<u64>().unwrap();
            if acknowledged >= KILL_AFTER {
                break;
            }
        }
    }
    process.kill().unwrap(); // SIGKILL on Unix
    let status = process.wait().unwrap();
    assert!(!status.success());

    let store = db.open();
    let report = store.verify_store().unwrap();
    assert!(report.is_valid(), "{report:#?}");
    let head = store.head(&sid("crash")).unwrap().sequence.get();
    // Every acknowledged commit survived. At most one further append may have
    // committed after the last line the parent read, and appends are atomic.
    assert!(
        head >= acknowledged,
        "head {head} < acknowledged {acknowledged}"
    );
    let events = store.read_stream(&sid("crash")).unwrap();
    assert_eq!(events.len() as u64, head);
    for (i, e) in events.iter().enumerate() {
        assert_eq!(e.sequence.get(), i as u64 + 1);
    }
}
