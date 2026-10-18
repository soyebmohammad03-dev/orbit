//! The lab endpoints over a real event store and a real, separate registry.

mod common;

use axum::http::StatusCode;
use common::{TestApi, assert_error};
use serde_json::{Value, json};

async fn seed_counter(api: &TestApi, stream: &str, steps: &[(&str, u64)]) {
    api.create_stream(stream).await;
    for (kind, by) in steps {
        api.append(stream, &format!("counter.{kind}"), json!({ "by": by }))
            .await;
    }
}

fn differential(stream: &str, left: &str, right: &str) -> Value {
    json!({ "kind": "differential_replay", "spec": { "stream": stream, "left": left, "right": right } })
}

#[tokio::test]
async fn implementations_are_listed_with_distinct_identities() {
    let api = TestApi::with_lab();
    let (status, body) = api.get("/api/v1/lab/implementations").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let all = body["implementations"].as_array().unwrap();
    let find = |id: &str| all.iter().find(|i| i["id"] == id).unwrap().clone();
    let (core, variant) = (find("orbit.counter"), find("example.counter-floor-at-zero"));
    assert_eq!(core["projection"], variant["projection"]);
    assert_eq!(core["name"], variant["name"]);
    assert_ne!(core["fingerprint"], variant["fingerprint"]);
}

#[tokio::test]
async fn create_inspect_verify_and_reproduce_a_differential_replay() {
    let mut api = TestApi::with_lab();
    seed_counter(
        &api,
        "c",
        &[("incremented", 2), ("decremented", 5), ("incremented", 1)],
    )
    .await;
    let (status, created) = api
        .post(
            "/api/v1/lab/experiments",
            differential("c", "orbit.counter", "example.counter-floor-at-zero"),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["status"], "completed");
    assert_eq!(created["outcome"], "diverged");
    assert_eq!(created["reproducibility"], "reproducible");
    assert_eq!(created["result"]["first_divergence"]["boundary"], 2);
    assert_eq!(
        created["result"]["first_divergence"]["changes"],
        json!([{ "path": "/value", "op": "modified", "from": -3, "to": 0 }])
    );
    let id = created["id"].as_str().unwrap().to_owned();

    // Persisted across a restart of the server.
    api.restart_with_lab();
    let (status, detail) = api.get(&format!("/api/v1/lab/experiments/{id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["experiment"], created);
    assert_eq!(detail["verifications"], json!([]));

    let (status, verified) = api
        .post(&format!("/api/v1/lab/experiments/{id}/verify"), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{verified}");
    assert_eq!(verified["report"]["status"], "intact");
    assert_eq!(verified["report"]["artifacts"].as_array().unwrap().len(), 4);

    let (status, reproduced) = api
        .post(
            &format!("/api/v1/lab/experiments/{id}/reproduce"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{reproduced}");
    assert_eq!(reproduced["status"], "reproduced");

    let (_, detail) = api.get(&format!("/api/v1/lab/experiments/{id}")).await;
    let kinds: Vec<&str> = detail["verifications"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["artifacts", "reproduction"]);

    // The event store is untouched: still 3 events, and no lab tables in it.
    let (_, stream) = api.get("/api/v1/streams/c").await;
    assert_eq!(stream["head_sequence"], 3);
    let tables: i64 = api
        .raw()
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name = 'experiments'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        tables, 0,
        "experiment storage is separate from the event store"
    );
}

#[tokio::test]
async fn unavailable_history_is_a_failed_experiment_not_an_http_error() {
    let api = TestApi::with_lab();
    let (status, body) = api
        .post(
            "/api/v1/lab/experiments",
            differential("missing", "orbit.counter", "orbit.counter"),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["status"], "failed");
    assert_eq!(body["failure"]["kind"], "history_unavailable");
    assert!(body["result"].is_null());
    let id = body["id"].as_str().unwrap();
    let (status, body) = api
        .post(
            &format!("/api/v1/lab/experiments/{id}/reproduce"),
            json!({}),
        )
        .await;
    assert_error(status, &body, StatusCode::CONFLICT, "invalid_state");
}

#[tokio::test]
async fn invalid_requests_are_rejected_before_anything_is_recorded() {
    let api = TestApi::with_lab();
    let cases = [
        (
            json!({ "kind": "chaos", "spec": {} }),
            "unsupported_experiment_kind",
        ),
        (
            differential("c", "orbit.counter", "nope"),
            "unknown_implementation",
        ),
        (
            differential("bad stream", "orbit.counter", "orbit.counter"),
            "invalid_spec",
        ),
        (
            json!({ "kind": "differential_replay", "spec": { "stream": "c", "left": "orbit.counter", "right": "orbit.counter", "path": "/etc/passwd" } }),
            "invalid_spec",
        ),
        (
            json!({ "kind": "differential_replay", "spec": { "stream": "c", "left": "orbit.counter", "right": "orbit.counter", "from": 5, "to": 1 } }),
            "invalid_spec",
        ),
    ];
    for (body, code) in cases {
        let (status, response) = api.post("/api/v1/lab/experiments", body).await;
        assert_error(status, &response, StatusCode::BAD_REQUEST, code);
    }
    let (status, body) = api
        .post("/api/v1/lab/experiments", json!({ "spec": {} }))
        .await;
    assert!(status.is_client_error(), "{body}");
    let (_, page) = api.get("/api/v1/lab/experiments").await;
    assert_eq!(page["experiments"], json!([]));

    let (status, body) = api.get("/api/v1/lab/experiments/not-a-uuid").await;
    assert_error(
        status,
        &body,
        StatusCode::BAD_REQUEST,
        "invalid_experiment_id",
    );
    let (status, body) = api
        .get("/api/v1/lab/experiments/0192f3a4-0000-7000-8000-000000000000")
        .await;
    assert_error(status, &body, StatusCode::NOT_FOUND, "experiment_not_found");
}

#[tokio::test]
async fn experiments_are_paginated_newest_first() {
    let api = TestApi::with_lab();
    seed_counter(&api, "c", &[("incremented", 1)]).await;
    let mut ids = Vec::new();
    for _ in 0..3 {
        let (_, body) = api
            .post(
                "/api/v1/lab/experiments",
                differential("c", "orbit.counter", "orbit.counter"),
            )
            .await;
        ids.push(body["id"].as_str().unwrap().to_owned());
    }
    let (status, first) = api.get("/api/v1/lab/experiments?limit=2").await;
    assert_eq!(status, StatusCode::OK);
    let got: Vec<&str> = first["experiments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert_eq!(got, [ids[2].as_str(), ids[1].as_str()]);
    assert_eq!(first["experiments"][0]["outcome"], "equivalent");
    let before = first["next_before"].as_str().unwrap();
    let (_, second) = api
        .get(&format!("/api/v1/lab/experiments?limit=2&before={before}"))
        .await;
    assert_eq!(second["experiments"][0]["id"], ids[0].as_str());
    assert!(second["next_before"].is_null());
    let (status, body) = api.get("/api/v1/lab/experiments?limit=0").await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "invalid_parameter");
}

#[tokio::test]
async fn the_lab_can_be_disabled() {
    let api = TestApi::new();
    let (status, body) = api.get("/api/v1/lab/experiments").await;
    assert_error(
        status,
        &body,
        StatusCode::SERVICE_UNAVAILABLE,
        "lab_unavailable",
    );
}

#[tokio::test]
async fn lab_endpoints_keep_the_host_guard_and_leak_no_paths() {
    let api = TestApi::with_lab();
    let request = axum::http::Request::get("/api/v1/lab/experiments")
        .header("host", "evil.example")
        .body(axum::body::Body::empty())
        .unwrap();
    let response = tower::ServiceExt::oneshot(api.app.clone(), request)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    seed_counter(&api, "c", &[("incremented", 1)]).await;
    let (_, body) = api
        .post(
            "/api/v1/lab/experiments",
            differential("c", "orbit.counter", "orbit.counter"),
        )
        .await;
    let text = body.to_string();
    assert!(!text.contains(api.dir.path().to_str().unwrap()), "{text}");
}

#[tokio::test]
async fn the_timeline_trace_and_identity_are_served() {
    let api = TestApi::with_lab();
    seed_counter(
        &api,
        "t",
        &[
            ("incremented", 1),
            ("decremented", 3),
            ("incremented", 5),
            ("decremented", 9),
        ],
    )
    .await;
    let (_, created) = api
        .post(
            "/api/v1/lab/experiments",
            differential("t", "orbit.counter", "example.counter-floor-at-zero"),
        )
        .await;
    let r = &created["result"];
    assert_eq!(r["result_version"], 2);
    assert_eq!(r["semantics"]["version"], 1);
    // b2: -2/0 differ, b3: 3/5 differ, b4: -6/0 differ.
    assert_eq!(r["timeline"]["intervals_total"], 1);
    assert_eq!(r["timeline"]["intervals"][0]["first"], 2);
    assert_eq!(r["timeline"]["intervals"][0]["end"], "open_at_end");
    assert_eq!(r["timeline"]["intervals"][0]["exit"]["boundary"], 4);
    assert_eq!(created["identity"]["completeness"], "executable");
    let id = created["id"].as_str().unwrap();

    let (status, page) = api
        .get(&format!(
            "/api/v1/lab/experiments/{id}/trace?from=1&limit=2"
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let entries: Vec<(u64, bool)> = page["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["boundary"].as_u64().unwrap(),
                e["equal"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(entries, [(1, true), (2, false)]);
    assert_eq!(page["next_from"], 3);
    assert_eq!(page["boundaries_total"], 5);
    let (status, body) = api
        .get(&format!("/api/v1/lab/experiments/{id}/trace?limit=501"))
        .await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "invalid_parameter");

    let (_, failed) = api
        .post(
            "/api/v1/lab/experiments",
            differential("nope", "orbit.counter", "orbit.counter"),
        )
        .await;
    let (status, body) = api
        .get(&format!(
            "/api/v1/lab/experiments/{}/trace",
            failed["id"].as_str().unwrap()
        ))
        .await;
    assert_error(status, &body, StatusCode::CONFLICT, "invalid_state");

    let (status, reproduced) = api
        .post(
            &format!("/api/v1/lab/experiments/{id}/reproduce"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(reproduced["detail"]["compared"], "full_result");
    assert_eq!(reproduced["detail"]["build"]["same_executable"], true);
}

#[tokio::test]
async fn reliability_scenarios_are_listed_but_never_run_by_the_api() {
    let api = TestApi::with_lab();
    let (status, body) = api.get("/api/v1/lab/reliability/scenarios").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let ids: Vec<&str> = body["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "event-append-interruption",
            "snapshot-write-interruption",
            "experiment-interruption",
            "registry-concurrency"
        ]
    );
    assert!(
        body["execution"]
            .as_str()
            .unwrap()
            .contains("orbit-reliability")
    );

    let (status, body) = api
        .post(
            "/api/v1/lab/experiments",
            json!({ "kind": "reliability", "spec": { "scenario": "event-append-interruption" } }),
        )
        .await;
    let error = assert_error(
        status,
        &body,
        StatusCode::BAD_REQUEST,
        "unsupported_experiment_kind",
    );
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("orbit-reliability")
    );
    // No route runs, kills or corrupts anything.
    for path in [
        "/api/v1/lab/reliability/run",
        "/api/v1/lab/reliability/scenarios/event-append-interruption/run",
        "/api/v1/lab/fault",
    ] {
        let (status, body) = api.post(path, json!({})).await;
        assert!(
            status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED,
            "{path}: {status} {body}"
        );
    }
    let (_, page) = api.get("/api/v1/lab/experiments").await;
    assert_eq!(page["experiments"], json!([]), "nothing was recorded");
}

#[tokio::test]
async fn a_timed_out_experiment_request_still_completes() {
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    let store = orbit_core::EventStore::open(
        orbit_core::StoreConfig::new(dir.path().join("orbit.db"))
            .with_durability(orbit_core::Durability::Normal),
    )
    .unwrap();
    let stream = orbit_core::StreamId::new("c").unwrap();
    store
        .create_stream(&stream, orbit_core::Metadata::new())
        .unwrap();
    let lab = orbit_lab::Lab::open(&dir.path().join("lab"), orbit_lab::Catalog::builtin()).unwrap();
    let app = orbit_api::router(
        orbit_api::AppState::new(store, None)
            .with_lab(lab)
            .with_request_timeout(Duration::from_millis(200)),
        &common::origins(),
    );
    let send = |method: &str, uri: &str, body: Option<serde_json::Value>| {
        let mut b = axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "127.0.0.1:7878");
        if body.is_some() {
            b = b.header("content-type", "application/json");
        }
        b.body(axum::body::Body::from(
            body.map(|v| v.to_string()).unwrap_or_default(),
        ))
        .unwrap()
    };
    let call = |request: axum::http::Request<axum::body::Body>| {
        let app = app.clone();
        async move {
            let response = tower::ServiceExt::oneshot(app, request).await.unwrap();
            let status = response.status();
            let bytes = http_body_util::BodyExt::collect(response.into_body())
                .await
                .unwrap()
                .to_bytes();
            (
                status,
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap_or_default(),
            )
        }
    };
    // Hold the registry's write lock so creating the experiment waits.
    let blocker = rusqlite::Connection::open(dir.path().join("lab").join("lab.db")).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let (status, body) = call(send(
        "POST",
        "/api/v1/lab/experiments",
        Some(differential("c", "orbit.counter", "orbit.counter")),
    ))
    .await;
    assert_error(
        status,
        &body,
        StatusCode::SERVICE_UNAVAILABLE,
        "request_timeout",
    );
    blocker.execute_batch("ROLLBACK").unwrap();
    // The request failed, but the work was not rolled back: it finishes.
    let mut seen = None;
    for _ in 0..500 {
        let (_, page) = call(send("GET", "/api/v1/lab/experiments", None)).await;
        if page["experiments"][0]["status"] == "completed" {
            seen = Some(page);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let page = seen.expect("the experiment completed after the lock was released");
    assert_eq!(page["experiments"].as_array().unwrap().len(), 1);
    assert_eq!(page["experiments"][0]["outcome"], "equivalent");
}
