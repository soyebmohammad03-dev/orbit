//! Performance experiments through the lab: every workload runs, results
//! are recorded with their samples and environment, comparisons refuse
//! incompatible configurations, and reproduction creates a linked attempt.
//! No assertion depends on how fast this machine is.

mod common;

use common::Env;
use orbit_lab::Status;
use orbit_lab::performance::{PerformanceSpec, RegressionPolicy, Verdict, WORKLOADS, result_of};
use orbit_lab::registry::FailureKind;
use orbit_lab::reproduction::ReproductionStatus;
use serde_json::json;

fn spec(value: serde_json::Value) -> PerformanceSpec {
    PerformanceSpec::from_json(value).unwrap()
}

#[test]
fn every_workload_runs_and_records_its_samples() {
    let env = Env::new();
    let lab = env.lab();
    for w in WORKLOADS {
        let input = w.input_size.min.max(10).min(w.input_size.max);
        let record = lab
            .measure_performance(&spec(json!({
                "workload": w.id, "input_size": input,
                "warmup_iterations": 1, "measured_iterations": 3,
            })))
            .unwrap();
        assert_eq!(
            record.status,
            Status::Completed,
            "{}: {:?}",
            w.id,
            record.failure
        );
        assert_eq!(record.kind, "performance");
        let result = result_of(&record).unwrap();
        assert_eq!(result.samples_ns.len(), 3, "{}", w.id);
        assert_eq!(result.warmup_samples_ns.len(), 1);
        assert_eq!(result.summary["count"], 3);
        assert_eq!(
            result.evidence_level, "smoke",
            "3 samples are never a measurement"
        );
        assert_eq!(result.configuration.input_size, Some(input));
        assert_eq!(record.outcome.as_deref(), Some("smoke"));
        assert!(result.limitations.len() >= 3);
        // The record's own digests and the work directory.
        let (report, _) = lab.verify_artifacts(record.id).unwrap();
        assert_eq!(report.status, "intact");
        assert!(
            !env.lab_dir
                .join("performance-work")
                .join(record.id.to_string())
                .exists()
        );
    }
}

#[test]
fn results_persist_and_samples_match_the_summary() {
    let env = Env::new();
    let id = {
        let lab = env.lab();
        lab.measure_performance(&spec(json!({
            "workload": "integrity-verification", "input_size": 50,
            "warmup_iterations": 0, "measured_iterations": 12,
        })))
        .unwrap()
        .id
    };
    let lab = env.lab();
    let result = result_of(&lab.experiment(id).unwrap()).unwrap();
    let mut sorted = result.samples_ns.clone();
    sorted.sort_unstable();
    assert_eq!(result.summary["min"].as_f64().unwrap(), sorted[0] as f64);
    assert_eq!(result.summary["max"].as_f64().unwrap(), sorted[11] as f64);
    let median = (sorted[5] + sorted[6]) as f64 / 2.0;
    assert_eq!(result.summary["median"].as_f64().unwrap(), median);
    assert!(
        result.summary["mean_ci"].is_object(),
        "12 samples give a mean interval"
    );
    let expected = if cfg!(debug_assertions) {
        "smoke"
    } else {
        "measurement"
    };
    assert_eq!(result.evidence_level, expected);
}

#[test]
fn invalid_specs_are_rejected_before_anything_is_recorded() {
    let env = Env::new();
    let lab = env.lab();
    for bad in [
        json!({ "workload": "rm -rf" }),
        json!({ "workload": "event-append", "input_size": 1_000_000 }),
        json!({ "workload": "event-append", "measured_iterations": 0 }),
    ] {
        assert!(lab.create_performance(&spec(bad)).is_err());
    }
    assert!(PerformanceSpec::from_json(json!({ "workload": "event-append", "dir": "/" })).is_err());
    assert!(lab.list(10, None).unwrap().is_empty());
}

#[test]
fn infrastructure_problems_fail_the_experiment_without_a_result() {
    let env = Env::new();
    let lab = env.lab();
    // A file where the work directory should go.
    std::fs::write(env.lab_dir.join("performance-work"), b"in the way").unwrap();
    let record = lab
        .measure_performance(&spec(
            json!({ "workload": "event-append", "input_size": 2, "measured_iterations": 1 }),
        ))
        .unwrap();
    assert_eq!(record.status, Status::Failed);
    assert_eq!(record.failure.unwrap().kind, FailureKind::Infrastructure);
    assert!(record.result.is_none());
}

#[test]
fn comparisons_require_compatible_configurations_and_a_baseline() {
    let env = Env::new();
    let lab = env.lab();
    let run = |seed: u64| {
        lab.measure_performance(&spec(json!({
            "workload": "historical-reconstruction", "input_size": 40, "seed": seed,
            "warmup_iterations": 0, "measured_iterations": 4,
        })))
        .unwrap()
        .id
    };
    let first = run(1);
    // No earlier experiment: no baseline, explicitly.
    assert!(lab.find_baseline(first).unwrap().is_none());
    let none = lab
        .compare_performance(None, &[first], &RegressionPolicy::default())
        .unwrap();
    assert_eq!(none["status"], "no_baseline");
    let other_seed = run(2);
    let second = run(1);
    // The baseline of `second` is `first`: same configuration, earlier.
    assert_eq!(lab.find_baseline(second).unwrap().unwrap().id, first);
    let found = lab
        .compare_performance(None, &[second], &RegressionPolicy::default())
        .unwrap();
    assert_eq!(found["status"], "compared");
    assert_eq!(found["baseline"]["experiment_id"], first.to_string());
    assert_eq!(
        found["baseline_selection"],
        "newest_compatible_before_candidate"
    );
    let verdict = &found["comparisons"][0]["comparison"]["verdict"];
    // Machine-dependent, but always one of the defined verdicts other than
    // incompatible; 4 samples are too few for the significance test.
    assert!(
        [
            "within_threshold",
            "suspected_regression",
            "suspected_improvement"
        ]
        .contains(&verdict.as_str().unwrap()),
        "{verdict}"
    );
    assert!(found["comparisons"][0]["comparison"]["test"].is_null());
    // A different seed measures different inputs: incompatible, no numbers.
    let mixed = lab
        .compare_performance(
            Some(first),
            &[other_seed, second],
            &RegressionPolicy::default(),
        )
        .unwrap();
    assert_eq!(
        mixed["comparisons"][0]["comparison"]["verdict"],
        "incompatible"
    );
    assert!(mixed["comparisons"][0]["comparison"]["relative_change"].is_null());
    assert_ne!(
        mixed["comparisons"][1]["comparison"]["verdict"],
        "incompatible"
    );
    // Comparing with something that is not a performance experiment fails.
    let store = env.store();
    common::seed(&store, "c", vec![common::inc(1)]);
    let diff = lab
        .create_and_run(&common::spec("c", "orbit.counter", "orbit.counter"), &store)
        .unwrap();
    assert!(
        lab.compare_performance(Some(diff.id), &[second], &RegressionPolicy::default())
            .is_err()
    );
    let _ = Verdict::Incompatible;
}

#[test]
fn reproduction_is_a_linked_new_measurement() {
    let env = Env::new();
    let lab = env.lab();
    let original = lab
        .measure_performance(&spec(json!({
            "workload": "artifact-persistence", "input_size": 4,
            "warmup_iterations": 1, "measured_iterations": 5,
        })))
        .unwrap();
    let (verification, attempt) = lab
        .reproduce_performance(original.id, &RegressionPolicy::default())
        .unwrap();
    assert_ne!(attempt.id, original.id);
    assert_eq!(attempt.spec, original.spec, "the identical specification");
    assert_eq!(
        verification.detail["attempt_experiment_id"],
        attempt.id.to_string()
    );
    let status = ReproductionStatus::parse(&verification.status).unwrap();
    // With 5 samples there is no significance test, so `differs` is impossible.
    assert!(
        matches!(
            status,
            ReproductionStatus::Reproduced | ReproductionStatus::Inconclusive
        ),
        "{status}"
    );
    // The original is unchanged.
    let again = lab.experiment(original.id).unwrap();
    assert_eq!(again.result_digest, original.result_digest);
    assert_eq!(lab.verifications(original.id).unwrap().len(), 1);
}
