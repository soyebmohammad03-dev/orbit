//! Security and reliability boundaries of the HTTP layer.

mod common;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApi, assert_error};
use http_body_util::BodyExt;
use orbit_api::{AppState, router};
use orbit_core::{Durability, EventStore, StoreConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn call(app: &axum::Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn get(host: Option<&str>) -> Request<Body> {
    let mut builder = Request::get("/api/v1/health");
    if let Some(host) = host {
        builder = builder.header("host", host);
    }
    builder.body(Body::empty()).unwrap()
}

/// DNS rebinding: a malicious page whose own host name resolves to 127.0.0.1
/// is same-origin to the browser, so CORS does not apply. The `Host` header
/// still carries the attacker's name, and must be refused.
#[tokio::test]
async fn only_loopback_host_names_are_served_by_default() {
    let api = TestApi::new();
    for host in [
        "127.0.0.1:7878",
        "localhost:7878",
        "LOCALHOST",
        "[::1]:7878",
        "127.0.0.1",
    ] {
        let (status, body) = call(&api.app, get(Some(host))).await;
        assert_eq!(status, StatusCode::OK, "{host}: {body}");
    }
    for host in [
        "evil.example",
        "evil.example:7878",
        "127.0.0.1.evil.example",
        "0.0.0.0:7878",
        "[::2]:7878",
    ] {
        let (status, body) = call(&api.app, get(Some(host))).await;
        assert_error(status, &body, StatusCode::FORBIDDEN, "host_not_allowed");
    }
    // HTTP/1.1 requires Host; a request without it is refused too.
    let (status, body) = call(&api.app, get(None)).await;
    assert_error(status, &body, StatusCode::FORBIDDEN, "host_not_allowed");
}

#[tokio::test]
async fn extra_host_names_can_be_allowed_explicitly() {
    let dir = tempfile::tempdir().unwrap();
    let store = EventStore::open(
        StoreConfig::new(dir.path().join("orbit.db")).with_durability(Durability::Normal),
    )
    .unwrap();
    let state = AppState::new(store, None).with_allowed_hosts(vec!["orbit.internal".into()]);
    let app = router(state, &common::origins());
    assert_eq!(
        call(&app, get(Some("orbit.internal:7878"))).await.0,
        StatusCode::OK
    );
    // The replacement list is exact: loopback names are not implicitly added here.
    assert_eq!(
        call(&app, get(Some("localhost"))).await.0,
        StatusCode::FORBIDDEN
    );
}

/// A cross-site form can send a "simple" POST (text/plain) without a CORS
/// preflight. Such requests must be rejected before any write.
#[tokio::test]
async fn simple_cross_site_posts_cannot_write() {
    let api = TestApi::new();
    let request = Request::post("/api/v1/streams")
        .header("host", "127.0.0.1:7878")
        .header("content-type", "text/plain")
        .header("origin", "http://evil.example")
        .body(Body::from(r#"{"stream_id":"pwned"}"#))
        .unwrap();
    let (status, body) = call(&api.app, request).await;
    assert_error(
        status,
        &body,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "invalid_body",
    );
    let (status, _) = api.get("/api/v1/streams/pwned").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "nothing was written");
}

#[tokio::test]
async fn cors_preflight_is_narrow() {
    let api = TestApi::new();
    let preflight = |origin: &'static str, method: &'static str| {
        Request::options("/api/v1/streams")
            .header("host", "127.0.0.1:7878")
            .header("origin", origin)
            .header("access-control-request-method", method)
            .header("access-control-request-headers", "content-type")
            .body(Body::empty())
            .unwrap()
    };
    let response = api
        .app
        .clone()
        .oneshot(preflight("http://localhost:3000", "POST"))
        .await
        .unwrap();
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .unwrap(),
        "http://localhost:3000"
    );
    let methods = response
        .headers()
        .get("access-control-allow-methods")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        methods.contains("POST") && methods.contains("GET") && !methods.contains("DELETE"),
        "{methods}"
    );
    let response = api
        .app
        .clone()
        .oneshot(preflight("http://evil.example", "POST"))
        .await
        .unwrap();
    assert!(
        response
            .headers()
            .get("access-control-allow-origin")
            .is_none()
    );
}

/// A request that cannot finish in time gets a structured 503 instead of
/// hanging. Deterministic: the test holds SQLite's write lock, so the append
/// cannot finish until the test releases it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn requests_over_the_deadline_get_a_structured_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("orbit.db");
    let store = EventStore::open(
        StoreConfig::new(&path)
            .with_durability(Durability::Normal)
            .with_busy_timeout(Duration::from_secs(60)),
    )
    .unwrap();
    store
        .create_stream(
            &orbit_core::StreamId::new("s").unwrap(),
            orbit_core::Metadata::new(),
        )
        .unwrap();
    let app = router(
        AppState::new(store, None).with_request_timeout(Duration::from_millis(200)),
        &common::origins(),
    );

    let blocker = rusqlite::Connection::open(&path).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let append = Request::post("/api/v1/streams/s/events")
        .header("host", "127.0.0.1:7878")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "event_type": "a", "payload": 1 }).to_string(),
        ))
        .unwrap();
    let (status, body) = call(&app, append).await;
    let error = assert_error(
        status,
        &body,
        StatusCode::SERVICE_UNAVAILABLE,
        "request_timeout",
    );
    assert_eq!(error["class"], "retryable");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("may still finish")
    );
    blocker.execute_batch("ROLLBACK").unwrap();

    // As documented, the timed-out write was not cancelled: it completes once
    // the lock is released. The next request waits for it (one connection),
    // so this observation is deterministic.
    let (status, stream) = call(
        &app,
        Request::get("/api/v1/streams/s")
            .header("host", "127.0.0.1:7878")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(stream["head_sequence"], 1);
}

#[tokio::test]
async fn errors_never_contain_local_paths() {
    let api = TestApi::new();
    let dir = api.dir.path().to_string_lossy().into_owned();
    api.create_stream("s").await;
    api.append("s", "a", json!(1)).await;
    // Damage a row so reads hit the engine's corruption path.
    let raw = api.raw();
    raw.execute_batch("DROP TRIGGER events_no_update; PRAGMA ignore_check_constraints = ON;")
        .unwrap();
    raw.execute("UPDATE events SET event_id = X'00'", [])
        .unwrap();
    drop(raw);
    let mut bodies = Vec::new();
    for uri in [
        "/api/v1/streams/s/events",
        "/api/v1/streams/s/events/1",
        "/api/v1/streams/s/state",
        "/api/v1/nope",
        "/api/v1/streams/s/events?limit=-1",
    ] {
        let (status, body) = api.get(uri).await;
        assert!(!status.is_success(), "{uri}");
        bodies.push(body.to_string());
    }
    let (_, verify) = api.post("/api/v1/verify", json!({})).await;
    bodies.push(verify.to_string());
    for body in bodies {
        assert!(!body.contains(&dir) && !body.contains("orbit.db"), "{body}");
    }
}
