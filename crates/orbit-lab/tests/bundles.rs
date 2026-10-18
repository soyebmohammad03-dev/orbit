//! Portable evidence bundles: round trips, reproduction from a bundle,
//! tampering, unsafe and malformed input, and the committed fixtures.

mod common;

use std::path::PathBuf;

use common::{Env, dec, inc, seed, spec};
use orbit_core::domain::canonical::to_canonical_string;
use orbit_lab::bundle::{
    self, BUNDLE_TAG, DOCUMENT_TAG, HISTORY_ENTRY, base64_decode, base64_encode, validate,
};
use orbit_lab::digest::{RESULT_TAG, content_digest, value_digest};
use orbit_lab::performance::{PerformanceSpec, RegressionPolicy};
use orbit_lab::reproduction::ReproductionStatus;
use orbit_lab::{Catalog, Lab, LabError, Status};
use serde_json::{Value, json};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bundles")
}

/// A completed differential replay (diverges at boundary 2) and its bundle.
fn differential_bundle(
    env: &Env,
    lab: &Lab,
    with_store: bool,
) -> (orbit_lab::ExperimentRecord, Vec<u8>) {
    let store = env.store();
    seed(&store, "acct", vec![inc(2), dec(5), inc(1), inc(3)]);
    let record = lab
        .create_and_run(
            &spec("acct", "orbit.counter", "example.counter-floor-at-zero"),
            &store,
        )
        .unwrap();
    assert_eq!(record.status, Status::Completed);
    let exported = lab
        .export_bundle(record.id, with_store.then_some(&store))
        .unwrap();
    assert_eq!(exported.history_included, with_store);
    (record, exported.bytes)
}

fn parse(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap()
}

fn entry(bundle: &Value, name: &str) -> Vec<u8> {
    base64_decode(bundle["entries"][name].as_str().unwrap()).unwrap()
}

/// Recomputes every digest after an edit, as a careful forger would.
fn reseal(mut bundle: Value) -> Vec<u8> {
    let names: Vec<String> = bundle["entries"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    for name in names {
        let bytes = entry(&bundle, &name);
        for info in bundle["manifest"]["entries"].as_array_mut().unwrap() {
            if info["name"] == name.as_str() {
                info["size"] = json!(bytes.len());
                info["sha256"] = json!(content_digest(&bytes));
            }
        }
    }
    let result = bundle["document"]["experiment"]["result"].clone();
    if !result.is_null() {
        bundle["document"]["experiment"]["result_digest"] =
            json!(value_digest(RESULT_TAG, &result));
    }
    bundle["manifest"]["document_digest"] = json!(value_digest(DOCUMENT_TAG, &bundle["document"]));
    to_canonical_string(&bundle).into_bytes()
}

fn codes(bytes: &[u8]) -> Vec<&'static str> {
    let (report, valid) = validate(bytes);
    assert!(!report.valid && valid.is_none());
    assert_eq!(report.integrity, "failed");
    report.errors.iter().map(|e| e.code).collect()
}

#[test]
fn a_differential_bundle_round_trips_and_reproduces_elsewhere() {
    let env = Env::new();
    let lab = env.lab();
    let (record, bytes) = differential_bundle(&env, &lab, true);
    // Deterministic: the same record exports to the same bytes.
    assert_eq!(
        lab.export_bundle(record.id, Some(&env.store()))
            .unwrap()
            .bytes,
        bytes
    );

    let (report, valid) = validate(&bytes);
    assert!(report.valid, "{:?}", report.errors);
    assert_eq!(report.integrity, "verified");
    assert_eq!(report.scientific_correctness, "not_verified");
    assert_eq!(report.history, "included_and_verified");
    assert!(report.independently_replayable);
    let valid = valid.unwrap();
    assert_eq!(report.bundle_digest, Some(valid.digest));
    assert_eq!(valid.history.as_ref().unwrap().len(), 4);
    assert_eq!(
        valid.document.experiment.result_digest,
        record.result_digest
    );

    // Another lab, with no access to the original store.
    let elsewhere = Env::new();
    let other = Lab::open(&elsewhere.lab_dir, Catalog::builtin()).unwrap();
    let imported = other.import_bundle(&bytes).unwrap();
    assert!(imported.newly_imported);
    assert_eq!(imported.local_experiment["exists"], false);
    assert_eq!(imported.bundle.kind, "differential_replay");
    assert_eq!(imported.bundle.outcome.as_deref(), Some("diverged"));
    // Importing again changes nothing.
    let again = other.import_bundle(&bytes).unwrap();
    assert!(!again.newly_imported);
    assert_eq!(again.bundle.imported_at, imported.bundle.imported_at);
    assert_eq!(
        other.registry().imported_bundles(10, None).unwrap().len(),
        1
    );
    assert!(
        other.list(10, None).unwrap().is_empty(),
        "no local experiment is created"
    );

    // Exact reproduction from the exported evidence alone.
    let attempt = other
        .reproduce_bundle(valid.digest, &RegressionPolicy::default())
        .unwrap();
    assert_eq!(attempt.status, "reproduced", "{}", attempt.detail);
    assert_eq!(attempt.detail["compared"], "full_result");
    assert_eq!(
        attempt.detail["source"],
        "the bundle's verified input history"
    );
    assert_eq!(
        other
            .registry()
            .bundle_verifications(valid.digest)
            .unwrap()
            .len(),
        1
    );

    // Importing into the lab that produced it leaves the local record alone.
    let home = lab.import_bundle(&bytes).unwrap();
    assert_eq!(
        home.local_experiment,
        json!({ "exists": true, "same_spec_digest": true, "same_result_digest": true })
    );
    assert_eq!(lab.experiment(record.id).unwrap(), record);
}

#[test]
fn a_bundle_without_history_says_so_and_does_not_claim_reproduction() {
    let env = Env::new();
    let lab = env.lab();
    let (_, bytes) = differential_bundle(&env, &lab, false);
    let (report, valid) = validate(&bytes);
    assert!(report.valid);
    assert_eq!(report.history, "not_included");
    assert!(!report.independently_replayable);
    assert!(
        report.replayability.contains("no event store"),
        "{}",
        report.replayability
    );
    let imported = lab.import_bundle(&bytes).unwrap();
    let attempt = lab
        .reproduce_bundle(imported.bundle.bundle_digest, &RegressionPolicy::default())
        .unwrap();
    assert_eq!(
        attempt.status,
        ReproductionStatus::HistoryUnavailable.as_str()
    );
    let _ = valid;
}

#[test]
fn a_changed_stream_is_not_exported_as_the_pinned_history() {
    let env = Env::new();
    let lab = env.lab();
    let store = env.store();
    seed(&store, "acct", vec![inc(2), dec(5)]);
    let record = lab
        .create_and_run(&spec("acct", "orbit.counter", "orbit.counter"), &store)
        .unwrap();
    // Appending is fine (the pinned events 1..=2 are unchanged)...
    store.append(&common::sid("acct"), inc(1)).unwrap();
    let exported = lab.export_bundle(record.id, Some(&store)).unwrap();
    assert!(exported.history_included);
    let bundle = parse(&exported.bytes);
    assert_eq!(bundle["document"]["history"]["events"], 2);
    // ...but a stream that lost its events cannot provide them.
    let other = Env::new();
    let other_lab = other.lab();
    let empty = other.store();
    seed(&empty, "acct", vec![inc(2)]);
    let exported_elsewhere = Lab::open(&env.lab_dir, common::test_catalog())
        .unwrap()
        .export_bundle(record.id, Some(&empty))
        .unwrap();
    assert!(!exported_elsewhere.history_included);
    let _ = other_lab;
}

#[test]
fn tampering_is_detected() {
    let env = Env::new();
    let lab = env.lab();
    let (_, bytes) = differential_bundle(&env, &lab, true);
    let original = parse(&bytes);

    // One flipped byte in an artifact entry.
    let mut b = original.clone();
    let mut trace = entry(&b, "artifacts/left_trace");
    trace[0] ^= 1;
    b["entries"]["artifacts/left_trace"] = json!(base64_encode(&trace));
    assert_eq!(
        codes(&to_canonical_string(&b).into_bytes()),
        ["digest_mismatch"]
    );

    // The result edited without updating anything.
    let mut b = original.clone();
    b["document"]["experiment"]["outcome"] = json!("equivalent");
    assert_eq!(
        codes(&to_canonical_string(&b).into_bytes()),
        ["digest_mismatch"]
    );

    // The result edited and only the document digest updated: the
    // experiment's own result digest no longer matches.
    let mut b = original.clone();
    b["document"]["experiment"]["result"]["outcome"] = json!("equivalent");
    b["manifest"]["document_digest"] = json!(value_digest(DOCUMENT_TAG, &b["document"]));
    assert_eq!(
        codes(&to_canonical_string(&b).into_bytes()),
        ["digest_mismatch"]
    );

    // An input event edited, with every entry digest recomputed: its own
    // event digest no longer matches.
    let mut b = original.clone();
    let mut events: Value = serde_json::from_slice(&entry(&b, HISTORY_ENTRY)).unwrap();
    events[1]["payload"]["by"] = json!(500);
    b["entries"][HISTORY_ENTRY] = json!(base64_encode(to_canonical_string(&events).as_bytes()));
    assert_eq!(codes(&reseal(b)), ["history_mismatch"]);

    // An entry that is not in the manifest, and one that is missing.
    let mut b = original.clone();
    b["entries"]["extra"] = json!(base64_encode(b"x"));
    assert_eq!(
        codes(&to_canonical_string(&b).into_bytes()),
        ["unlisted_entry"]
    );
    let mut b = original.clone();
    b["entries"]
        .as_object_mut()
        .unwrap()
        .remove("artifacts/right_trace");
    assert!(codes(&to_canonical_string(&b).into_bytes()).contains(&"missing_entry"));

    // Unknown fields are not ignored.
    let mut b = original.clone();
    b["manifest"]["signature"] = json!("trust me");
    assert_eq!(
        codes(&to_canonical_string(&b).into_bytes()),
        ["invalid_structure"]
    );
    let mut b = original;
    b["document"]["experiment"]["executed_by"] = json!("someone");
    assert_eq!(codes(&reseal(b)), ["invalid_structure"]);
}

#[test]
fn a_consistent_forgery_passes_integrity_but_not_reproduction() {
    // Integrity is not authenticity: a forger who recomputes every digest
    // produces a valid bundle. Reproduction is what exposes it.
    let env = Env::new();
    let lab = env.lab();
    let (_, bytes) = differential_bundle(&env, &lab, true);
    let mut b = parse(&bytes);
    b["document"]["experiment"]["result"]["outcome"] = json!("equivalent");
    b["document"]["experiment"]["outcome"] = json!("equivalent");
    let forged = reseal(b);
    let (report, _) = validate(&forged);
    assert!(report.valid, "{:?}", report.errors);
    assert_eq!(report.scientific_correctness, "not_verified");
    let imported = lab.import_bundle(&forged).unwrap();
    let attempt = lab
        .reproduce_bundle(imported.bundle.bundle_digest, &RegressionPolicy::default())
        .unwrap();
    assert_eq!(attempt.status, "differs");
    assert!(
        attempt.detail["differences"]
            .as_array()
            .unwrap()
            .contains(&json!("result_document"))
    );
}

#[test]
fn unsafe_names_are_refused_and_nothing_is_imported() {
    let env = Env::new();
    let lab = env.lab();
    let (_, bytes) = differential_bundle(&env, &lab, true);
    let mut b = parse(&bytes);
    let content = b["entries"]["artifacts/left_trace"].clone();
    b["entries"]["../../outside"] = content;
    b["manifest"]["entries"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "name": "../../outside", "media_type": "application/octet-stream",
            "size": 1, "sha256": content_digest(b"x"),
        }));
    let bad = to_canonical_string(&b).into_bytes();
    assert!(codes(&bad).contains(&"unsafe_entry_name"));
    let before = lab.registry().imported_bundles(10, None).unwrap().len();
    match lab.import_bundle(&bad) {
        Err(LabError::InvalidBundle(report)) => assert!(!report.valid),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        lab.registry().imported_bundles(10, None).unwrap().len(),
        before
    );
    assert!(!env.lab_dir.join("..").join("outside").exists());
}

#[test]
fn performance_failed_and_reliability_bundles_report_what_they_can() {
    let env = Env::new();
    let lab = env.lab();
    let perf = lab
        .measure_performance(
            &PerformanceSpec::from_json(json!({
                "workload": "integrity-verification", "input_size": 20,
                "warmup_iterations": 0, "measured_iterations": 4,
            }))
            .unwrap(),
        )
        .unwrap();
    let exported = lab.export_bundle(perf.id, None).unwrap();
    let (report, _) = validate(&exported.bytes);
    assert!(report.valid && !report.independently_replayable);
    assert!(
        report
            .replayability
            .contains("measures the same workload again")
    );
    let bundle = parse(&exported.bytes);
    assert_eq!(
        bundle["document"]["experiment"]["result"]["samples_ns"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    let elsewhere = Env::new();
    let other = elsewhere.lab();
    let imported = other.import_bundle(&exported.bytes).unwrap();
    let attempt = other
        .reproduce_bundle(imported.bundle.bundle_digest, &RegressionPolicy::default())
        .unwrap();
    // Same machine and build, 4 samples (no significance test): never `differs`.
    assert!(
        ["reproduced", "inconclusive"].contains(&attempt.status.as_str()),
        "{}",
        attempt.status
    );
    assert!(attempt.detail["attempt_experiment_id"].is_string());

    // A failed experiment: valid evidence of a failure, nothing to reproduce.
    let failed = lab
        .create_and_run(
            &spec("missing", "orbit.counter", "orbit.counter"),
            &env.store(),
        )
        .unwrap();
    assert_eq!(failed.status, Status::Failed);
    let exported = lab.export_bundle(failed.id, Some(&env.store())).unwrap();
    let imported = other.import_bundle(&exported.bytes).unwrap();
    let attempt = other
        .reproduce_bundle(imported.bundle.bundle_digest, &RegressionPolicy::default())
        .unwrap();
    assert_eq!(attempt.status, "not_reproducible");

    // A reliability result: unsupported here.
    let reliability = lab
        .create_reliability(
            &orbit_lab::reliability::ReliabilitySpec::from_json(json!({
                "scenario": "snapshot-write-interruption", "parameters": { "events": 2 },
            }))
            .unwrap(),
        )
        .unwrap();
    let ran = orbit_lab::reliability::runner::run(
        &lab,
        reliability.id,
        std::path::Path::new(env!("CARGO_BIN_EXE_orbit-reliability")),
    )
    .unwrap();
    let exported = lab.export_bundle(ran.id, None).unwrap();
    let imported = other.import_bundle(&exported.bytes).unwrap();
    let attempt = other
        .reproduce_bundle(imported.bundle.bundle_digest, &RegressionPolicy::default())
        .unwrap();
    assert_eq!(attempt.status, "unsupported");
}

#[test]
fn unfinished_experiments_are_not_exported() {
    let env = Env::new();
    let lab = env.lab();
    let created = lab
        .create(&spec("acct", "orbit.counter", "orbit.counter"))
        .unwrap();
    assert!(matches!(
        lab.export_bundle(created.id, None),
        Err(LabError::InvalidState { .. })
    ));
}

#[test]
fn the_committed_fixtures_validate_as_documented() {
    let read = |name: &str| std::fs::read(fixtures().join(name)).unwrap();
    // The valid fixture: a differential replay with its history, exported
    // by an earlier build. It still validates and reproduces exactly here.
    let valid = read("valid-differential.json");
    let (report, _) = validate(&valid);
    assert!(report.valid, "{:?}", report.errors);
    assert!(report.independently_replayable);
    let env = Env::new();
    let lab = Lab::open(&env.lab_dir, Catalog::builtin()).unwrap();
    let imported = lab.import_bundle(&valid).unwrap();
    let attempt = lab
        .reproduce_bundle(imported.bundle.bundle_digest, &RegressionPolicy::default())
        .unwrap();
    assert_eq!(attempt.status, "reproduced", "{}", attempt.detail);

    for (name, code) in [
        ("truncated.json", "truncated"),
        ("malformed.json", "malformed_json"),
        ("unsupported-version.json", "unsupported_version"),
        ("tampered-artifact.json", "digest_mismatch"),
        ("tampered-history.json", "history_mismatch"),
        ("path-traversal.json", "unsafe_entry_name"),
    ] {
        let (report, _) = validate(&read(name));
        assert!(!report.valid, "{name}");
        assert!(
            report.errors.iter().any(|e| e.code == code),
            "{name}: {:?}",
            report.errors
        );
    }
}

/// Regenerates the fixtures (run with `--ignored` after a format change).
#[test]
#[ignore = "writes tests/fixtures/bundles"]
fn regenerate_fixtures() {
    let env = Env::new();
    let lab = Lab::open(&env.lab_dir, Catalog::builtin()).unwrap();
    let (_, bytes) = differential_bundle(&env, &lab, true);
    let dir = fixtures();
    std::fs::create_dir_all(&dir).unwrap();
    let write = |name: &str, bytes: &[u8]| std::fs::write(dir.join(name), bytes).unwrap();
    write("valid-differential.json", &bytes);
    write("truncated.json", &bytes[..bytes.len() / 2]);
    write(
        "malformed.json",
        b"{\"format\": \"orbit-evidence-bundle\", \"format_version\": 1,,}",
    );
    let original = parse(&bytes);
    let mut b = original.clone();
    b["format_version"] = json!(2);
    write(
        "unsupported-version.json",
        to_canonical_string(&b).as_bytes(),
    );
    let mut b = original.clone();
    let mut trace = entry(&b, "artifacts/left_trace");
    trace[0] ^= 1;
    b["entries"]["artifacts/left_trace"] = json!(base64_encode(&trace));
    write("tampered-artifact.json", to_canonical_string(&b).as_bytes());
    let mut b = original.clone();
    let mut events: Value = serde_json::from_slice(&entry(&b, HISTORY_ENTRY)).unwrap();
    events[0]["payload"]["by"] = json!(99);
    b["entries"][HISTORY_ENTRY] = json!(base64_encode(to_canonical_string(&events).as_bytes()));
    write("tampered-history.json", &reseal(b));
    let mut b = original;
    let content = b["entries"]["artifacts/left_trace"].clone();
    b["entries"]["../escape"] = content;
    b["manifest"]["entries"].as_array_mut().unwrap().push(json!({
        "name": "../escape", "media_type": "application/octet-stream", "size": 1, "sha256": content_digest(b"x"),
    }));
    write("path-traversal.json", to_canonical_string(&b).as_bytes());
    let _ = (BUNDLE_TAG, bundle::FORMAT);
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(512))]

    /// No single-byte change of a valid bundle is accepted under the same
    /// bundle digest: it is either invalid, or (an edit of the manifest's
    /// own unhashed fields, such as the exporter's commit) a different
    /// bundle with a different digest. Validation never panics.
    #[test]
    fn single_byte_changes_never_keep_the_bundle_digest(
        index in 0usize..1_000_000, mask in 1u8..=255,
    ) {
        let original = std::fs::read(fixtures().join("valid-differential.json")).unwrap();
        let digest = validate(&original).0.bundle_digest;
        let mut mutated = original.clone();
        let i = index % mutated.len();
        mutated[i] ^= mask;
        let (report, _) = validate(&mutated);
        if report.valid {
            let a: Value = serde_json::from_slice(&original).unwrap();
            let b: Value = serde_json::from_slice(&mutated).unwrap();
            proptest::prop_assert!(a == b || report.bundle_digest != digest);
        }
    }

    /// Strict base64 round-trips every byte string.
    #[test]
    fn base64_round_trips(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..300)) {
        proptest::prop_assert_eq!(base64_decode(&base64_encode(&bytes)).unwrap(), bytes);
    }
}
