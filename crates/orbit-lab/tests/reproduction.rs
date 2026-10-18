//! Sprint 8: reproduction classification, and reading and reproducing
//! experiments recorded by Sprint 7 (registry schema v1, result layout v1).

mod common;

use std::path::Path;

use common::*;
use orbit_lab::{IdentityCompleteness, Status};
use serde_json::{Value, json};

#[test]
fn identical_results_reproduce_with_a_full_comparison() {
    let env = Env::new();
    let store = env.store();
    seed(&store, "c", vec![inc(2), dec(5), inc(1)]);
    let lab = env.lab();
    let record = lab
        .create_and_run(
            &spec("c", "orbit.counter", "example.counter-floor-at-zero"),
            &store,
        )
        .unwrap();
    let v = lab.reproduce(record.id, &store).unwrap();
    assert_eq!(v.status, "reproduced");
    assert_eq!(v.detail["compared"], "full_result");
    assert_eq!(v.detail["differences"], json!([]));
    assert_eq!(v.detail["build"]["same_executable"], true);
    assert_eq!(v.detail["identity_completeness"], "executable");
}

#[test]
fn a_changed_result_is_classified_differs_with_what_changed() {
    let env = Env::new();
    let store = env.store();
    seed(&store, "c", vec![inc(1), inc(1)]);
    let lab = env.lab_with(sprint8_catalog());
    SWITCH.store(false, std::sync::atomic::Ordering::SeqCst);
    let record = lab
        .create_and_run(&spec("c", "orbit.counter", "test.switchable"), &store)
        .unwrap();
    assert_eq!(record.outcome.as_deref(), Some("equivalent"));
    SWITCH.store(true, std::sync::atomic::Ordering::SeqCst);
    let v = lab.reproduce(record.id, &store).unwrap();
    SWITCH.store(false, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(v.status, "differs");
    assert_eq!(v.detail["recorded_outcome"], "equivalent");
    assert_eq!(v.detail["reproduced_outcome"], "diverged");
    let differences: Vec<&str> = v.detail["differences"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d.as_str().unwrap())
        .collect();
    assert_eq!(differences, ["right_trace", "result_document"]);
}

#[test]
fn tampered_configuration_or_other_comparison_rules_are_a_configuration_mismatch() {
    let env = Env::new();
    let store = env.store();
    seed(&store, "c", vec![inc(1)]);
    let lab = env.lab();
    let a = lab
        .create_and_run(&spec("c", "orbit.counter", "orbit.counter"), &store)
        .unwrap();
    let b = lab
        .create_and_run(&spec("c", "orbit.counter", "orbit.counter"), &store)
        .unwrap();
    let raw = env.raw_registry();
    raw.execute_batch(
        "DROP TRIGGER experiments_lifecycle; DROP TRIGGER experiments_identity_is_immutable;",
    )
    .unwrap();
    raw.execute(
        "UPDATE experiments SET spec = replace(spec, '\"from\":0', '\"from\":1') WHERE id = ?1",
        [a.id.as_uuid().as_bytes().as_slice()],
    )
    .unwrap();
    raw.execute(
        "UPDATE experiments SET result = replace(result, '\"version\":1', '\"version\":99') WHERE id = ?1",
        [b.id.as_uuid().as_bytes().as_slice()],
    )
    .unwrap();
    let va = lab.reproduce(a.id, &store).unwrap();
    assert_eq!(va.status, "configuration_mismatch");
    assert!(va.detail["reason"].as_str().unwrap().contains("digest"));
    let vb = lab.reproduce(b.id, &store).unwrap();
    assert_eq!(vb.status, "configuration_mismatch");
    assert_eq!(vb.detail["recorded_semantics_version"], 99);
}

#[test]
fn nondeterministic_results_are_never_reproduced() {
    let env = Env::new();
    let store = env.store();
    seed(&store, "c", vec![inc(1)]);
    let lab = env.lab();
    let record = lab
        .create_and_run(&spec("c", "orbit.counter", "test.global-counter"), &store)
        .unwrap();
    assert_eq!(record.reproducibility.as_deref(), Some("not_reproducible"));
    let v = lab.reproduce(record.id, &store).unwrap();
    assert_eq!(v.status, "not_reproducible");
    assert!(
        v.detail["reasons"].as_array().unwrap()[0]
            .as_str()
            .unwrap()
            .contains("different results on two executions")
    );
}

#[test]
fn every_classification_leaves_a_recorded_verification() {
    let env = Env::new();
    let store = env.store();
    let id = seed(&store, "c", vec![inc(1), inc(2)]);
    let lab = env.lab();
    let record = lab
        .create_and_run(
            &spec("c", "orbit.counter", "example.counter-floor-at-zero"),
            &store,
        )
        .unwrap();
    assert_eq!(
        lab.reproduce(record.id, &store).unwrap().status,
        "reproduced"
    );
    // Missing implementation.
    let bare = env.lab_with(orbit_lab::Catalog::new());
    assert_eq!(
        bare.reproduce(record.id, &store).unwrap().status,
        "implementation_unavailable"
    );
    drop(bare);
    // Changed history (a different store with a same-named stream).
    let other = Env::new();
    let other_store = other.store();
    seed(&other_store, "c", vec![inc(1), inc(2)]);
    assert_eq!(
        lab.reproduce(record.id, &other_store).unwrap().status,
        "history_changed"
    );
    // Unavailable history: too short.
    let short = Env::new();
    let short_store = short.store();
    seed(&short_store, "c", vec![inc(1)]);
    let v = lab.reproduce(record.id, &short_store).unwrap();
    assert_eq!(v.status, "history_unavailable");
    assert_eq!(v.detail["failure"]["kind"], "history_unavailable");
    // Extra events after the pinned range are irrelevant.
    store.append(&id, inc(7)).unwrap();
    assert_eq!(
        lab.reproduce(record.id, &store).unwrap().status,
        "reproduced"
    );
    let statuses: Vec<String> = lab
        .verifications(record.id)
        .unwrap()
        .into_iter()
        .map(|v| v.status)
        .collect();
    assert_eq!(
        statuses,
        [
            "reproduced",
            "implementation_unavailable",
            "history_changed",
            "history_unavailable",
            "reproduced"
        ]
    );
}

/// Loads the Sprint 7 fixture (a real event store and a v1 registry with
/// artifacts) into a fresh directory.
fn sprint7() -> Env {
    let env = Env::new();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sprint7");
    let load = |sql: &str, db: &Path| {
        let text = std::fs::read_to_string(fixtures.join(sql)).unwrap();
        rusqlite::Connection::open(db)
            .unwrap()
            .execute_batch(&text)
            .unwrap();
    };
    load("orbit.sql", &env.db);
    std::fs::create_dir_all(&env.lab_dir).unwrap();
    load("lab.sql", &env.lab_dir.join("lab.db"));
    copy_dir(
        &fixtures.join("sha256"),
        &env.lab_dir.join("artifacts/sha256"),
    );
    env
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn sprint7_experiments_remain_readable_after_the_v2_migration() {
    let env = sprint7();
    let lab = env.lab();
    let version: i64 = env
        .raw_registry()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 3, "migrated in place (v1 → v2 → v3)");
    let list = lab.list(10, None).unwrap();
    assert_eq!(list.len(), 3);
    let statuses: Vec<Status> = list.iter().map(|s| s.status).collect();
    assert_eq!(
        statuses,
        [Status::Failed, Status::Completed, Status::Completed]
    );
    let diverged = lab.experiment(list[2].id).unwrap();
    assert_eq!(diverged.outcome.as_deref(), Some("diverged"));
    let result = diverged.result.as_ref().unwrap();
    assert!(
        result.get("result_version").is_none(),
        "an untouched Sprint 7 result"
    );
    assert_eq!(
        result["divergent_segments"],
        json!([{ "first": 2, "last": 2 }, { "first": 5, "last": 5 }])
    );
    assert!(diverged.build.executable_digest.is_none());
    assert_eq!(
        diverged.identity.completeness,
        IdentityCompleteness::SourceRevision
    );
    assert!(
        diverged
            .identity
            .gaps
            .iter()
            .any(|g| g.contains("no executable digest"))
    );
    let (report, _) = lab.verify_artifacts(diverged.id).unwrap();
    assert_eq!(report.status, "intact");
    assert_eq!(lab.trace(diverged.id, None, 100).unwrap().entries.len(), 6);
    let failed = lab.experiment(list[0].id).unwrap();
    assert_eq!(
        serde_json::to_value(failed.failure.unwrap().kind).unwrap(),
        "history_unavailable"
    );
}

#[test]
fn sprint7_results_reproduce_by_traces_and_summary() {
    let env = sprint7();
    let store = env.store();
    let lab = env.lab();
    let list = lab.list(10, None).unwrap();
    for summary in list.iter().filter(|s| s.status == Status::Completed) {
        let v = lab.reproduce(summary.id, &store).unwrap();
        assert_eq!(v.status, "reproduced", "{}", v.detail);
        assert_eq!(v.detail["compared"], "traces_and_summary");
        assert_eq!(v.detail["recorded_result_version"], 1);
        assert_eq!(
            v.detail["build"]["same_executable"],
            Value::Null,
            "unknown for Sprint 7 records"
        );
    }
    let failed = list.iter().find(|s| s.status == Status::Failed).unwrap();
    assert!(matches!(
        lab.reproduce(failed.id, &store),
        Err(orbit_lab::LabError::InvalidState { .. })
    ));
}
