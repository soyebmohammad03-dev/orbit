//! The reliability launcher: a separately configured runner process, started
//! with fixed arguments on an experiment recorded beforehand. Uses the real
//! `orbit-reliability` tool, plus two small shell scripts standing in for a
//! runner that hangs and one that exits without doing anything.
#![cfg(unix)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use common::Env;
use orbit_lab::Status;
use orbit_lab::registry::FailureKind;
use orbit_lab::reliability::ReliabilitySpec;
use orbit_lab::reliability::launcher::{LaunchError, LaunchReport, Launcher};
use serde_json::json;

const RUNNER: &str = env!("CARGO_BIN_EXE_orbit-reliability");

fn spec(value: serde_json::Value) -> ReliabilitySpec {
    ReliabilitySpec::from_json(value).unwrap()
}

fn small() -> ReliabilitySpec {
    spec(json!({ "scenario": "snapshot-write-interruption", "parameters": { "events": 2 } }))
}

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Launches and waits for the supervisor's report.
fn run(launcher: &Launcher, lab: &Arc<orbit_lab::Lab>, s: &ReliabilitySpec) -> LaunchReport {
    let (tx, rx) = mpsc::channel();
    launcher
        .launch(Arc::clone(lab), s, move |r| {
            let _ = tx.send(r);
        })
        .unwrap();
    rx.recv_timeout(Duration::from_secs(300))
        .expect("the launch finishes")
}

#[test]
fn configuration_is_validated() {
    let env = Env::new();
    let t = Duration::from_secs(60);
    assert!(
        Launcher::new(Path::new("orbit-reliability"), t).is_err(),
        "relative"
    );
    assert!(
        Launcher::new(&env.dir.path().join("missing"), t).is_err(),
        "missing"
    );
    assert!(Launcher::new(env.dir.path(), t).is_err(), "a directory");
    assert!(Launcher::new(Path::new(RUNNER), Duration::ZERO).is_err());
    assert!(Launcher::new(Path::new(RUNNER), Duration::from_secs(3_601)).is_err());
    assert!(Launcher::new(Path::new(RUNNER), t).is_ok());
}

#[test]
fn only_fixed_arguments_reach_the_runner() {
    let id = orbit_lab::ExperimentId::generate();
    let args = Launcher::arguments(Path::new("/labs/a"), id);
    let expected: Vec<std::ffi::OsString> = ["--lab", "/labs/a", "run-id", &id.to_string()]
        .iter()
        .map(Into::into)
        .collect();
    assert_eq!(args, expected);
}

#[test]
fn invalid_requests_record_nothing_and_do_not_block() {
    let env = Env::new();
    let lab = Arc::new(env.lab());
    let launcher = Launcher::new(Path::new(RUNNER), Duration::from_secs(60)).unwrap();
    for bad in [
        json!({ "scenario": "rm -rf /" }),
        json!({ "scenario": "snapshot-write-interruption", "parameters": { "events": 0 } }),
        json!({ "scenario": "snapshot-write-interruption", "parameters": { "path": 1 } }),
        json!({ "scenario": "snapshot-write-interruption", "scenario_version": 99 }),
    ] {
        let r = launcher.launch(Arc::clone(&lab), &spec(bad.clone()), |_| {});
        assert!(matches!(r, Err(LaunchError::Lab(_))), "{bad}");
    }
    // Unknown fields (a command, an executable) do not even parse.
    for bad in [
        json!({ "scenario": "registry-concurrency", "command": "sh -c id" }),
        json!({ "scenario": "registry-concurrency", "runner": "/bin/sh" }),
    ] {
        assert!(ReliabilitySpec::from_json(bad).is_err());
    }
    assert!(lab.list(10, None).unwrap().is_empty());
    assert!(!launcher.busy());
}

#[test]
fn a_launched_scenario_runs_in_the_runner_process_and_completes() {
    let env = Env::new();
    let lab = Arc::new(env.lab());
    let launcher = Launcher::new(Path::new(RUNNER), Duration::from_secs(300)).unwrap();
    let report = run(&launcher, &lab, &small());
    assert_eq!(report.status, Status::Completed, "{report:?}");
    assert!(!report.timed_out && report.finalized.is_none());
    let record = lab.experiment(report.id).unwrap();
    assert_eq!(record.kind, "reliability");
    assert_eq!(record.outcome.as_deref(), Some("invariants_preserved"));
    assert_eq!(record.result.as_ref().unwrap()["summary"]["cases"], 6);
    assert!(
        !env.lab_dir
            .join("reliability-work")
            .join(report.id.to_string())
            .exists()
    );
    assert!(!launcher.busy());

    // A later launch that fails leaves the completed result untouched.
    let broken = script(env.dir.path(), "exits.sh", "exit 3");
    let failing = Launcher::new(&broken, Duration::from_secs(60)).unwrap();
    let failed = run(&failing, &lab, &small());
    assert_eq!(failed.status, Status::Failed);
    let after = lab.experiment(report.id).unwrap();
    assert_eq!(after, record);
}

#[test]
fn a_runner_that_never_starts_is_an_infrastructure_failure() {
    let env = Env::new();
    let lab = Arc::new(env.lab());
    let runner = script(env.dir.path(), "exits.sh", "exit 3");
    let launcher = Launcher::new(&runner, Duration::from_secs(60)).unwrap();
    let report = run(&launcher, &lab, &small());
    assert_eq!(report.finalized, Some("failed_before_start"));
    let record = lab.experiment(report.id).unwrap();
    assert_eq!(record.status, Status::Failed);
    let failure = record.failure.unwrap();
    assert_eq!(failure.kind, FailureKind::Infrastructure);
    assert!(failure.message.contains("exited"), "{}", failure.message);
    assert!(record.result.is_none());
}

#[test]
fn a_hung_runner_is_stopped_with_its_children_at_the_deadline() {
    let env = Env::new();
    let lab = Arc::new(env.lab());
    let pid_file = env.dir.path().join("child.pid");
    let runner = script(
        env.dir.path(),
        "hangs.sh",
        &format!("sleep 600 &\necho $! > '{}'\nwait", pid_file.display()),
    );
    let launcher = Launcher::new(&runner, Duration::from_secs(1)).unwrap();
    let report = run(&launcher, &lab, &small());
    assert!(report.timed_out);
    assert_eq!(report.finalized, Some("failed_before_start"));
    let failure = lab.experiment(report.id).unwrap().failure.unwrap();
    assert_eq!(failure.kind, FailureKind::Infrastructure);
    assert!(
        failure.message.contains("did not finish within 1 s"),
        "{}",
        failure.message
    );
    // The runner's own child process was killed with it (process group).
    let pid = std::fs::read_to_string(&pid_file).unwrap();
    let alive = std::process::Command::new("kill")
        .args(["-0", pid.trim()])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(!alive, "the scenario's child process was left running");
    assert!(!launcher.busy());
}

#[test]
fn concurrent_launches_admit_exactly_one() {
    let env = Env::new();
    let lab = Arc::new(env.lab());
    let runner = script(env.dir.path(), "slow.sh", "sleep 2");
    let launcher = Arc::new(Launcher::new(&runner, Duration::from_secs(60)).unwrap());
    let (tx, rx) = mpsc::channel();
    let handles: Vec<_> = (0..6)
        .map(|_| {
            let (launcher, lab, tx) = (Arc::clone(&launcher), Arc::clone(&lab), tx.clone());
            std::thread::spawn(move || {
                launcher
                    .launch(lab, &small(), move |r| {
                        let _ = tx.send(r);
                    })
                    .map(|r| r.id)
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let admitted: Vec<_> = results.iter().filter_map(|r| r.as_ref().ok()).collect();
    assert_eq!(admitted.len(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(LaunchError::Busy)))
            .count(),
        5
    );
    let report = rx.recv_timeout(Duration::from_secs(60)).unwrap();
    assert_eq!(&report.id, admitted[0]);
    // Exactly one experiment was recorded, and it has a terminal status.
    let all = lab.list(10, None).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].status, Status::Failed);
}
