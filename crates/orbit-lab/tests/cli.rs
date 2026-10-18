//! The `orbit-lab` binary, end to end.

mod common;

use std::process::Command;

use common::*;
use serde_json::Value;

fn cli(env: &Env, args: &[&str]) -> (bool, Value, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_orbit-lab"))
        .arg("--db")
        .arg(&env.db)
        .arg("--lab")
        .arg(&env.lab_dir)
        .args(args)
        .output()
        .expect("run orbit-lab");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let json = serde_json::from_str(&stdout).unwrap_or(Value::Null);
    (output.status.success(), json, stderr)
}

#[test]
fn run_show_verify_and_reproduce_from_the_command_line() {
    let env = Env::new();
    {
        let store = env.store();
        seed(&store, "c", vec![inc(2), dec(5), inc(1)]);
    }
    let (ok, implementations, _) = cli(&env, &["implementations"]);
    assert!(ok);
    assert_eq!(implementations.as_array().unwrap().len(), 5);

    let (ok, run, err) = cli(
        &env,
        &[
            "run",
            "--stream",
            "c",
            "--left",
            "orbit.counter",
            "--right",
            "example.counter-floor-at-zero",
            "--reference",
            "left",
        ],
    );
    assert!(ok, "{err}");
    assert_eq!(run["status"], "completed");
    assert_eq!(run["outcome"], "diverged");
    assert_eq!(run["result"]["first_divergence"]["boundary"], 2);
    let id = run["id"].as_str().unwrap().to_owned();

    let (ok, shown, _) = cli(&env, &["show", &id]);
    assert!(ok);
    assert_eq!(shown["experiment"]["id"], id.as_str());
    let (ok, verified, _) = cli(&env, &["verify", &id]);
    assert!(ok);
    assert_eq!(verified["status"], "intact");
    let (ok, reproduced, _) = cli(&env, &["reproduce", &id]);
    assert!(ok);
    assert_eq!(reproduced["status"], "reproduced");
    let (ok, listed, _) = cli(&env, &["list"]);
    assert!(ok);
    assert_eq!(listed[0]["id"], id.as_str());

    let spec_file = env.dir.path().join("spec.json");
    std::fs::write(
        &spec_file,
        r#"{"stream":"c","left":"orbit.counter","right":"orbit.counter"}"#,
    )
    .unwrap();
    let (ok, from_file, _) = cli(&env, &["run", "--spec", spec_file.to_str().unwrap()]);
    assert!(ok);
    assert_eq!(from_file["outcome"], "equivalent");
}

#[test]
fn errors_exit_non_zero_with_a_message() {
    let env = Env::new();
    let (ok, _, err) = cli(
        &env,
        &[
            "run",
            "--stream",
            "c",
            "--left",
            "orbit.counter",
            "--right",
            "orbit.counter",
        ],
    );
    assert!(!ok);
    assert!(err.contains("no event store"), "{err}");
    assert!(!env.db.exists(), "the lab never creates an event store");
    let (ok, _, err) = cli(&env, &["show", "not-a-uuid"]);
    assert!(!ok);
    assert!(err.contains("not a UUID"), "{err}");
    let (ok, _, err) = cli(&env, &["frobnicate"]);
    assert!(!ok);
    assert!(err.contains("unknown command"), "{err}");
}

#[test]
fn measure_compare_and_reproduce_performance_from_the_command_line() {
    let env = Env::new();
    let (ok, workloads, _) = cli(&env, &["workloads"]);
    assert!(ok);
    assert_eq!(workloads.as_array().unwrap().len(), 7);
    let measure = |seed: &str| {
        let (ok, record, err) = cli(
            &env,
            &[
                "measure",
                "integrity-verification",
                "--input-size",
                "20",
                "--warmup",
                "0",
                "--iterations",
                "3",
                "--seed",
                seed,
            ],
        );
        assert!(ok, "{err}");
        assert_eq!(record["status"], "completed");
        record["id"].as_str().unwrap().to_owned()
    };
    let (a, b, other) = (measure("1"), measure("1"), measure("9"));
    let (ok, cmp, err) = cli(&env, &["compare", "--baseline", &a, &b, &other]);
    assert!(ok, "{err}");
    assert_eq!(cmp["status"], "compared");
    assert_ne!(
        cmp["comparisons"][0]["comparison"]["verdict"],
        "incompatible"
    );
    assert_eq!(
        cmp["comparisons"][1]["comparison"]["verdict"],
        "incompatible"
    );
    let (ok, _, err) = cli(&env, &["compare", &a, "--threshold", "0"]);
    assert!(!ok && err.contains("threshold_percent"), "{err}");
    let (ok, repro, err) = cli(&env, &["reproduce", &a]);
    assert!(ok, "{err}");
    assert_eq!(repro["verification"]["kind"], "reproduction");
    assert_eq!(repro["attempt"]["status"], "completed");
    let (ok, _, err) = cli(&env, &["measure", "event-append", "--input-size", "0"]);
    assert!(!ok && err.contains("input_size"), "{err}");
}

#[test]
fn export_validate_import_and_reproduce_a_bundle_from_the_command_line() {
    let env = Env::new();
    {
        let store = env.store();
        seed(&store, "c", vec![inc(2), dec(5), inc(1)]);
    }
    let (ok, run, err) = cli(
        &env,
        &[
            "run",
            "--stream",
            "c",
            "--left",
            "orbit.counter",
            "--right",
            "orbit.counter",
        ],
    );
    assert!(ok, "{err}");
    let id = run["id"].as_str().unwrap().to_owned();
    let file = env.dir.path().join("bundle.json");
    let file = file.to_str().unwrap();
    let (ok, exported, err) = cli(&env, &["export", &id, "--out", file]);
    assert!(ok, "{err}");
    assert_eq!(exported["history_included"], true);
    let (ok, report, _) = cli(&env, &["validate", file]);
    assert!(ok);
    assert_eq!(report["valid"], true);
    assert_eq!(report["bundle_digest"], exported["bundle_digest"]);
    let (ok, imported, err) = cli(&env, &["import", file]);
    assert!(ok, "{err}");
    assert_eq!(imported["newly_imported"], true);
    let (ok, list, _) = cli(&env, &["bundles"]);
    assert!(ok);
    assert_eq!(list.as_array().unwrap().len(), 1);
    let digest = exported["bundle_digest"].as_str().unwrap();
    let (ok, attempt, err) = cli(&env, &["reproduce-bundle", digest]);
    assert!(ok, "{err}");
    assert_eq!(attempt["status"], "reproduced");
    // A truncated copy is refused with its report.
    let bytes = std::fs::read(file).unwrap();
    std::fs::write(file, &bytes[..bytes.len() / 3]).unwrap();
    let (ok, report, _) = cli(&env, &["validate", file]);
    assert!(ok);
    assert_eq!(report["errors"][0]["code"], "truncated");
    let (ok, _, err) = cli(&env, &["import", file]);
    assert!(!ok && err.contains("nothing was imported"), "{err}");
}
