//! Performance experiments and evidence bundles through the API: background
//! measurement with one job at a time, comparison, linked reproduction,
//! bundle export, validation, import, reproduction, and the size limits of
//! the bundle routes.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::http::StatusCode;
use common::{TestApi, assert_error};
use orbit_api::{AppState, router};
use orbit_core::{Durability, EventStore, StoreConfig};
use serde_json::{Value, json};

fn perf(workload: &str, seed: u64, iterations: u64) -> Value {
    json!({ "kind": "performance", "spec": {
        "workload": workload, "input_size": 20, "seed": seed,
        "warmup_iterations": 0, "measured_iterations": iterations,
    } })
}

/// Polls an experiment until it is finished (bounded: the work is a few
/// milliseconds of measurement; the bound only prevents a hang).
async fn finished(api: &TestApi, id: &str) -> Value {
    for _ in 0..3_000 {
        let (status, body) = api.get(&format!("/api/v1/lab/experiments/{id}")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let s = body["experiment"]["status"].as_str().unwrap().to_owned();
        if s == "completed" || s == "failed" {
            return body;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("experiment {id} did not finish");
}

/// Waits until the server accepts a new measurement.
async fn idle(api: &TestApi) {
    for _ in 0..3_000 {
        let (_, body) = api.get("/api/v1/lab/performance/workloads").await;
        if body["measuring"] == false {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the server stayed busy");
}

async fn start(api: &TestApi, body: Value) -> String {
    idle(api).await;
    let (status, created) = api.post("/api/v1/lab/experiments", body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{created}");
    assert_eq!(created["status"], "created");
    assert_eq!(created["kind"], "performance");
    created["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn performance_experiments_run_in_the_background_and_compare() {
    let api = TestApi::with_lab();
    let (status, catalog) = api.get("/api/v1/lab/performance/workloads").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(catalog["workloads"].as_array().unwrap().len(), 7);
    assert_eq!(catalog["default_policy"]["threshold_percent"], 10.0);

    let first = start(&api, perf("integrity-verification", 1, 4)).await;
    let done = finished(&api, &first).await;
    assert_eq!(done["experiment"]["status"], "completed", "{done}");
    let result = &done["experiment"]["result"];
    assert_eq!(result["samples_ns"].as_array().unwrap().len(), 4);
    assert_eq!(result["evidence_level"], "smoke");

    // No earlier run: no baseline.
    let (status, none) = api
        .post(
            "/api/v1/lab/performance/compare",
            json!({ "candidates": [first] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{none}");
    assert_eq!(none["status"], "no_baseline");

    let other_seed = start(&api, perf("integrity-verification", 2, 4)).await;
    finished(&api, &other_seed).await;
    let second = start(&api, perf("integrity-verification", 1, 4)).await;
    finished(&api, &second).await;
    let (status, cmp) = api
        .post(
            "/api/v1/lab/performance/compare",
            json!({ "candidates": [second], "threshold_percent": 25 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{cmp}");
    assert_eq!(cmp["baseline"]["experiment_id"], first.as_str());
    assert_ne!(
        cmp["comparisons"][0]["comparison"]["verdict"],
        "incompatible"
    );
    assert_eq!(cmp["policy"]["threshold_percent"], 25.0);
    let (_, mixed) = api
        .post(
            "/api/v1/lab/performance/compare",
            json!({ "baseline": first, "candidates": [other_seed] }),
        )
        .await;
    assert_eq!(
        mixed["comparisons"][0]["comparison"]["verdict"],
        "incompatible"
    );

    // Filtering by kind.
    let (_, page) = api.get("/api/v1/lab/experiments?kind=performance").await;
    assert_eq!(page["experiments"].as_array().unwrap().len(), 3);
    let (_, page) = api
        .get("/api/v1/lab/experiments?kind=differential_replay")
        .await;
    assert_eq!(page["experiments"].as_array().unwrap().len(), 0);
    let (status, body) = api.get("/api/v1/lab/experiments?kind=bogus").await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "invalid_parameter");

    // A linked reproduction attempt.
    idle(&api).await;
    let (status, attempt) = api
        .post(
            &format!("/api/v1/lab/experiments/{first}/reproduce"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{attempt}");
    let attempt_id = attempt["id"].as_str().unwrap().to_owned();
    assert_ne!(attempt_id, first);
    finished(&api, &attempt_id).await;
    idle(&api).await;
    let (_, detail) = api.get(&format!("/api/v1/lab/experiments/{first}")).await;
    let v = &detail["verifications"][0];
    assert_eq!(v["kind"], "reproduction");
    assert_eq!(v["detail"]["attempt_experiment_id"], attempt_id.as_str());
    assert!(
        ["reproduced", "inconclusive"].contains(&v["status"].as_str().unwrap()),
        "{v}"
    );
}

#[tokio::test]
async fn invalid_performance_requests_are_refused() {
    let api = TestApi::with_lab();
    for spec in [
        json!({ "workload": "nope" }),
        json!({ "workload": "event-append", "input_size": 0 }),
        json!({ "workload": "event-append", "measured_iterations": 100_000 }),
        json!({ "workload": "event-append", "path": "/tmp" }),
    ] {
        let (status, body) = api
            .post(
                "/api/v1/lab/experiments",
                json!({ "kind": "performance", "spec": spec }),
            )
            .await;
        assert_error(status, &body, StatusCode::BAD_REQUEST, "invalid_spec");
    }
    for body in [
        json!({ "candidates": [] }),
        json!({ "candidates": ["x"] }),
        json!({ "candidates": vec![uuid_like(); 11] }),
        json!({ "candidates": [uuid_like()], "threshold_percent": 0 }),
        json!({ "candidates": [uuid_like()], "alpha": 0.5 }),
    ] {
        let (status, resp) = api
            .post("/api/v1/lab/performance/compare", body.clone())
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body} → {resp}");
    }
    let (status, body) = api
        .post(
            "/api/v1/lab/performance/compare",
            json!({ "candidates": [uuid_like()] }),
        )
        .await;
    assert_error(status, &body, StatusCode::NOT_FOUND, "experiment_not_found");
    let (_, list) = api.get("/api/v1/lab/experiments").await;
    assert!(
        list["experiments"].as_array().unwrap().is_empty(),
        "nothing was recorded"
    );
}

fn uuid_like() -> String {
    "01890000-0000-7000-8000-000000000001".to_owned()
}

#[tokio::test]
async fn one_measurement_runs_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let store = EventStore::open(
        StoreConfig::new(dir.path().join("orbit.db")).with_durability(Durability::Normal),
    )
    .unwrap();
    let lab = orbit_lab::Lab::open(&dir.path().join("lab"), orbit_lab::Catalog::builtin()).unwrap();
    let state = AppState::new(store, None).with_lab(lab);
    let measuring: Arc<AtomicBool> = Arc::clone(&state.measuring);
    let api = TestApi {
        app: router(state, &common::origins()),
        dir,
    };
    // As if a measurement were running.
    measuring.store(true, Ordering::SeqCst);
    let (status, body) = api
        .post("/api/v1/lab/experiments", perf("event-append", 1, 1))
        .await;
    assert_error(
        status,
        &body,
        StatusCode::CONFLICT,
        "measurement_in_progress",
    );
    let (_, list) = api.get("/api/v1/lab/experiments").await;
    assert!(
        list["experiments"].as_array().unwrap().is_empty(),
        "nothing was created"
    );
    measuring.store(false, Ordering::SeqCst);
    let id = start(&api, perf("event-append", 1, 1)).await;
    finished(&api, &id).await;
    idle(&api).await;
    assert!(!measuring.load(Ordering::SeqCst), "released after the job");
}

async fn differential_experiment(api: &TestApi) -> String {
    api.create_stream("acct").await;
    for (kind, by) in [("incremented", 2), ("decremented", 5), ("incremented", 1)] {
        api.append("acct", &format!("counter.{kind}"), json!({ "by": by }))
            .await;
    }
    let (status, created) = api
        .post(
            "/api/v1/lab/experiments",
            json!({ "kind": "differential_replay", "spec": { "stream": "acct", "left": "orbit.counter", "right": "example.counter-floor-at-zero" } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    created["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn bundles_export_validate_import_and_reproduce() {
    let api = TestApi::with_lab();
    let id = differential_experiment(&api).await;
    let (status, headers, bytes) = api
        .send_raw(
            "GET",
            &format!("/api/v1/lab/experiments/{id}/bundle"),
            Vec::new(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "application/json");
    assert!(
        headers["content-disposition"]
            .to_str()
            .unwrap()
            .contains(&format!("orbit-evidence-{id}.json"))
    );
    let digest = headers["x-orbit-bundle-digest"]
        .to_str()
        .unwrap()
        .to_owned();
    let bundle: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(bundle["document"]["history"]["included"], true);
    let local = api.dir.path().display().to_string();
    assert!(
        !String::from_utf8_lossy(&bytes).contains(&local),
        "no local paths in a bundle"
    );

    let (status, _, report) = api
        .send_raw("POST", "/api/v1/lab/bundles/validate", bytes.clone())
        .await;
    assert_eq!(status, StatusCode::OK);
    let report: Value = serde_json::from_slice(&report).unwrap();
    assert_eq!(report["valid"], true, "{report}");
    assert_eq!(report["bundle_digest"], digest.as_str());
    assert_eq!(report["scientific_correctness"], "not_verified");

    // Without the history.
    let (_, _, bare) = api
        .send_raw(
            "GET",
            &format!("/api/v1/lab/experiments/{id}/bundle?history=false"),
            Vec::new(),
        )
        .await;
    let bare: Value = serde_json::from_slice(&bare).unwrap();
    assert_eq!(bare["document"]["history"]["included"], false);

    let (status, _, imported) = api
        .send_raw("POST", "/api/v1/lab/bundles", bytes.clone())
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let imported: Value = serde_json::from_slice(&imported).unwrap();
    assert_eq!(imported["local_experiment"]["exists"], true);
    let (status, _, _) = api
        .send_raw("POST", "/api/v1/lab/bundles", bytes.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "already imported");

    let (status, list) = api.get("/api/v1/lab/bundles?limit=1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["bundles"][0]["bundle_digest"], digest.as_str());
    assert!(list["next_before"].is_null());
    let (status, attempt) = api
        .post(
            &format!("/api/v1/lab/bundles/{digest}/reproduce"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{attempt}");
    assert_eq!(attempt["status"], "reproduced");
    let (_, detail) = api.get(&format!("/api/v1/lab/bundles/{digest}")).await;
    assert_eq!(detail["verifications"].as_array().unwrap().len(), 1);
    assert!(detail["bundle"]["document"]["manifest"]["entries"].is_array());
    assert!(
        detail["bundle"].get("entries").is_none(),
        "entry contents are not returned"
    );

    // A tampered bundle is refused with its report, and nothing is stored.
    let mut tampered = bundle.clone();
    tampered["document"]["experiment"]["outcome"] = json!("equivalent");
    let (status, _, body) = api
        .send_raw(
            "POST",
            "/api/v1/lab/bundles",
            serde_json::to_vec(&tampered).unwrap(),
        )
        .await;
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_error(
        status,
        &body,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_bundle",
    );
    assert_eq!(
        body["error"]["details"]["errors"][0]["code"],
        "digest_mismatch"
    );
    let (status, _, body) = api
        .send_raw(
            "POST",
            "/api/v1/lab/bundles/validate",
            bytes[..bytes.len() / 2].to_vec(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["errors"][0]["code"], "truncated");
    let (_, list) = api.get("/api/v1/lab/bundles").await;
    assert_eq!(list["bundles"].as_array().unwrap().len(), 1);

    // Bad and unknown digests.
    let (status, body) = api.get("/api/v1/lab/bundles/../../etc").await;
    assert!(
        status == StatusCode::BAD_REQUEST || status == StatusCode::NOT_FOUND,
        "{body}"
    );
    let (status, body) = api.get("/api/v1/lab/bundles/xyz").await;
    assert_error(
        status,
        &body,
        StatusCode::BAD_REQUEST,
        "invalid_bundle_digest",
    );
    let (status, body) = api
        .get(&format!("/api/v1/lab/bundles/{}", "0".repeat(64)))
        .await;
    assert_error(status, &body, StatusCode::NOT_FOUND, "bundle_not_found");
}

#[tokio::test]
async fn bundle_routes_have_their_own_size_limit() {
    let api = TestApi::with_lab();
    // Larger than the general body limit, smaller than the bundle limit.
    let big = vec![b' '; 3 * 1024 * 1024];
    let (status, _, body) = api
        .send_raw("POST", "/api/v1/lab/bundles/validate", big.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let report: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(report["valid"], false);
    let (status, _, _) = api.send_raw("POST", "/api/v1/streams", big).await;
    assert_eq!(
        status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "other routes keep the general limit"
    );
    let huge = vec![b' '; orbit_lab::bundle::MAX_BUNDLE_BYTES + 1];
    let (status, _, _) = api
        .send_raw("POST", "/api/v1/lab/bundles/validate", huge.clone())
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    let (status, _, _) = api.send_raw("POST", "/api/v1/lab/bundles", huge).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn unfinished_and_reliability_experiments() {
    let api = TestApi::with_lab();
    let (status, body) = api
        .send(
            "GET",
            &format!("/api/v1/lab/experiments/{}/bundle", uuid_like()),
            None,
        )
        .await;
    assert_error(status, &body, StatusCode::NOT_FOUND, "experiment_not_found");
    let (status, body) = api
        .post(
            "/api/v1/lab/experiments",
            json!({ "kind": "reliability", "spec": { "scenario": "registry-concurrency" } }),
        )
        .await;
    assert_error(
        status,
        &body,
        StatusCode::BAD_REQUEST,
        "unsupported_experiment_kind",
    );
}
