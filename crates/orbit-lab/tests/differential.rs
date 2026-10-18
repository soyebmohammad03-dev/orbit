//! Differential replay behaviour.

mod common;

use common::*;
use orbit_core::replay::{ReplayLab, ReplayStart};
use orbit_core::{Target, projection::examples::Counter};
use orbit_lab::{DifferentialSpec, Status};
use serde_json::{Value, json};

/// `(first, last, end, converged_at)` of every recorded interval.
fn intervals(r: &Value) -> Vec<(u64, u64, &str, Option<u64>)> {
    r["timeline"]["intervals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["first"].as_u64().unwrap(),
                i["last"].as_u64().unwrap(),
                i["end"].as_str().unwrap(),
                i["converged_at"].as_u64(),
            )
        })
        .collect()
}

fn result(record: &orbit_lab::ExperimentRecord) -> &Value {
    assert_eq!(
        record.status,
        Status::Completed,
        "failure: {:?}",
        record.failure
    );
    record
        .result
        .as_ref()
        .expect("completed experiments have a result")
}

#[test]
fn equivalent_implementations_report_equality_at_every_boundary() {
    let env = Env::new();
    let store = env.store();
    seed(&store, "c", vec![inc(1), inc(2), dec(1), inc(5)]);
    let lab = env.lab();
    let record = lab
        .create_and_run(&spec("c", "orbit.counter", "orbit.counter"), &store)
        .unwrap();
    let r = result(&record);
    assert_eq!(record.outcome.as_deref(), Some("equivalent"));
    assert_eq!(r["boundaries_compared"], 5, "boundaries 0..=4");
    assert_eq!(r["boundaries_differing"], 0);
    assert_eq!(r["events_processed"], 4);
    assert!(r["first_divergence"].is_null());
    assert_eq!(r["final_comparison"]["equal"], true);
    assert_eq!(r["reproducibility"]["status"], "reproducible");
    assert_eq!(record.reproducibility.as_deref(), Some("reproducible"));
    let input = record.input.as_ref().unwrap();
    assert_eq!((input.from, input.to), (0, 4));
    assert_eq!(
        input.to_event_digest,
        store
            .event(&sid("c"), orbit_core::Sequence::new(4).unwrap())
            .unwrap()
            .digest
    );
}

#[test]
fn first_divergence_is_located_with_a_structural_diff() {
    let env = Env::new();
    let store = env.store();
    // Values: 2, 3, then core -1 vs floor 0, then 4 vs 5.
    seed(&store, "c", vec![inc(2), inc(1), dec(4), inc(5)]);
    let third = store
        .event(&sid("c"), orbit_core::Sequence::new(3).unwrap())
        .unwrap();
    let record = env
        .lab()
        .create_and_run(
            &spec("c", "orbit.counter", "example.counter-floor-at-zero"),
            &store,
        )
        .unwrap();
    let r = result(&record);
    assert_eq!(record.outcome.as_deref(), Some("diverged"));
    let first = &r["first_divergence"];
    assert_eq!(first["boundary"], 3);
    assert_eq!(first["event"]["sequence"], 3);
    assert_eq!(first["event"]["event_id"], json!(third.event_id));
    assert_eq!(first["event"]["event_type"], "counter.decremented");
    assert_eq!(
        first["boundaries_matched_before"], 3,
        "boundaries 0, 1, 2 matched"
    );
    assert_eq!(first["equal"], false);
    assert_eq!(
        first["changes"],
        json!([{ "path": "/value", "op": "modified", "from": -1, "to": 0 }])
    );
    assert_ne!(first["left"]["digest"], first["right"]["digest"]);
    assert_eq!(
        r["final_comparison"]["changes"],
        json!([{ "path": "/value", "op": "modified", "from": 4, "to": 5 }])
    );
    assert_eq!(intervals(r), [(3, 4, "open_at_end", None)]);
    // Same declared projection key, distinct identities.
    assert_ne!(r["left"]["fingerprint"], r["right"]["fingerprint"]);
}

#[test]
fn reconvergence_and_multiple_segments_are_distinguished() {
    let env = Env::new();
    let store = env.store();
    // b1: 1/1  b2: -1/0  b3: 0/0 (reset)  b4: 2/2  b5: -1/0  b6: 0/0 (reset)
    seed(
        &store,
        "r",
        vec![inc(1), dec(2), reset(), inc(2), dec(3), reset()],
    );
    // Same, but the second dip is never reset.
    seed(
        &store,
        "d",
        vec![inc(1), dec(2), reset(), inc(2), dec(3), inc(1)],
    );
    let lab = env.lab();

    let reconverged = lab
        .create_and_run(
            &spec("r", "orbit.counter", "example.counter-floor-at-zero"),
            &store,
        )
        .unwrap();
    let r = result(&reconverged);
    assert_eq!(reconverged.outcome.as_deref(), Some("reconverged"));
    assert_eq!(r["first_divergence"]["boundary"], 2);
    assert_eq!(
        intervals(r),
        [(2, 2, "converged", Some(3)), (5, 5, "converged", Some(6))]
    );
    assert_eq!(r["timeline"]["intervals_total"], 2);
    assert_eq!(r["timeline"]["convergences"], 2);
    assert_eq!(r["timeline"]["final_equal"], true);
    assert_eq!(r["boundaries_differing"], 2);
    assert_eq!(r["final_comparison"]["equal"], true);

    let diverged = lab
        .create_and_run(
            &spec("d", "orbit.counter", "example.counter-floor-at-zero"),
            &store,
        )
        .unwrap();
    let d = result(&diverged);
    assert_eq!(diverged.outcome.as_deref(), Some("diverged"));
    assert_eq!(
        intervals(d),
        [(2, 2, "converged", Some(3)), (5, 6, "open_at_end", None)]
    );
    assert_eq!(d["timeline"]["convergences"], 1);
}

#[test]
fn empty_and_single_event_histories() {
    let env = Env::new();
    let store = env.store();
    seed(&store, "empty", vec![]);
    seed(&store, "one", vec![inc(7)]);
    let lab = env.lab();

    let empty = lab
        .create_and_run(
            &spec("empty", "orbit.counter", "example.counter-floor-at-zero"),
            &store,
        )
        .unwrap();
    let e = result(&empty);
    assert_eq!(empty.outcome.as_deref(), Some("equivalent"));
    assert_eq!(e["boundaries_compared"], 1, "only the initial states");
    assert_eq!(e["events_processed"], 0);
    assert_eq!(
        e["input"]["to_event_digest"],
        json!(orbit_core::Digest::GENESIS)
    );

    let one = lab
        .create_and_run(&spec("one", "orbit.counter", "orbit.inventory"), &store)
        .unwrap();
    let o = result(&one);
    assert_eq!(one.outcome.as_deref(), Some("implementation_failed"));
    assert_eq!(o["failures"][0]["side"], "right");
    assert_eq!(o["failures"][0]["boundary"], 1);
    assert_eq!(o["failures"][0]["failure"]["kind"], "projection");
    assert_eq!(
        o["failures"][0]["failure"]["error"]["kind"],
        "unsupported_event"
    );
    assert_eq!(o["boundaries_compared"], 1);
    assert!(o["final_comparison"].is_null());
}

#[test]
fn different_initial_states_diverge_at_boundary_zero() {
    let env = Env::new();
    let store = env.store();
    seed(
        &store,
        "w",
        vec![event("word.seen", json!({ "word": "a" }))],
    );
    let record = env
        .lab()
        .create_and_run(&spec("w", "orbit.counter", "test.tree-words"), &store)
        .unwrap();
    let r = result(&record);
    assert_eq!(r["first_divergence"]["boundary"], 0);
    assert!(r["first_divergence"]["event"].is_null());
}

#[test]
fn missing_or_invalid_history_fails_the_experiment_with_a_structured_failure() {
    let env = Env::new();
    let store = env.store();
    seed(&store, "c", vec![inc(1), inc(2)]);
    let lab = env.lab();

    let missing = lab
        .create_and_run(&spec("nope", "orbit.counter", "orbit.counter"), &store)
        .unwrap();
    assert_eq!(missing.status, Status::Failed);
    let failure = missing.failure.as_ref().unwrap();
    assert_eq!(
        serde_json::to_value(failure.kind).unwrap(),
        "history_unavailable"
    );
    assert!(missing.result.is_none() && missing.outcome.is_none());

    let mut beyond = spec("c", "orbit.counter", "orbit.counter");
    beyond.to = Some(5);
    let beyond = lab.create_and_run(&beyond, &store).unwrap();
    assert_eq!(beyond.status, Status::Failed);
    assert_eq!(
        beyond.failure.as_ref().unwrap().details,
        json!({ "required": 5, "head": 2 })
    );

    let mut window = spec("c", "orbit.counter", "orbit.counter");
    window.from = 3;
    let window = lab.create_and_run(&window, &store).unwrap();
    assert_eq!(
        serde_json::to_value(window.failure.unwrap().kind).unwrap(),
        "invalid_range"
    );

    // Rejected before anything is recorded.
    let mut inverted = spec("c", "orbit.counter", "orbit.counter");
    inverted.from = 2;
    inverted.to = Some(1);
    assert!(matches!(
        lab.create(&inverted),
        Err(orbit_lab::LabError::InvalidSpec { field: "from", .. })
    ));
    assert!(matches!(
        lab.create(&spec("c", "orbit.counter", "no.such")),
        Err(orbit_lab::LabError::UnknownImplementation(_))
    ));
    assert!(matches!(
        DifferentialSpec::from_json(
            json!({ "stream": "c", "left": "a", "right": "b", "extra": 1 })
        ),
        Err(orbit_lab::LabError::InvalidSpec { .. })
    ));
    assert!(matches!(
        lab.create(&spec("bad stream", "orbit.counter", "orbit.counter")),
        Err(orbit_lab::LabError::InvalidSpec {
            field: "stream",
            ..
        })
    ));
    assert_eq!(
        lab.list(100, None).unwrap().len(),
        3,
        "invalid specs leave no record"
    );
}

#[test]
fn damaged_history_is_refused_before_any_implementation_runs() {
    let env = Env::new();
    let store = env.store();
    seed(&store, "c", vec![inc(1), inc(2), inc(3)]);
    let raw = env.raw_store();
    raw.execute_batch(
        "DROP TRIGGER events_no_update;
         UPDATE events SET payload = '{\"by\":9}' WHERE stream_id = 'c' AND sequence = 2;",
    )
    .unwrap();
    let record = env
        .lab()
        .create_and_run(&spec("c", "orbit.counter", "orbit.counter"), &store)
        .unwrap();
    assert_eq!(record.status, Status::Failed);
    let failure = record.failure.unwrap();
    assert_eq!(
        serde_json::to_value(failure.kind).unwrap(),
        "integrity_failure"
    );
    assert!(record.artifacts.is_empty());
}

#[test]
fn implementation_errors_and_panics_are_results_not_crashes() {
    let env = Env::new();
    let store = env.store();
    seed(&store, "c", vec![inc(1), inc(2), inc(3), inc(4)]);
    let lab = env.lab();

    let rejected = lab
        .create_and_run(&spec("c", "orbit.counter", "test.rejects-at-3"), &store)
        .unwrap();
    let r = result(&rejected);
    assert_eq!(rejected.outcome.as_deref(), Some("implementation_failed"));
    assert_eq!(r["failures"].as_array().unwrap().len(), 1);
    assert_eq!(r["failures"][0]["side"], "right");
    assert_eq!(r["failures"][0]["implementation"], "test.rejects-at-3");
    assert_eq!(r["failures"][0]["event"]["sequence"], 3);
    assert_eq!(
        r["failures"][0]["failure"]["error"]["kind"],
        "rejected_transition"
    );
    assert_eq!(
        r["boundaries_compared"], 3,
        "boundaries 0..=2 before the failure"
    );
    assert_eq!(r["events_processed"], 3);
    assert_eq!(
        r["reproducibility"]["status"], "reproducible",
        "the failure itself reproduces"
    );

    let both = lab
        .create_and_run(&spec("c", "test.rejects-at-3", "test.rejects-at-3"), &store)
        .unwrap();
    assert_eq!(result(&both)["failures"].as_array().unwrap().len(), 2);

    let panicked = lab
        .create_and_run(&spec("c", "test.panics-at-2", "orbit.counter"), &store)
        .unwrap();
    let p = result(&panicked);
    assert_eq!(p["failures"][0]["side"], "left");
    assert_eq!(p["failures"][0]["failure"]["kind"], "panicked");
    assert_eq!(
        p["failures"][0]["failure"]["message"],
        "deliberate panic at 2"
    );
}

#[test]
fn nondeterminism_is_detected_and_never_reported_reproducible() {
    let env = Env::new();
    let store = env.store();
    seed(&store, "c", vec![inc(1), inc(2)]);
    let lab = env.lab();

    let global = lab
        .create_and_run(&spec("c", "orbit.counter", "test.global-counter"), &store)
        .unwrap();
    let g = result(&global);
    assert_eq!(global.outcome.as_deref(), Some("nondeterministic"));
    assert_eq!(g["determinism"]["consistent"], false);
    assert_eq!(g["determinism"]["inconsistency"]["side"], "right");
    assert_eq!(global.reproducibility.as_deref(), Some("not_reproducible"));

    let declared = lab
        .create_and_run(
            &spec("c", "orbit.counter", "test.counter-uncontrolled"),
            &store,
        )
        .unwrap();
    assert_eq!(declared.outcome.as_deref(), Some("equivalent"));
    assert_eq!(
        declared.reproducibility.as_deref(),
        Some("not_reproducible")
    );
    assert_eq!(
        result(&declared)["determinism"]["declared_uncontrolled"],
        json!(["right"])
    );

    let mut unchecked = spec("c", "orbit.counter", "test.global-counter");
    unchecked.determinism_check = false;
    let unchecked = lab.create_and_run(&unchecked, &store).unwrap();
    assert_eq!(unchecked.reproducibility.as_deref(), Some("unverified"));
    assert_eq!(result(&unchecked)["determinism"]["checked"], false);
    assert!(result(&unchecked)["determinism"]["consistent"].is_null());
}

#[test]
fn states_are_compared_canonically() {
    let env = Env::new();
    let store = env.store();
    let words = ["pear", "apple", "fig", "apple", "kiwi", "banana", "fig"];
    seed(
        &store,
        "w",
        words
            .iter()
            .map(|w| event("word.seen", json!({ "word": w })))
            .collect(),
    );
    let record = env
        .lab()
        .create_and_run(&spec("w", "test.hash-words", "test.tree-words"), &store)
        .unwrap();
    assert_eq!(record.outcome.as_deref(), Some("equivalent"));
}

#[test]
fn structural_diffs_name_paths_and_values() {
    let env = Env::new();
    let store = env.store();
    seed(
        &store,
        "inv",
        vec![
            event(
                "inventory.item_added",
                json!({ "sku": "A", "name": "Apple", "quantity": 10 }),
            ),
            event("inventory.reserved", json!({ "sku": "A", "quantity": 4 })),
            event("inventory.shipped", json!({ "sku": "A", "quantity": 3 })),
        ],
    );
    let mut s = spec(
        "inv",
        "orbit.inventory",
        "example.inventory-untracked-shipments",
    );
    s.reference = Some(orbit_lab::Side::Left);
    let record = env.lab().create_and_run(&s, &store).unwrap();
    let r = result(&record);
    assert_eq!(r["reference"], "left");
    assert_eq!(r["first_divergence"]["boundary"], 3);
    assert_eq!(
        r["first_divergence"]["changes"],
        json!([{ "path": "/shipped_total", "op": "modified", "from": 3, "to": 0 }])
    );
    // Both full states at the divergence are kept as a verified artifact.
    let states: Value = serde_json::from_slice(
        &env.lab()
            .artifact(record.id, "first_divergence_states")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        states["left"]["items"]["A"],
        json!({ "name": "Apple", "on_hand": 6, "reserved": 1 })
    );
    assert_eq!(states["right"]["shipped_total"], 0);
}

#[test]
fn a_window_compares_only_boundaries_from_onwards() {
    let env = Env::new();
    let store = env.store();
    seed(&store, "c", vec![inc(1), dec(5), reset(), inc(1), inc(1)]);
    let lab = env.lab();
    let mut window = spec("c", "orbit.counter", "example.counter-floor-at-zero");
    window.from = 3;
    let record = lab.create_and_run(&window, &store).unwrap();
    let r = result(&record);
    assert_eq!(record.outcome.as_deref(), Some("equivalent"));
    assert_eq!(r["boundaries_compared"], 3, "boundaries 3, 4, 5");
    assert_eq!(
        r["events_processed"], 5,
        "both implementations still fold from event 1"
    );
    assert_eq!(
        record
            .artifacts
            .iter()
            .find(|a| a.role == "left_trace")
            .unwrap()
            .size,
        3 * 32
    );
}

#[test]
fn state_digests_match_the_core_replay_trace() {
    let env = Env::new();
    let store = env.store();
    let id = seed(&store, "c", vec![inc(3), dec(1), reset(), inc(9), dec(2)]);
    let run = ReplayLab::new(&store, Counter)
        .record(&id, ReplayStart::Initial, Target::Latest)
        .unwrap();
    let lab = env.lab();
    let mut s = spec("c", "orbit.counter", "orbit.counter");
    s.from = 1;
    let record = lab.create_and_run(&s, &store).unwrap();
    let trace = lab.artifact(record.id, "left_trace").unwrap();
    let digests: Vec<orbit_core::Digest> = trace
        .chunks(32)
        .map(|c| orbit_core::Digest::from_slice(c).unwrap())
        .collect();
    assert_eq!(
        digests, run.trace,
        "the lab and the core agree on every state digest"
    );
}

#[test]
fn results_reproduce_from_persisted_configuration() {
    let env = Env::new();
    let store = env.store();
    let id = seed(&store, "c", vec![inc(2), dec(5), inc(1)]);
    let lab = env.lab();
    let record = lab
        .create_and_run(
            &spec("c", "orbit.counter", "example.counter-floor-at-zero"),
            &store,
        )
        .unwrap();
    assert_eq!(record.outcome.as_deref(), Some("diverged"));

    let again = lab.reproduce(record.id, &store).unwrap();
    assert_eq!(again.status, "reproduced");
    assert_eq!(
        again.detail["reproduced_result_digest"],
        json!(record.result_digest)
    );

    // `to` was resolved to 3 and is pinned: later events do not matter.
    store.append(&id, inc(100)).unwrap();
    assert_eq!(
        lab.reproduce(record.id, &store).unwrap().status,
        "reproduced"
    );

    // A reopened lab (new process) still reproduces it.
    drop(lab);
    let lab = env.lab();
    assert_eq!(
        lab.reproduce(record.id, &store).unwrap().status,
        "reproduced"
    );

    // Without the implementation, it is not claimed to be reproducible.
    drop(lab);
    let lab = env.lab_with(orbit_lab::Catalog::new());
    let missing = lab.reproduce(record.id, &store).unwrap();
    assert_eq!(missing.status, "implementation_unavailable");

    // Damaged history is reported, not silently accepted.
    env.raw_store()
        .execute_batch(
            "DROP TRIGGER events_no_update;
             UPDATE events SET payload = '{\"by\":4}' WHERE stream_id = 'c' AND sequence = 1;",
        )
        .unwrap();
    let lab = env.lab();
    let damaged = lab.reproduce(record.id, &store).unwrap();
    assert_eq!(damaged.status, "history_unavailable");
    assert_eq!(damaged.detail["failure"]["kind"], "integrity_failure");
    let kinds: Vec<String> = lab
        .verifications(record.id)
        .unwrap()
        .iter()
        .map(|v| v.status.clone())
        .collect();
    assert_eq!(
        kinds,
        [
            "reproduced",
            "reproduced",
            "reproduced",
            "implementation_unavailable",
            "history_unavailable"
        ]
    );
}

#[test]
fn a_missing_stream_is_reported_when_reproducing() {
    let env = Env::new();
    let store = env.store();
    seed(&store, "c", vec![inc(1)]);
    let lab = env.lab();
    let record = lab
        .create_and_run(&spec("c", "orbit.counter", "orbit.counter"), &store)
        .unwrap();
    let other = Env::new();
    let empty_store = other.store();
    let attempt = lab.reproduce(record.id, &empty_store).unwrap();
    assert_eq!(attempt.status, "history_unavailable");
    assert_eq!(attempt.detail["failure"]["kind"], "history_unavailable");
}

#[test]
fn a_changed_history_with_the_same_length_is_detected() {
    let env = Env::new();
    let store = env.store();
    seed(&store, "c", vec![inc(1), inc(2)]);
    let lab = env.lab();
    let record = lab
        .create_and_run(&spec("c", "orbit.counter", "orbit.counter"), &store)
        .unwrap();
    // A different store with a stream of the same name and length.
    let other = Env::new();
    let other_store = other.store();
    seed(&other_store, "c", vec![inc(1), inc(2)]);
    let attempt = lab.reproduce(record.id, &other_store).unwrap();
    assert_eq!(
        attempt.status, "history_changed",
        "event ids and timestamps differ, so digests differ"
    );
}
