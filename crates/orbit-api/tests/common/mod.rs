//! Test harness: the real router over a real SQLite file in a temp directory.
#![allow(dead_code, unreachable_pub)]

use std::path::PathBuf;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use orbit_api::{AppState, router};
use orbit_core::{Durability, EventStore, Limits, StoreConfig};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

pub struct TestApi {
    pub dir: TempDir,
    pub app: Router,
}

pub fn origins() -> Vec<String> {
    vec!["http://localhost:3000".to_owned()]
}

impl TestApi {
    pub fn new() -> Self {
        Self::with_limits(Limits::default(), None)
    }

    pub fn with_limits(limits: Limits, benchmarks: Option<PathBuf>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let app = Self::build(&dir, limits, benchmarks);
        Self { dir, app }
    }

    fn build(dir: &TempDir, limits: Limits, benchmarks: Option<PathBuf>) -> Router {
        let config = StoreConfig::new(dir.path().join("orbit.db"))
            .with_durability(Durability::Normal)
            .with_limits(limits);
        router(
            AppState::new(EventStore::open(config).unwrap(), benchmarks),
            &origins(),
        )
    }

    /// Rebuilds the app on the same database file (simulates a restart).
    pub fn restart(&mut self) {
        self.app = Self::build(&self.dir, Limits::default(), None);
    }

    pub fn raw(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.dir.path().join("orbit.db")).unwrap()
    }

    pub async fn send(&self, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let builder = Request::builder().method(method).uri(uri);
        let request = match body {
            Some(body) => builder
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into_owned()))
        };
        (status, value)
    }

    pub async fn get(&self, uri: &str) -> (StatusCode, Value) {
        self.send("GET", uri, None).await
    }

    pub async fn post(&self, uri: &str, body: Value) -> (StatusCode, Value) {
        self.send("POST", uri, Some(body)).await
    }

    pub async fn create_stream(&self, id: &str) {
        let (status, body) = self
            .post("/api/v1/streams", json!({ "stream_id": id }))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }

    pub async fn append(&self, stream: &str, event_type: &str, payload: Value) -> Value {
        let (status, body) = self
            .post(
                &format!("/api/v1/streams/{stream}/events"),
                json!({ "event_type": event_type, "payload": payload }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body
    }
}

/// Asserts the structured error envelope and returns its body.
pub fn assert_error(
    status: StatusCode,
    body: &Value,
    expected_status: StatusCode,
    code: &str,
) -> Value {
    assert_eq!(status, expected_status, "{body}");
    assert_eq!(body["error"]["code"], code, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|m| !m.is_empty()),
        "{body}"
    );
    body["error"].clone()
}
