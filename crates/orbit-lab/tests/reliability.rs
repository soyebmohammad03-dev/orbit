//! Reliability scenarios, end to end: real child processes (the
//! `orbit-reliability` binary), persisted results, reproduction, and the
//! separation between simulated and real failures.

mod common;

use std::path::Path;
use std::process::Command;

use common::*;
use orbit_lab::registry::FailureKind;
use orbit_lab::reliability::{ReliabilitySpec, SCENARIOS, runner};
use orbit_lab::{Catalog, ExperimentRecord, Lab, Status};
use serde_json::{Value, json};

const EXE: &str = env!("CARGO_BIN_EXE_orbit-reliability");

fn run(lab: &Lab, scenario: &str, parameters: Value) -> ExperimentRecord {
    let spec =
        ReliabilitySpec::from_json(json!({ "scenario": scenario, "parameters": parameters }))
            .unwrap();
    let created = lab.create_reliability(&spec).unwrap();
    assert_eq!(created.status, Status::Created);
    runner::run(lab, created.id, Path::new(EXE)).unwrap()
}

fn cases(record: &ExperimentRecord) -> Vec<Value> {
    record.result.as_ref().unwrap()["cases"]
        .as_array()
        .unwrap()
        .clone()
}

/// Every case observed its fault and preserved every invariant.
fn assert_all_preserved(record: &ExperimentRecord) {
    assert_eq!(record.status, Status::Completed, "{:?}", record.failure);
    for case in cases(record) {
        let failed: Vec<&Value> = case["invariants"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["held"] != true)
            .collect();
        assert_eq!(
            case["fault_observed"], true,
            "{}: {}",
            case["id"], case["fault_evidence"]
        );
        assert!(failed.is_empty(), "{}: {failed:?}", case["id"]);
        assert_eq!(case["verdict"], "invariant_preserved", "{}", case["id"]);
    }
    assert_eq!(record.outcome.as_deref(), Some("invariants_preserved"));
    assert_eq!(record.reproducibility.as_deref(), Some("unverified"));
}

fn ids(record: &ExperimentRecord) -> Vec<String> {
    cases(record)
        .iter()
        .map(|c| c["id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn event_append_interruption_commits_nothing_and_keeps_acknowledged_events() {
    let env = Env::new();
    let lab = env.lab();
    let record = run(&lab, "event-append-interruption", json!({ "events": 5 }));
    assert_all_preserved(&record);
    let ids = ids(&record);
    assert_eq!(ids.len(), 12);
    for point in [
        "append_before_begin",
        "append_after_begin",
        "append_after_sequence_allocated",
        "append_after_insert",
        "append_before_commit",
    ] {
        assert!(ids.contains(&format!("{point}/simulated_error")));
        assert!(ids.contains(&format!("{point}/process_abort")));
    }
    let all = cases(&record);
    let abort = all
        .iter()
        .find(|c| c["id"] == "append_after_insert/process_abort")
        .unwrap();
    assert_eq!(abort["mode"], "process_abort");
    assert_eq!(abort["real_process"], true);
    #[cfg(unix)]
    assert!(
        abort["fault_evidence"]
            .as_str()
            .unwrap()
            .contains("SIGABRT")
    );
    let simulated = all
        .iter()
        .find(|c| c["id"] == "append_after_insert/simulated_error")
        .unwrap();
    assert_eq!(
        simulated["real_process"], false,
        "simulated errors are labelled as such"
    );
    let names: Vec<&str> = abort["invariants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "store_reopens",
            "nothing_committed",
            "committed_events_unchanged",
            "dense_sequences",
            "store_verifies",
            "next_append_continues"
        ]
    );
    let killed = all.iter().find(|c| c["mode"] == "process_killed").unwrap();
    assert!(killed["recovery"]["acknowledged"].as_u64().unwrap() > 5);
    let corrupt = all
        .iter()
        .find(|c| c["mode"] == "deliberate_corruption")
        .unwrap();
    assert_eq!(
        corrupt["integrity"]["valid"], false,
        "the corruption is detected"
    );
    // The work directory is removed; the user's files were never touched.
    assert!(
        !env.lab_dir
            .join("reliability-work")
            .join(record.id.to_string())
            .exists()
    );
    assert!(
        !env.db.exists(),
        "no event store was created outside the work directory"
    );
}

#[test]
fn snapshot_write_interruption_stores_no_snapshot() {
    let env = Env::new();
    let lab = env.lab();
    let record = run(&lab, "snapshot-write-interruption", json!({ "events": 3 }));
    assert_all_preserved(&record);
    assert_eq!(cases(&record).len(), 6);
    assert_eq!(
        record.result.as_ref().unwrap()["summary"]["real_process_cases"],
        3
    );
}

#[test]
fn experiment_interruption_is_never_reported_completed() {
    let env = Env::new();
    let lab = env.lab();
    let record = run(&lab, "experiment-interruption", json!({}));
    assert_all_preserved(&record);
    assert_eq!(record.spec["scenario_version"], 2);
    let all = cases(&record);
    assert_eq!(all.len(), 8);
    let abandoned = all
        .iter()
        .find(|c| c["id"] == "experiment_created/process_killed")
        .unwrap();
    assert_eq!(abandoned["fault_observed"], true, "the creator was killed");
    for name in [
        "live_creator_not_abandoned",
        "abandoned_after_creator_died",
        "abandoned_exactly_once",
    ] {
        assert!(
            abandoned["invariants"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["name"] == name && i["held"] == true),
            "{name}"
        );
    }
    let partial = all
        .iter()
        .find(|c| c["id"] == "artifact_after_partial_write/process_abort")
        .unwrap();
    assert!(
        partial["recovery"]["temporary_files_before_recovery"]
            .as_u64()
            .unwrap()
            >= 1,
        "the write really was interrupted"
    );
    let paused = all
        .iter()
        .find(|c| c["id"] == "run_after_start/process_paused")
        .unwrap();
    assert!(
        paused["invariants"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["name"] == "live_experiment_not_interrupted" && i["held"] == true)
    );
    let complete = all
        .iter()
        .find(|c| c["id"] == "run_before_complete/process_abort")
        .unwrap();
    assert!(
        complete["recovery"]["unlinked_artifact_files"]
            .as_u64()
            .unwrap()
            >= 1,
        "artifacts written before the crash stay unlinked"
    );
}

#[test]
fn registry_concurrency_with_separate_processes() {
    let env = Env::new();
    let lab = env.lab();
    let record = run(
        &lab,
        "registry-concurrency",
        json!({ "processes": 4, "experiments_per_process": 6 }),
    );
    assert_all_preserved(&record);
    let all = cases(&record);
    let create = &all[0];
    assert_eq!(create["mode"], "concurrent_processes");
    let lost = create["invariants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["name"] == "none_lost_or_duplicated")
        .unwrap();
    assert_eq!(
        lost["observed"],
        "24 reported, 24 distinct, 24 in the registry"
    );
    let start = &all[1];
    assert_eq!(start["invariants"][0]["observed"], "1 won, 3 lost");
}

#[test]
fn results_persist_and_reproduce_without_changing_the_original() {
    let env = Env::new();
    let id = {
        let lab = env.lab();
        run(&lab, "snapshot-write-interruption", json!({ "events": 2 })).id
    };
    // A new process (a reopened lab) sees the same completed evidence.
    let lab = env.lab();
    let stored = lab.experiment(id).unwrap();
    assert_eq!(stored.kind, "reliability");
    assert_eq!(stored.status, Status::Completed);
    assert!(stored.input.is_none() && stored.implementations == json!([]));
    assert_eq!(stored.spec["scenario_version"], 1);
    let (report, _) = lab.verify_artifacts(id).unwrap();
    assert_eq!(report.status, "intact");
    assert!(
        report.traces_consistent.is_none(),
        "no traces for reliability results"
    );
    let output = lab.artifact(id, "child_output").unwrap();
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("terminated by"));
    assert!(
        !output.contains(env.dir.path().to_str().unwrap()),
        "no local paths in child output"
    );

    let v = runner::reproduce(&lab, id, Path::new(EXE)).unwrap();
    assert_eq!(v.status, "reproduced", "{}", v.detail);
    assert_eq!(v.detail["differences"], json!([]));
    let after = lab.experiment(id).unwrap();
    assert_eq!(
        after, stored,
        "reproduction never modifies the original record"
    );
    assert_eq!(lab.verifications(id).unwrap().len(), 2);
    // The differential path refuses reliability experiments, and vice versa.
    let store = env.store();
    assert!(matches!(
        lab.reproduce(id, &store),
        Err(orbit_lab::LabError::InvalidState { .. })
    ));
    assert!(matches!(
        lab.run(id, &store),
        Err(orbit_lab::LabError::InvalidState { .. })
    ));
    let differential = lab
        .create(&spec("x", "orbit.counter", "orbit.counter"))
        .unwrap();
    assert!(matches!(
        runner::run(&lab, differential.id, Path::new(EXE)),
        Err(orbit_lab::LabError::InvalidState { .. })
    ));
}

#[test]
fn infrastructure_failures_are_not_results() {
    let env = Env::new();
    let lab = env.lab();
    let spec = ReliabilitySpec::from_json(
        json!({ "scenario": "snapshot-write-interruption", "parameters": { "events": 1 } }),
    )
    .unwrap();
    let created = lab.create_reliability(&spec).unwrap();
    let record = runner::run(&lab, created.id, &env.dir.path().join("no-such-binary")).unwrap();
    assert_eq!(record.status, Status::Failed);
    let failure = record.failure.unwrap();
    assert_eq!(failure.kind, FailureKind::Infrastructure);
    assert!(
        failure.message.contains("cannot start child process"),
        "{}",
        failure.message
    );
    assert!(record.result.is_none() && record.outcome.is_none());
    assert!(
        !env.lab_dir
            .join("reliability-work")
            .join(created.id.to_string())
            .exists()
    );
}

#[test]
fn reliability_rows_are_constrained_by_the_v3_schema() {
    let env = Env::new();
    drop(env.lab());
    let raw = env.raw_registry();
    let insert = |kind: &str, stream: Option<&str>| {
        raw.execute(
            "INSERT INTO experiments (id, kind, spec_version, spec, spec_digest, stream_id, implementations, orbit_version, created_at, status)
             VALUES (randomblob(16), ?1, 1, '{}', zeroblob(32), ?2, '[]', '0', 0, 'created')",
            rusqlite::params![kind, stream],
        )
    };
    assert!(insert("reliability", None).is_ok());
    assert!(
        insert("differential_replay", None).is_err(),
        "differential replays still need a stream"
    );
    assert!(insert("chaos", None).is_err());
}

#[test]
fn the_tool_lists_runs_and_rejects_unsupported_requests() {
    let env = Env::new();
    let tool = |args: &[&str]| {
        Command::new(EXE)
            .arg("--lab")
            .arg(&env.lab_dir)
            .args(args)
            .output()
            .unwrap()
    };
    let out = tool(&["scenarios"]);
    assert!(out.status.success());
    let listed: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), SCENARIOS.len());
    for bad in [
        vec!["run", "delete-everything"],
        vec!["run", "registry-concurrency", "--param", "processes=1000"],
        vec!["run", "registry-concurrency", "--param", "path=1"],
    ] {
        let out = tool(&bad);
        assert!(!out.status.success(), "{bad:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("invalid experiment specification"),
            "{bad:?}"
        );
    }
    let out = tool(&["run", "registry-concurrency", "--param", "path=/etc"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("must be a non-negative integer"));
    let out = tool(&["run", "snapshot-write-interruption", "--param", "events=2"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let record: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(record["outcome"], "invariants_preserved");
    // Child roles only come from the command line; a malformed one exits.
    let out = Command::new(EXE)
        .args(["__child", "{\"role\":\"format_disk\"}"])
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn ordinary_lab_runs_have_no_active_fault_hook() {
    // The fault points exist in this build (the feature is on for tests),
    // but nothing is injected unless a hook is installed explicitly.
    let env = Env::new();
    let store = env.store();
    seed(&store, "c", vec![inc(1)]);
    let lab = Lab::open(&env.lab_dir, Catalog::builtin()).unwrap();
    let record = lab
        .create_and_run(&spec("c", "orbit.counter", "orbit.counter"), &store)
        .unwrap();
    assert_eq!(record.status, Status::Completed);
}

/// Regression: a child that appends continuously must not outlive its
/// parent. Closing its stdout pipe (as a killed runner would) makes it exit.
#[test]
fn a_child_exits_when_its_parent_stops_reading() {
    use std::io::{BufRead, BufReader};
    let env = Env::new();
    {
        let store = env.store();
        store
            .create_stream(&common::sid("reliability"), orbit_core::Metadata::new())
            .unwrap();
    }
    let role = json!({ "role": "append_forever", "db": env.db });
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_orbit-reliability"))
        .arg("__child")
        .arg(role.to_string())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    out.read_line(&mut line).unwrap();
    assert!(line.starts_with("committed "), "{line}");
    drop(out);
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(30),
            "the child kept running after its parent stopped reading"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert_eq!(status.code(), Some(4));
}
