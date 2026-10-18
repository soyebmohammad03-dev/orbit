//! Registry persistence, lifecycle, recovery and artifacts.

mod common;

use std::fs;
use std::sync::Arc;

use common::*;
use orbit_lab::artifacts::{ArtifactState, ArtifactStore};
use orbit_lab::registry::{Failure, FailureKind, NewExperiment, Registry};
use orbit_lab::{BuildInfo, ExperimentId, LabError, Status};
use serde_json::json;

fn new_experiment(stream: &str) -> NewExperiment {
    NewExperiment {
        spec: spec(stream, "orbit.counter", "orbit.counter"),
        implementations: json!([]),
        build: BuildInfo::current(),
    }
}

#[test]
fn initialization_is_deterministic_and_refuses_foreign_files() {
    let env = Env::new();
    fs::create_dir_all(&env.lab_dir).unwrap();
    let path = env.lab_dir.join("lab.db");
    drop(Registry::open(&path).unwrap());
    let raw = env.raw_registry();
    let version: i64 = raw
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    let app: i64 = raw
        .query_row("PRAGMA application_id", [], |r| r.get(0))
        .unwrap();
    assert_eq!((version, app), (2, orbit_lab::registry::APPLICATION_ID));
    let mut tables: Vec<String> = raw
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    tables.sort();
    assert_eq!(
        tables,
        [
            "artifacts",
            "experiment_artifacts",
            "experiments",
            "verifications"
        ]
    );
    // Reopening is a no-op.
    drop(Registry::open(&path).unwrap());

    // An event store is not a registry, and vice versa.
    drop(env.store());
    assert!(matches!(
        Registry::open(&env.db),
        Err(LabError::NotALabRegistry(_))
    ));
    assert!(orbit_core::EventStore::open(orbit_core::StoreConfig::new(&path)).is_err());

    // A newer schema is refused.
    raw.execute_batch("PRAGMA user_version = 99").unwrap();
    assert!(matches!(
        Registry::open(&path),
        Err(LabError::UnsupportedSchemaVersion {
            found: 99,
            supported: 2
        })
    ));

    // A file with someone else's schema is refused.
    let other = env.dir.path().join("other.db");
    rusqlite::Connection::open(&other)
        .unwrap()
        .execute_batch("CREATE TABLE t (x)")
        .unwrap();
    assert!(matches!(
        Registry::open(&other),
        Err(LabError::NotALabRegistry(_))
    ));
}

#[test]
fn lifecycle_transitions_are_enforced() {
    let env = Env::new();
    fs::create_dir_all(&env.lab_dir).unwrap();
    let registry = Registry::open(&env.lab_dir.join("lab.db")).unwrap();
    let id = registry.create(&new_experiment("s")).unwrap();
    let record = registry.get(id).unwrap();
    assert_eq!(record.status, Status::Created);
    assert!(record.started_at.is_none() && record.result.is_none());

    registry.start(id).unwrap();
    assert!(matches!(
        registry.start(id),
        Err(LabError::InvalidTransition {
            from: Status::Running,
            to: Status::Running,
            ..
        })
    ));
    let failure = Failure::new(FailureKind::Internal, "boom");
    registry.fail(id, &failure, None, Some(5)).unwrap();
    let failed = registry.get(id).unwrap();
    assert_eq!(failed.status, Status::Failed);
    assert_eq!(failed.failure, Some(failure.clone()));
    assert!(failed.finished_at.is_some());
    for attempt in [registry.start(id), registry.fail(id, &failure, None, None)] {
        assert!(matches!(
            attempt,
            Err(LabError::InvalidTransition {
                from: Status::Failed,
                ..
            })
        ));
    }

    // created → failed directly is allowed (for example a validation problem).
    let other = registry.create(&new_experiment("s")).unwrap();
    registry.fail(other, &failure, None, None).unwrap();
    let missing = ExperimentId::generate();
    assert!(matches!(
        registry.start(missing),
        Err(LabError::ExperimentNotFound(_))
    ));
}

#[test]
fn triggers_reject_invalid_updates_even_from_raw_sql() {
    let env = Env::new();
    let lab = env.lab();
    let store = env.store();
    seed(&store, "c", vec![inc(1)]);
    let done = lab
        .create_and_run(&spec("c", "orbit.counter", "orbit.counter"), &store)
        .unwrap();
    let created = lab
        .create(&spec("c", "orbit.counter", "orbit.counter"))
        .unwrap();
    let raw = env.raw_registry();
    let id = |e: &orbit_lab::ExperimentRecord| e.id.as_uuid().as_bytes().to_vec();
    let attempts = [
        (
            "UPDATE experiments SET status = 'running' WHERE id = ?1",
            id(&done),
        ),
        (
            "UPDATE experiments SET outcome = 'equivalent' WHERE id = ?1",
            id(&done),
        ),
        (
            "UPDATE experiments SET status = 'completed' WHERE id = ?1",
            id(&created),
        ),
        (
            "UPDATE experiments SET spec = '{}' WHERE id = ?1",
            id(&created),
        ),
        ("DELETE FROM experiments WHERE id = ?1", id(&done)),
        (
            "DELETE FROM experiment_artifacts WHERE experiment_id = ?1",
            id(&done),
        ),
        (
            "UPDATE experiment_artifacts SET role = 'x' WHERE experiment_id = ?1",
            id(&done),
        ),
    ];
    for (sql, param) in attempts {
        assert!(raw.execute(sql, [param]).is_err(), "{sql} must be rejected");
    }
    assert!(raw.execute("DELETE FROM artifacts", []).is_err());
    lab.verify_artifacts(done.id).unwrap();
    assert!(
        raw.execute("UPDATE verifications SET status = 'x'", [])
            .is_err()
    );
    assert!(raw.execute("DELETE FROM verifications", []).is_err());
    // Completed without a result violates the CHECK constraint.
    let running = lab
        .create(&spec("c", "orbit.counter", "orbit.counter"))
        .unwrap();
    lab.registry().start(running.id).unwrap();
    assert!(
        raw.execute(
            "UPDATE experiments SET status = 'completed' WHERE id = ?1",
            [id(&running)]
        )
        .is_err()
    );
    assert_eq!(lab.experiment(done.id).unwrap(), done);
}

#[test]
fn experiments_persist_across_restarts() {
    let env = Env::new();
    let store = env.store();
    seed(&store, "c", vec![inc(1), dec(3)]);
    let (done, failed) = {
        let lab = env.lab();
        let done = lab
            .create_and_run(
                &spec("c", "orbit.counter", "example.counter-floor-at-zero"),
                &store,
            )
            .unwrap();
        let failed = lab
            .create_and_run(&spec("gone", "orbit.counter", "orbit.counter"), &store)
            .unwrap();
        (done, failed)
    };
    let lab = env.lab();
    assert_eq!(lab.experiment(done.id).unwrap(), done);
    assert_eq!(lab.experiment(failed.id).unwrap(), failed);
    assert_eq!(done.status, Status::Completed);
    assert_eq!(failed.status, Status::Failed);
    let listed = lab.list(10, None).unwrap();
    assert_eq!(
        listed.iter().map(|s| s.id).collect::<Vec<_>>(),
        [failed.id, done.id]
    );
    assert_eq!(listed[1].outcome.as_deref(), Some("diverged"));
    assert_eq!(listed[1].left, "orbit.counter");
    assert_eq!(listed[1].right, "example.counter-floor-at-zero");
    // Cursor pagination.
    let page = lab.list(1, Some(failed.id)).unwrap();
    assert_eq!(page.iter().map(|s| s.id).collect::<Vec<_>>(), [done.id]);
    assert!(lab.list(10, Some(done.id)).unwrap().is_empty());
    // Recorded identity and build.
    assert_eq!(done.implementations[0]["side"], "left");
    assert_eq!(
        done.implementations[1]["descriptor"]["id"],
        "example.counter-floor-at-zero"
    );
    assert_eq!(done.build.orbit_version, env!("CARGO_PKG_VERSION"));
    assert!(done.started_at.unwrap() <= done.finished_at.unwrap());
    let created_json = serde_json::to_value(&done).unwrap();
    assert!(
        created_json["created_at"].as_str().unwrap().ends_with('Z'),
        "RFC 3339 UTC"
    );
}

#[test]
fn concurrent_registries_on_one_file_lose_nothing() {
    let env = Env::new();
    drop(env.lab());
    let path = env.lab_dir.join("lab.db");
    let threads: Vec<_> = (0..4)
        .map(|t| {
            let path = path.clone();
            std::thread::spawn(move || {
                let registry = Registry::open(&path).unwrap();
                (0..25)
                    .map(|_| {
                        let id = registry.create(&new_experiment(&format!("s{t}"))).unwrap();
                        registry.start(id).unwrap();
                        registry
                            .fail(id, &Failure::new(FailureKind::Internal, "test"), None, None)
                            .unwrap();
                        id
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let mut ids: Vec<ExperimentId> = threads
        .into_iter()
        .flat_map(|t| t.join().unwrap())
        .collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 100);
    let registry = Registry::open(&path).unwrap();
    assert_eq!(registry.list(1000, None).unwrap().len(), 100);
    // A shared registry used from several threads at once.
    let shared = Arc::new(registry);
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || shared.create(&new_experiment("t")).unwrap())
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(shared.list(1000, None).unwrap().len(), 104);
}

#[test]
fn interrupted_experiments_are_marked_failed_but_live_ones_are_not() {
    let env = Env::new();
    let lab = env.lab();
    let orphan = lab
        .create(&spec("c", "orbit.counter", "orbit.counter"))
        .unwrap();
    lab.registry().start(orphan.id).unwrap(); // running, but nobody holds its lock
    let live = lab
        .create(&spec("c", "orbit.counter", "orbit.counter"))
        .unwrap();
    lab.registry().start(live.id).unwrap();
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(env.lab_dir.join("locks").join(format!("{}.lock", live.id)))
        .unwrap();
    lock.try_lock().unwrap(); // as a running process would
    drop(lab);

    let lab = env.lab();
    let orphan = lab.experiment(orphan.id).unwrap();
    assert_eq!(orphan.status, Status::Failed);
    assert_eq!(orphan.failure.unwrap().kind, FailureKind::Interrupted);
    assert!(orphan.result.is_none(), "never completed");
    assert_eq!(lab.experiment(live.id).unwrap().status, Status::Running);

    drop(lock); // the "process" exits
    assert_eq!(lab.recover_interrupted().unwrap(), [live.id]);
    assert_eq!(lab.experiment(live.id).unwrap().status, Status::Failed);
}

#[test]
fn only_created_experiments_run_and_only_finished_ones_verify() {
    let env = Env::new();
    let store = env.store();
    seed(&store, "c", vec![inc(1)]);
    let lab = env.lab();
    let record = lab
        .create(&spec("c", "orbit.counter", "orbit.counter"))
        .unwrap();
    assert!(matches!(
        lab.verify_artifacts(record.id),
        Err(LabError::InvalidState { .. })
    ));
    assert!(matches!(
        lab.reproduce(record.id, &store),
        Err(LabError::InvalidState { .. })
    ));
    let done = lab.run(record.id, &store).unwrap();
    assert_eq!(done.status, Status::Completed);
    assert!(matches!(
        lab.run(record.id, &store),
        Err(LabError::InvalidTransition {
            from: Status::Completed,
            ..
        })
    ));
}

#[test]
fn an_implementation_whose_identity_changed_fails_the_run() {
    let env = Env::new();
    let store = env.store();
    seed(&store, "c", vec![inc(1)]);
    let record = env
        .lab()
        .create(&spec("c", "orbit.counter", "orbit.counter"))
        .unwrap();
    // Same id, different implementation type: a different identity.
    let mut catalog = orbit_lab::Catalog::new();
    catalog
        .register(Arc::new(
            orbit_lab::ProjectionImplementation::new(
                "orbit.counter",
                "Counter",
                orbit_lab::variants::CounterFloorAtZero,
                orbit_lab::Determinism::Pure,
            )
            .unwrap(),
        ))
        .unwrap();
    let lab = env.lab_with(catalog);
    let run = lab.run(record.id, &store).unwrap();
    assert_eq!(run.status, Status::Failed);
    assert_eq!(
        run.failure.unwrap().kind,
        FailureKind::ImplementationUnavailable
    );
}

#[test]
fn artifacts_are_content_addressed_and_verified() {
    let env = Env::new();
    let store = env.store();
    seed(&store, "c", vec![inc(1), dec(4)]);
    let lab = env.lab();
    let record = lab
        .create_and_run(
            &spec("c", "orbit.counter", "example.counter-floor-at-zero"),
            &store,
        )
        .unwrap();
    let mut roles: Vec<&str> = record.artifacts.iter().map(|a| a.role.as_str()).collect();
    roles.sort_unstable();
    assert_eq!(
        roles,
        [
            "final_states",
            "first_divergence_states",
            "left_trace",
            "right_trace"
        ]
    );
    let (report, stored) = lab.verify_artifacts(record.id).unwrap();
    assert_eq!(report.status, "intact");
    assert!(report.spec_digest_ok);
    assert_eq!(report.result_digest_ok, Some(true));
    assert_eq!(report.traces_consistent, Some(true));
    assert_eq!(stored.status, "intact");

    // Portable metadata holds digests, never paths.
    let text = serde_json::to_string(&record).unwrap();
    assert!(!text.contains(env.dir.path().to_str().unwrap()));
    assert!(!text.contains("/artifacts/"));

    // Corrupt one artifact and delete another.
    let trace = record
        .artifacts
        .iter()
        .find(|a| a.role == "left_trace")
        .unwrap();
    let states = record
        .artifacts
        .iter()
        .find(|a| a.role == "final_states")
        .unwrap();
    let path = lab.artifacts().local_path(&trace.digest);
    let mut bytes = fs::read(&path).unwrap();
    bytes[0] ^= 0xff;
    fs::write(&path, bytes).unwrap();
    fs::remove_file(lab.artifacts().local_path(&states.digest)).unwrap();
    let (report, _) = lab.verify_artifacts(record.id).unwrap();
    assert_eq!(report.status, "damaged");
    let state_of = |role: &str| {
        report
            .artifacts
            .iter()
            .find(|a| a.artifact.role == role)
            .unwrap()
            .state
    };
    assert_eq!(state_of("left_trace"), ArtifactState::Corrupted);
    assert_eq!(state_of("final_states"), ArtifactState::Missing);
    assert_eq!(state_of("right_trace"), ArtifactState::Intact);
    assert!(matches!(
        lab.artifact(record.id, "left_trace"),
        Err(LabError::ArtifactCorrupted(_))
    ));
    assert!(matches!(
        lab.artifact(record.id, "final_states"),
        Err(LabError::ArtifactMissing(_))
    ));
    assert!(matches!(
        lab.artifact(record.id, "nope"),
        Err(LabError::InvalidState { .. })
    ));
    let history: Vec<String> = lab
        .verifications(record.id)
        .unwrap()
        .into_iter()
        .map(|v| v.status)
        .collect();
    assert_eq!(history, ["intact", "damaged"]);
}

#[test]
fn tampered_records_fail_verification() {
    let env = Env::new();
    let store = env.store();
    seed(&store, "c", vec![inc(1)]);
    let lab = env.lab();
    let record = lab
        .create_and_run(&spec("c", "orbit.counter", "orbit.counter"), &store)
        .unwrap();
    let raw = env.raw_registry();
    raw.execute_batch(
        "DROP TRIGGER experiments_lifecycle;
         DROP TRIGGER experiments_identity_is_immutable;",
    )
    .unwrap();
    raw.execute(
        "UPDATE experiments SET result = replace(result, '\"equivalent\"', '\"diverged\"') WHERE id = ?1",
        [record.id.as_uuid().as_bytes().as_slice()],
    )
    .unwrap();
    let (report, _) = lab.verify_artifacts(record.id).unwrap();
    assert_eq!(report.status, "damaged");
    assert_eq!(report.result_digest_ok, Some(false));
    assert!(report.spec_digest_ok);
}

#[test]
fn the_artifact_store_writes_atomically_and_deduplicates() {
    let env = Env::new();
    let store = ArtifactStore::open(&env.dir.path().join("a")).unwrap();
    let digest = store.put(b"hello").unwrap();
    assert_eq!(store.put(b"hello").unwrap(), digest);
    assert_eq!(store.read(&digest).unwrap(), b"hello");
    assert_eq!(store.check(&digest, 5).unwrap(), ArtifactState::Intact);
    assert_eq!(store.check(&digest, 6).unwrap(), ArtifactState::Corrupted);
    let tmp: Vec<_> = fs::read_dir(env.dir.path().join("a/tmp"))
        .unwrap()
        .collect();
    assert!(tmp.is_empty(), "no temporary files left behind");
    // A damaged file is replaced by a later put of the same content.
    fs::write(store.local_path(&digest), b"HELLO").unwrap();
    assert!(matches!(
        store.read(&digest),
        Err(LabError::ArtifactCorrupted(_))
    ));
    store.put(b"hello").unwrap();
    assert_eq!(store.read(&digest).unwrap(), b"hello");
    // Symbolic links are never followed.
    #[cfg(unix)]
    {
        let target = env.dir.path().join("elsewhere");
        fs::write(&target, b"hello").unwrap();
        let path = store.local_path(&digest);
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert_eq!(store.check(&digest, 5).unwrap(), ArtifactState::Missing);
    }
}
