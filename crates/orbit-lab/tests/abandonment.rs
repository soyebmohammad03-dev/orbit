//! Creation locks: an experiment whose creator is gone before it starts is
//! failed/abandoned, and nothing else is. (The same rule with a real killed
//! process is the `experiment_created/process_killed` reliability case.)

mod common;

use common::{Env, inc, seed, spec};
use orbit_lab::Status;
use orbit_lab::registry::FailureKind;

#[test]
fn a_live_creator_keeps_its_experiment() {
    let env = Env::new();
    let creator = env.lab();
    let record = creator
        .create(&spec("c", "orbit.counter", "orbit.counter"))
        .unwrap();
    // Opening the lab again recovers; the creator still holds its lock.
    let other = env.lab();
    assert_eq!(other.experiment(record.id).unwrap().status, Status::Created);
    assert!(other.recover_abandoned().unwrap().is_empty());
    // The creator can still run it.
    let store = env.store();
    seed(&store, "c", vec![inc(1)]);
    assert_eq!(
        creator.run(record.id, &store).unwrap().status,
        Status::Completed
    );
    assert!(
        !env.lab_dir
            .join("locks")
            .join(format!("{}.create", record.id))
            .exists(),
        "starting removes the creation lock file"
    );
}

#[test]
fn a_dropped_creator_abandons_its_experiment_exactly_once() {
    let env = Env::new();
    let id = {
        let creator = env.lab();
        creator
            .create(&spec("c", "orbit.counter", "orbit.counter"))
            .unwrap()
            .id
    };
    let lab = env.lab();
    let record = lab.experiment(id).unwrap();
    assert_eq!(record.status, Status::Failed);
    let failure = record.failure.unwrap();
    assert_eq!(failure.kind, FailureKind::Abandoned);
    assert!(record.result.is_none() && record.started_at.is_none());
    assert!(lab.recover_abandoned().unwrap().is_empty());
    // Abandoned experiments cannot be started afterwards.
    let store = env.store();
    assert!(lab.run(id, &store).is_err());
}

#[test]
fn a_released_experiment_is_never_abandoned() {
    let env = Env::new();
    let id = {
        let creator = env.lab();
        let id = creator
            .create(&spec("c", "orbit.counter", "orbit.counter"))
            .unwrap()
            .id;
        creator.release(id);
        id
    };
    let lab = env.lab();
    assert_eq!(lab.experiment(id).unwrap().status, Status::Created);
    let store = env.store();
    seed(&store, "c", vec![inc(1)]);
    assert_eq!(lab.run(id, &store).unwrap().status, Status::Completed);
}

#[test]
fn a_started_experiment_is_never_abandoned() {
    let env = Env::new();
    let id = {
        let creator = env.lab();
        let id = creator
            .create(&spec("c", "orbit.counter", "orbit.counter"))
            .unwrap()
            .id;
        // Started by someone else while the creator still holds its lock
        // (the start race of the registry-concurrency scenario).
        env.lab_with(common::test_catalog())
            .registry()
            .start(id)
            .unwrap();
        id
    };
    // The creator is gone and its lock file remains, but the experiment
    // is running: it may be interrupted, never abandoned.
    let lab = orbit_lab::Lab::open_without_recovery(&env.lab_dir, common::test_catalog()).unwrap();
    assert!(lab.recover_abandoned().unwrap().is_empty());
    assert_eq!(lab.experiment(id).unwrap().status, Status::Running);
    assert_eq!(lab.recover_interrupted().unwrap(), vec![id]);
    assert_eq!(
        lab.experiment(id).unwrap().failure.unwrap().kind,
        FailureKind::Interrupted
    );
}

#[test]
fn experiments_without_a_creation_lock_file_are_left_alone() {
    let env = Env::new();
    let id = {
        let creator = env.lab();
        creator
            .create(&spec("c", "orbit.counter", "orbit.counter"))
            .unwrap()
            .id
    };
    // As if created by a version without creation locks.
    std::fs::remove_file(env.lab_dir.join("locks").join(format!("{id}.create"))).unwrap();
    let lab = env.lab();
    assert_eq!(lab.experiment(id).unwrap().status, Status::Created);
}

/// Regression: a run killed mid-measurement leaves its work directory; the
/// next `Lab::open` removes it once the experiment is no longer running,
/// and leaves directories it does not own alone.
#[test]
fn work_directories_of_finished_runs_are_removed_on_open() {
    let env = Env::new();
    let id = {
        let lab = env.lab();
        let record = lab
            .measure_performance(
                &orbit_lab::performance::PerformanceSpec::from_json(serde_json::json!({
                    "workload": "integrity-verification", "input_size": 5, "measured_iterations": 1,
                }))
                .unwrap(),
            )
            .unwrap();
        record.id
    };
    let stale = env.lab_dir.join("performance-work").join(id.to_string());
    let unknown = env
        .lab_dir
        .join("reliability-work")
        .join(format!("{id}-reproduction-x"));
    let foreign = env
        .lab_dir
        .join("reliability-work")
        .join(orbit_lab::ExperimentId::generate().to_string());
    for dir in [&stale, &unknown, &foreign] {
        std::fs::create_dir_all(dir.join("inner")).unwrap();
    }
    let _lab = env.lab();
    assert!(!stale.exists(), "the finished run's directory is removed");
    assert!(!foreign.exists(), "an id with no experiment is not running");
    assert!(unknown.exists(), "other names are left alone");
}
