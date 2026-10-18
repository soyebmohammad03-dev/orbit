//! Launching reliability runs through the API. The production API has no
//! fault hooks; it can only start a separately configured runner process.
//! These tests use small shell scripts as runners (the real
//! `orbit-reliability` tool is exercised through the API by the Playwright
//! suite and directly by `orbit-lab`'s `tests/launcher.rs`).
#![cfg(unix)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::http::StatusCode;
use common::{TestApi, assert_error};
use orbit_api::{AppState, router};
use orbit_core::{Durability, EventStore, StoreConfig};
use orbit_lab::reliability::launcher::Launcher;
use serde_json::{Value, json};

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn with_runner(body: &str) -> TestApi {
    let dir = tempfile::tempdir().unwrap();
    let runner = script(dir.path(), "runner.sh", body);
    let store = EventStore::open(
        StoreConfig::new(dir.path().join("orbit.db")).with_durability(Durability::Normal),
    )
    .unwrap();
    let lab = orbit_lab::Lab::open(&dir.path().join("lab"), orbit_lab::Catalog::builtin()).unwrap();
    let state = AppState::new(store, None)
        .with_lab(lab)
        .with_reliability_launcher(Launcher::new(&runner, Duration::from_secs(30)).unwrap());
    TestApi {
        app: router(state, &common::origins()),
        dir,
    }
}

fn run(scenario: &str) -> Value {
    json!({ "scenario": scenario, "parameters": { "events": 2 } })
}

async fn finished(api: &TestApi, id: &str) -> Value {
    for _ in 0..3_000 {
        let (_, body) = api.get(&format!("/api/v1/lab/experiments/{id}")).await;
        let s = body["experiment"]["status"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if s == "completed" || s == "failed" {
            return body["experiment"].clone();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("experiment {id} did not finish");
}

#[tokio::test]
async fn launching_is_disabled_by_default() {
    let api = TestApi::with_lab();
    let (status, catalog) = api.get("/api/v1/lab/reliability/scenarios").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(catalog["runner"]["enabled"], false);
    assert!(
        catalog["runner"]["reason"]
            .as_str()
            .unwrap()
            .contains("ORBIT_RELIABILITY_RUNNER")
    );
    let (status, body) = api
        .post(
            "/api/v1/lab/reliability/runs",
            run("snapshot-write-interruption"),
        )
        .await;
    assert_error(
        status,
        &body,
        StatusCode::SERVICE_UNAVAILABLE,
        "reliability_runner_disabled",
    );
    let (_, list) = api.get("/api/v1/lab/experiments").await;
    assert!(list["experiments"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn only_catalog_scenarios_with_bounded_parameters_are_accepted() {
    let api = with_runner("exit 3");
    for bad in [
        json!({ "scenario": "sh -c 'rm -rf /'" }),
        json!({ "scenario": "../../bin/sh" }),
        json!({ "scenario": "snapshot-write-interruption", "parameters": { "events": 100_000 } }),
        json!({ "scenario": "snapshot-write-interruption", "parameters": { "events": -1 } }),
        json!({ "scenario": "snapshot-write-interruption", "parameters": { "--lab": 1 } }),
        json!({ "scenario": "snapshot-write-interruption", "command": "id" }),
        json!({ "scenario": "snapshot-write-interruption", "runner": "/bin/sh" }),
        json!({ "scenario": "snapshot-write-interruption", "args": ["--lab", "/"] }),
        json!("snapshot-write-interruption"),
    ] {
        let (status, body) = api.post("/api/v1/lab/reliability/runs", bad.clone()).await;
        assert_error(status, &body, StatusCode::BAD_REQUEST, "invalid_spec");
    }
    let (_, list) = api.get("/api/v1/lab/experiments").await;
    assert!(
        list["experiments"].as_array().unwrap().is_empty(),
        "nothing was recorded"
    );
}

#[tokio::test]
async fn a_runner_failure_is_an_infrastructure_failure_not_a_result() {
    let api = with_runner("exit 3");
    let (status, created) = api
        .post(
            "/api/v1/lab/reliability/runs",
            run("snapshot-write-interruption"),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{created}");
    assert_eq!(created["status"], "created");
    assert_eq!(created["kind"], "reliability");
    assert_eq!(created["spec"]["scenario_version"], 1);
    let done = finished(&api, created["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "failed");
    assert_eq!(done["failure"]["kind"], "infrastructure");
    assert!(done["result"].is_null());
}

#[tokio::test]
async fn one_run_at_a_time() {
    let api = with_runner("sleep 1");
    let (status, first) = api
        .post(
            "/api/v1/lab/reliability/runs",
            run("snapshot-write-interruption"),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (_, catalog) = api.get("/api/v1/lab/reliability/scenarios").await;
    assert_eq!(catalog["runner"]["busy"], true);
    let (status, body) = api
        .post(
            "/api/v1/lab/reliability/runs",
            run("event-append-interruption"),
        )
        .await;
    assert_error(
        status,
        &body,
        StatusCode::CONFLICT,
        "reliability_run_in_progress",
    );
    finished(&api, first["id"].as_str().unwrap()).await;
    let (_, list) = api.get("/api/v1/lab/experiments").await;
    assert_eq!(list["experiments"].as_array().unwrap().len(), 1);
}
