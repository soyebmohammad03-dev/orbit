//! Sprint 8: the complete divergence timeline, compared with an independent
//! reference (direct folds and `serde_json::Value` equality), plus nested
//! JSON diagnosis, a large bounded history and trace paging.

mod common;

use common::*;
use orbit_core::projection::MergePatch;
use orbit_core::projection::examples::Counter;
use orbit_lab::variants::CounterFloorAtZero;
use orbit_lab::{ExperimentRecord, Lab, Status};
use serde_json::{Value, json};

fn result(record: &ExperimentRecord) -> &Value {
    assert_eq!(record.status, Status::Completed, "{:?}", record.failure);
    record.result.as_ref().unwrap()
}

fn lab_intervals(r: &Value) -> Vec<(u64, u64, Option<u64>)> {
    r["timeline"]["intervals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["first"].as_u64().unwrap(),
                i["last"].as_u64().unwrap(),
                i["converged_at"].as_u64(),
            )
        })
        .collect()
}

/// Every trace entry, read page by page through `Lab::trace`.
fn full_trace(lab: &Lab, record: &ExperimentRecord, page: usize) -> Vec<(u64, bool)> {
    let mut out = Vec::new();
    let mut from = None;
    loop {
        let p = lab.trace(record.id, from, page).unwrap();
        out.extend(p.entries.iter().map(|e| (e.boundary, e.equal)));
        match p.next_from {
            Some(next) => from = Some(next),
            None => return out,
        }
    }
}

#[test]
fn divergence_at_the_first_and_at_the_final_event() {
    let env = Env::new();
    let store = env.store();
    seed(&store, "first", vec![dec(1), inc(1), inc(2)]);
    seed(&store, "last", vec![inc(1), inc(1), dec(5)]);
    let lab = env.lab();

    let first = lab
        .create_and_run(
            &spec("first", "orbit.counter", "example.counter-floor-at-zero"),
            &store,
        )
        .unwrap();
    let r = result(&first);
    assert_eq!(r["first_divergence"]["boundary"], 1);
    assert_eq!(
        r["first_divergence"]["boundaries_matched_before"], 1,
        "only the initial states matched"
    );
    assert_eq!(
        lab_intervals(r),
        [(1, 3, None)],
        "-1/0, 0/1, 2/3: never equal again"
    );
    assert_eq!(r["timeline"]["intervals"][0]["end"], "open_at_end");

    let last = lab
        .create_and_run(
            &spec("last", "orbit.counter", "example.counter-floor-at-zero"),
            &store,
        )
        .unwrap();
    let r = result(&last);
    assert_eq!(r["first_divergence"]["boundary"], 3);
    assert_eq!(lab_intervals(r), [(3, 3, None)]);
    assert_eq!(r["timeline"]["final_equal"], false);
    let interval = &r["timeline"]["intervals"][0];
    assert_eq!(interval["boundaries"], 1);
    assert!(
        interval["exit"].is_null(),
        "a one-boundary interval has no separate exit"
    );
    assert_eq!(
        interval["entry"],
        r["first_divergence"]
            .as_object()
            .map(|o| {
                let mut o = o.clone();
                o.remove("boundaries_matched_before");
                Value::Object(o)
            })
            .unwrap()
    );
}

#[test]
fn intervals_record_entry_exit_and_convergence_events() {
    let env = Env::new();
    let store = env.store();
    // b: 1  2   3   4  5  6   7
    // L: 1 -2  -4   0  1 -1   0
    // R: 1  0   0   0  1  0   0
    let id = seed(
        &store,
        "c",
        vec![inc(1), dec(3), dec(2), reset(), inc(1), dec(2), reset()],
    );
    let lab = env.lab();
    let record = lab
        .create_and_run(
            &spec("c", "orbit.counter", "example.counter-floor-at-zero"),
            &store,
        )
        .unwrap();
    let r = result(&record);
    assert_eq!(record.outcome.as_deref(), Some("reconverged"));
    assert_eq!(lab_intervals(r), [(2, 3, Some(4)), (6, 6, Some(7))]);
    assert_eq!(r["boundaries_differing"], 3);
    assert_eq!(r["timeline"]["convergences"], 2);
    let one = &r["timeline"]["intervals"][0];
    assert_eq!(one["index"], 1);
    assert_eq!(
        one["entry"]["changes"],
        json!([{ "path": "/value", "op": "modified", "from": -2, "to": 0 }])
    );
    assert_eq!(one["exit"]["boundary"], 3);
    assert_eq!(
        one["exit"]["changes"],
        json!([{ "path": "/value", "op": "modified", "from": -4, "to": 0 }])
    );
    let reset_event = store
        .event(&id, orbit_core::Sequence::new(4).unwrap())
        .unwrap();
    assert_eq!(
        one["convergence_event"]["event_id"],
        json!(reset_event.event_id)
    );
    assert_eq!(one["convergence_event"]["event_type"], "counter.reset");
    assert_eq!(
        one["convergence_event"]["digest"],
        json!(reset_event.digest)
    );
    // Diagnosis separates evidence from what is not established.
    let evidence = r["diagnosis"]["evidence"].as_array().unwrap();
    assert!(
        evidence[0]
            .as_str()
            .unwrap()
            .contains("first differ at boundary 2")
    );
    assert!(
        evidence[0]
            .as_str()
            .unwrap()
            .starts_with("The states were equal at 2 compared boundaries")
    );
    assert!(
        evidence
            .iter()
            .any(|e| e.as_str().unwrap().contains("3 boundaries of 8 differ"))
    );
    assert!(
        evidence[1]
            .as_str()
            .unwrap()
            .starts_with("There, 1 JSON path differs:")
    );
    assert!(
        evidence
            .iter()
            .any(|e| e.as_str().unwrap().contains("2 divergence intervals"))
    );
    let open = r["diagnosis"]["not_established"].as_array().unwrap();
    assert!(
        open.iter()
            .any(|q| q.as_str().unwrap().starts_with("Why the states differ"))
    );
    assert!(
        open.iter()
            .any(|q| q.as_str().unwrap().contains("no reference side"))
    );
    assert_eq!(r["perspective"]["changes_orientation"], "left_to_right");
    assert!(r["perspective"]["expected"].is_null());
}

#[test]
fn the_reference_side_is_expected_and_the_other_observed() {
    let env = Env::new();
    let store = env.store();
    seed(&store, "c", vec![dec(1)]);
    let lab = env.lab();
    let mut s = spec("c", "orbit.counter", "example.counter-floor-at-zero");
    s.reference = Some(orbit_lab::Side::Right);
    let r = result(&lab.create_and_run(&s, &store).unwrap()).clone();
    assert_eq!(
        r["perspective"],
        json!({ "reference": "right", "expected": "right", "observed": "left", "changes_orientation": "left_to_right" })
    );
    let open = r["diagnosis"]["not_established"].as_array().unwrap();
    assert!(open.iter().any(|q| {
        q.as_str()
            .unwrap()
            .contains("right implementation is correct")
    }));
}

#[test]
fn event_context_lists_metadata_keys_but_not_values() {
    let env = Env::new();
    let store = env.store();
    let sid = sid("m");
    store
        .create_stream(&sid, orbit_core::Metadata::new())
        .unwrap();
    let mut meta = serde_json::Map::new();
    meta.insert("tenant".into(), json!("secret-tenant"));
    meta.insert("actor".into(), json!("someone@example.com"));
    store.append(&sid, dec(1).with_metadata(meta)).unwrap();
    let record = env
        .lab()
        .create_and_run(
            &spec("m", "orbit.counter", "example.counter-floor-at-zero"),
            &store,
        )
        .unwrap();
    let event = &result(&record)["first_divergence"]["event"];
    assert_eq!(event["metadata_keys"], json!(["actor", "tenant"]));
    let text = serde_json::to_string(&record).unwrap();
    assert!(!text.contains("secret-tenant") && !text.contains("someone@example.com"));
    assert_eq!(event["schema_version"], 1);
    assert!(event["recorded_at"].as_str().unwrap().ends_with('Z'));
}

#[test]
fn nested_json_nulls_arrays_and_numbers_are_diagnosed() {
    let env = Env::new();
    let store = env.store();
    seed(
        &store,
        "doc",
        vec![
            event(
                "doc.patched",
                json!({ "profile": { "name": "Ada", "nick": "a" }, "tags": [1, 2], "score": 1.5 }),
            ),
            event(
                "doc.patched",
                json!({ "profile": { "nick": null }, "tags": [1, 2, 3] }),
            ),
            event("doc.patched", json!({ "profile": { "nick": "b" } })),
            event("doc.patched", json!({ "extra": null, "score": -0.25 })),
        ],
    );
    let events = store.read_stream(&sid("doc")).unwrap();
    let lab = env.lab_with(sprint8_catalog());
    let record = lab
        .create_and_run(&spec("doc", "orbit.merge-patch", "test.keep-nulls"), &store)
        .unwrap();
    let r = result(&record);
    // Reference: RFC 7386 deletes on null; the variant stores null.
    let left = reference_states(&MergePatch, &events);
    let right = reference_states(&KeepNulls, &events);
    assert_eq!(lab_intervals(r), reference_intervals(&left, &right, 0));
    assert_eq!(lab_intervals(r), [(2, 2, Some(3)), (4, 4, None)]);
    let one = &r["timeline"]["intervals"][0]["entry"];
    assert_eq!(
        one["changes"],
        json!([{ "path": "/profile/nick", "op": "added", "value": null }])
    );
    assert_eq!(
        one["change_summary"],
        json!({ "added": 1, "removed": 0, "modified": 0, "type_changed": 0 })
    );
    let two = &r["timeline"]["intervals"][1]["entry"];
    assert_eq!(
        two["changes"],
        json!([{ "path": "/extra", "op": "added", "value": null }])
    );
    // Arrays and numbers were applied identically by both sides.
    let states: Value =
        serde_json::from_slice(&lab.artifact(record.id, "final_states").unwrap()).unwrap();
    assert_eq!(states["left"]["tags"], json!([1, 2, 3]));
    assert_eq!(states["left"]["score"], json!(-0.25));
    assert_eq!(states["left"], left[4]);
    assert_eq!(states["right"], right[4]);
}

#[test]
fn a_large_bounded_history_is_paged_and_matches_the_reference() {
    let env = Env::new();
    let store = env.store();
    // 3,000 events: every 10th event dips below zero, the next resets.
    let mut requests = Vec::new();
    for n in 0..3_000u64 {
        requests.push(match n % 10 {
            8 => dec(100),
            9 => reset(),
            _ => inc(1),
        });
    }
    seed(&store, "big", requests);
    let events = store.read_stream(&sid("big")).unwrap();
    let lab = env.lab();
    let record = lab
        .create_and_run(
            &spec("big", "orbit.counter", "example.counter-floor-at-zero"),
            &store,
        )
        .unwrap();
    let r = result(&record);
    let left = reference_states(&Counter, &events);
    let right = reference_states(&CounterFloorAtZero, &events);
    let expected = reference_intervals(&left, &right, 0);
    assert_eq!(expected.len(), 300);
    assert_eq!(r["timeline"]["intervals_total"], 300);
    assert_eq!(lab_intervals(r), expected);
    assert_eq!(r["timeline"]["convergences"], 300);
    // Diffs only for the first MAX_DIAGNOSED_INTERVALS intervals.
    let intervals = r["timeline"]["intervals"].as_array().unwrap();
    assert!(intervals[19]["entry"].is_object());
    assert!(intervals[20]["entry"].is_null());
    // The trace covers every boundary, across pages, and agrees with the reference.
    let trace = full_trace(&lab, &record, 700);
    assert_eq!(trace.len(), 3_001);
    for (b, equal) in &trace {
        assert_eq!(
            *equal,
            left[*b as usize] == right[*b as usize],
            "boundary {b}"
        );
    }
    assert_eq!(
        trace.iter().filter(|(_, e)| !e).count() as u64,
        r["boundaries_differing"].as_u64().unwrap()
    );
}

#[test]
fn trace_pages_respect_windows_and_bounds() {
    let env = Env::new();
    let store = env.store();
    seed(&store, "c", vec![inc(1), dec(5), reset(), inc(2)]);
    let lab = env.lab();
    let mut s = spec("c", "orbit.counter", "example.counter-floor-at-zero");
    s.from = 2;
    let record = lab.create_and_run(&s, &store).unwrap();
    let page = lab.trace(record.id, None, 2).unwrap();
    assert_eq!(
        (
            page.first_boundary,
            page.last_boundary,
            page.boundaries_total
        ),
        (2, Some(4), 3)
    );
    assert_eq!(
        page.entries
            .iter()
            .map(|e| (e.boundary, e.equal))
            .collect::<Vec<_>>(),
        [(2, false), (3, true)]
    );
    assert_eq!(page.next_from, Some(4));
    let rest = lab.trace(record.id, Some(4), 2).unwrap();
    assert_eq!(rest.entries.len(), 1);
    assert_eq!(rest.next_from, None);
    assert!(
        lab.trace(record.id, Some(99), 10)
            .unwrap()
            .entries
            .is_empty()
    );
    assert_eq!(
        lab.trace(record.id, Some(0), 1).unwrap().entries[0].boundary,
        2,
        "clamped to the window"
    );
    // Digests match the stored first divergence.
    let first = &record.result.as_ref().unwrap()["first_divergence"];
    assert_eq!(json!(page.entries[0].left), first["left"]["digest"]);
    assert_eq!(json!(page.entries[0].right), first["right"]["digest"]);
    // A failed experiment has no trace.
    let failed = lab
        .create_and_run(&spec("missing", "orbit.counter", "orbit.counter"), &store)
        .unwrap();
    assert!(matches!(
        lab.trace(failed.id, None, 10),
        Err(orbit_lab::LabError::InvalidState { .. })
    ));
}

#[test]
fn a_failure_inside_an_interval_ends_it_as_stopped() {
    let env = Env::new();
    let store = env.store();
    seed(&store, "c", vec![dec(1), dec(1), inc(1), inc(1)]);
    let lab = env.lab();
    // Floor variant diverges at 1; rejects-at-3 fails at event 3.
    let record = lab
        .create_and_run(
            &spec("c", "example.counter-floor-at-zero", "test.rejects-at-3"),
            &store,
        )
        .unwrap();
    let r = result(&record);
    assert_eq!(record.outcome.as_deref(), Some("implementation_failed"));
    assert_eq!(r["timeline"]["intervals"][0]["end"], "stopped_by_failure");
    assert_eq!(r["timeline"]["intervals"][0]["last"], 2);
    assert_eq!(r["timeline"]["last_boundary"], 2);
}

#[test]
fn identities_report_their_completeness() {
    let env = Env::new();
    let store = env.store();
    seed(&store, "c", vec![inc(1)]);
    let record = env
        .lab()
        .create_and_run(
            &spec("c", "orbit.counter", "example.counter-floor-at-zero"),
            &store,
        )
        .unwrap();
    assert!(
        record.build.executable_digest.is_some(),
        "the test binary is readable"
    );
    assert_eq!(
        record.identity.completeness,
        orbit_lab::IdentityCompleteness::Executable
    );
    assert_eq!(record.identity.sides.len(), 2);
    assert_ne!(
        record.identity.sides[0].fingerprint,
        record.identity.sides[1].fingerprint
    );
    assert!(record.identity.gaps.iter().any(|g| g.contains("not code")));
    let v = serde_json::to_value(&record).unwrap();
    assert_eq!(v["identity"]["completeness"], "executable");
}

#[cfg(unix)]
#[test]
fn an_artifact_write_failure_fails_the_experiment_without_a_partial_result() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new();
    let store = env.store();
    seed(&store, "c", vec![inc(1)]);
    let lab = env.lab();
    let dir = env.lab_dir.join("artifacts").join("sha256");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let record = lab
        .create_and_run(&spec("c", "orbit.counter", "orbit.counter"), &store)
        .unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(record.status, Status::Failed);
    assert_eq!(
        record.failure.unwrap().kind,
        orbit_lab::registry::FailureKind::Internal
    );
    assert!(record.result.is_none() && record.artifacts.is_empty());
    assert!(
        !env.lab_dir
            .join("locks")
            .join(format!("{}.lock", record.id))
            .exists()
    );
}
