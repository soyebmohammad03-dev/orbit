//! HTTP contract tests for `/api/v1`, against the real engine and SQLite files.

mod common;

use axum::http::StatusCode;
use common::{TestApi, assert_error};
use orbit_core::Limits;
use serde_json::{Value, json};

const V1: &str = "/api/v1";

#[tokio::test]
async fn health_capabilities_and_openapi() {
    let api = TestApi::new();
    let (status, health) = api.get(&format!("{V1}/health")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(health["status"], "ok");
    assert_eq!(health["database"]["reachable"], true);
    assert_eq!(health["database"]["schema_version"], 3);
    assert!(
        !health.to_string().contains("orbit.db"),
        "paths must not be exposed"
    );

    let (_, caps) = api.get(&format!("{V1}/capabilities")).await;
    assert_eq!(caps["default_projection"], "merge-patch");
    let keys: Vec<&str> = caps["projections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, vec!["merge-patch@v1", "counter@v1", "inventory@v1"]);
    assert_eq!(caps["limits"]["max_page_size"], 500);
    assert_eq!(caps["limits"]["max_replay_events"], 100_000);
    assert_eq!(caps["benchmarks_available"], false);

    let (status, doc) = api.get(&format!("{V1}/openapi.json")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(doc["openapi"].as_str().unwrap().starts_with("3."));
    let paths = doc["paths"].as_object().unwrap();
    for path in [
        "/api/v1/health",
        "/api/v1/streams",
        "/api/v1/streams/{stream_id}/events",
        "/api/v1/streams/{stream_id}/state",
        "/api/v1/streams/{stream_id}/diff",
        "/api/v1/streams/{stream_id}/snapshots",
        "/api/v1/replays/{run_id}/executions",
        "/api/v1/verify",
        "/api/v1/benchmarks",
    ] {
        assert!(paths.contains_key(path), "missing {path}");
    }
    assert!(doc["components"]["schemas"]["ErrorResponse"].is_object());

    let (status, body) = api.get(&format!("{V1}/nope")).await;
    assert_error(status, &body, StatusCode::NOT_FOUND, "route_not_found");
}

#[tokio::test]
async fn stream_lifecycle_and_validation() {
    let api = TestApi::new();
    let (status, created) = api
        .post(
            &format!("{V1}/streams"),
            json!({ "stream_id": "orders-1", "metadata": { "tenant": "acme" } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["stream_id"], "orders-1");
    assert_eq!(created["head_sequence"], 0);
    assert_eq!(created["metadata"], json!({ "tenant": "acme" }));

    let (status, body) = api
        .post(&format!("{V1}/streams"), json!({ "stream_id": "orders-1" }))
        .await;
    let error = assert_error(status, &body, StatusCode::CONFLICT, "stream_already_exists");
    assert_eq!(error["class"], "conflict");

    let (status, body) = api
        .post(&format!("{V1}/streams"), json!({ "stream_id": "bad id" }))
        .await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "invalid_stream_id");
    let (status, body) = api
        .post(
            &format!("{V1}/streams"),
            json!({ "stream_id": "x", "colour": "red" }),
        )
        .await;
    assert_error(
        status,
        &body,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_body",
    );
    let (status, body) = api.send("POST", &format!("{V1}/streams"), None).await;
    assert!(status.is_client_error(), "{status} {body}");

    let (status, body) = api.get(&format!("{V1}/streams/ghost")).await;
    assert_error(status, &body, StatusCode::NOT_FOUND, "stream_not_found");

    for id in ["b", "a", "c"] {
        api.create_stream(id).await;
    }
    let (_, page) = api.get(&format!("{V1}/streams?limit=2")).await;
    let ids: Vec<&str> = page["streams"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["stream_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["a", "b"]);
    assert_eq!(page["next_after"], "b");
    let (_, page) = api.get(&format!("{V1}/streams?after=b&limit=2")).await;
    let ids: Vec<&str> = page["streams"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["stream_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["c", "orders-1"]);
    assert_eq!(page["next_after"], Value::Null);
}

#[tokio::test]
async fn append_read_and_paginate_events() {
    let api = TestApi::new();
    api.create_stream("s").await;
    for n in 1..=7 {
        let event = api.append("s", "thing.happened", json!({ "n": n })).await;
        assert_eq!(event["sequence"], n);
        assert_eq!(event["digest"].as_str().unwrap().len(), 64);
    }
    let (status, page) = api.get(&format!("{V1}/streams/s/events?limit=3")).await;
    assert_eq!(status, StatusCode::OK);
    let seqs = |p: &Value| {
        p["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["sequence"].as_u64().unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(seqs(&page), vec![1, 2, 3]);
    assert_eq!(page["next_after"], 3);
    assert_eq!(page["head_sequence"], 7);
    let (_, page) = api
        .get(&format!("{V1}/streams/s/events?after=3&limit=3"))
        .await;
    assert_eq!(seqs(&page), vec![4, 5, 6]);
    let (_, page) = api
        .get(&format!("{V1}/streams/s/events?after=6&limit=3"))
        .await;
    assert_eq!(seqs(&page), vec![7]);
    assert_eq!(page["next_after"], Value::Null);
    let (_, page) = api.get(&format!("{V1}/streams/s/events?after=7")).await;
    assert!(page["events"].as_array().unwrap().is_empty());

    for bad in ["limit=0", "limit=501", "after=-1", "limit=abc"] {
        let (status, body) = api.get(&format!("{V1}/streams/s/events?{bad}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
        assert!(body["error"]["code"].is_string());
    }

    let (status, event) = api.get(&format!("{V1}/streams/s/events/4")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(event["payload"], json!({ "n": 4 }));
    let (status, body) = api.get(&format!("{V1}/streams/s/events/99")).await;
    assert_error(status, &body, StatusCode::NOT_FOUND, "event_not_found");
    let (status, body) = api.get(&format!("{V1}/streams/ghost/events")).await;
    assert_error(status, &body, StatusCode::NOT_FOUND, "stream_not_found");
}

#[tokio::test]
async fn optimistic_concurrency_and_duplicate_ids() {
    let api = TestApi::new();
    api.create_stream("s").await;
    let uri = format!("{V1}/streams/s/events");
    let append = |body: Value| api.post(&uri, body);
    let (status, _) =
        append(json!({ "event_type": "a", "payload": 1, "expected_sequence": 0 })).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, body) =
        append(json!({ "event_type": "a", "payload": 2, "expected_sequence": 0 })).await;
    let error = assert_error(status, &body, StatusCode::CONFLICT, "sequence_conflict");
    assert_eq!(error["details"]["expected_sequence"], 0);
    assert_eq!(error["details"]["actual_sequence"], 1);
    let (status, _) =
        append(json!({ "event_type": "a", "payload": 2, "expected_sequence": 1 })).await;
    assert_eq!(status, StatusCode::CREATED);

    let id = "01920000-0000-7000-8000-000000000042";
    let (status, _) = append(json!({ "event_type": "a", "payload": 3, "event_id": id })).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, body) = append(json!({ "event_type": "a", "payload": 4, "event_id": id })).await;
    assert_error(status, &body, StatusCode::CONFLICT, "duplicate_event_id");

    let (status, body) = append(json!({ "event_type": "bad type", "payload": 1 })).await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "validation_failed");
    let (status, body) =
        append(json!({ "event_type": "a", "payload": 1, "schema_version": 0 })).await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "validation_failed");
    let (status, body) = append(json!({ "event_type": "a" })).await;
    assert_error(
        status,
        &body,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_body",
    );
    let (_, stream) = api.get(&format!("{V1}/streams/s")).await;
    assert_eq!(stream["head_sequence"], 3, "failed appends wrote nothing");
}

#[tokio::test]
async fn payload_and_body_limits() {
    let api = TestApi::with_limits(
        Limits {
            max_payload_bytes: 64,
            max_metadata_bytes: 64,
        },
        None,
    );
    api.create_stream("s").await;
    let (status, body) = api
        .post(
            &format!("{V1}/streams/s/events"),
            json!({ "event_type": "a", "payload": "x".repeat(100) }),
        )
        .await;
    assert_error(
        status,
        &body,
        StatusCode::UNPROCESSABLE_ENTITY,
        "payload_too_large",
    );
    // Beyond the HTTP body limit (payload + metadata + 64 KiB) the request is rejected before parsing.
    let (status, body) = api
        .post(
            &format!("{V1}/streams/s/events"),
            json!({ "event_type": "a", "payload": "x".repeat(200_000) }),
        )
        .await;
    assert_error(
        status,
        &body,
        StatusCode::PAYLOAD_TOO_LARGE,
        "payload_too_large",
    );
}

async fn order_stream(api: &TestApi) {
    api.create_stream("order-7").await;
    api.append(
        "order-7",
        "order.created",
        json!({ "status": "new", "items": { "A": 1 } }),
    )
    .await;
    api.append(
        "order-7",
        "order.item_added",
        json!({ "items": { "B": 2 } }),
    )
    .await;
    api.append(
        "order-7",
        "order.paid",
        json!({ "status": "paid", "total": 30 }),
    )
    .await;
    api.append(
        "order-7",
        "order.item_removed",
        json!({ "items": { "A": null } }),
    )
    .await;
}

#[tokio::test]
async fn reconstruction_and_time_travel() {
    let api = TestApi::new();
    order_stream(&api).await;
    let (status, latest) = api.get(&format!("{V1}/streams/order-7/state")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(latest["projection"], "merge-patch@v1");
    assert_eq!(
        latest["state"],
        json!({ "status": "paid", "items": { "B": 2 }, "total": 30 })
    );
    assert_eq!(latest["report"]["reconstructed"], 4);
    assert_eq!(latest["report"]["start"]["kind"], "initial");

    let (_, at2) = api
        .get(&format!(
            "{V1}/streams/order-7/state?at=2&record_events=true"
        ))
        .await;
    assert_eq!(
        at2["state"],
        json!({ "status": "new", "items": { "A": 1, "B": 2 } })
    );
    assert_eq!(at2["report"]["applied"].as_array().unwrap().len(), 2);
    let (_, at0) = api.get(&format!("{V1}/streams/order-7/state?at=0")).await;
    assert_eq!(at0["state"], json!({}));

    let (status, body) = api.get(&format!("{V1}/streams/order-7/state?at=5")).await;
    let error = assert_error(
        status,
        &body,
        StatusCode::UNPROCESSABLE_ENTITY,
        "target_beyond_head",
    );
    assert_eq!(error["details"]["head"], 4);
    let (status, body) = api
        .get(&format!("{V1}/streams/order-7/state?projection=nope"))
        .await;
    let error = assert_error(status, &body, StatusCode::BAD_REQUEST, "unknown_projection");
    assert!(error["details"]["available"].as_array().unwrap().len() >= 3);
    let (status, body) = api
        .get(&format!("{V1}/streams/order-7/state?snapshots=sometimes"))
        .await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "invalid_query");

    // A domain projection that does not understand these events fails explicitly.
    let (status, body) = api
        .get(&format!("{V1}/streams/order-7/state?projection=counter"))
        .await;
    let error = assert_error(
        status,
        &body,
        StatusCode::UNPROCESSABLE_ENTITY,
        "projection_failed",
    );
    assert_eq!(error["details"]["sequence"], 1);
    assert_eq!(error["details"]["source"]["kind"], "unsupported_event");

    let (status, diff) = api.get(&format!("{V1}/streams/order-7/diff?from=2")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(diff["to"], 4);
    let changes: Vec<(&str, &str)> = diff["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["path"].as_str().unwrap(), c["op"].as_str().unwrap()))
        .collect();
    assert_eq!(
        changes,
        vec![
            ("/items/A", "removed"),
            ("/status", "modified"),
            ("/total", "added")
        ]
    );
    assert_eq!(diff["events"].as_array().unwrap().len(), 2);

    let (_, explain) = api
        .get(&format!("{V1}/streams/order-7/explain?from=0"))
        .await;
    let transitions = explain["transitions"].as_array().unwrap();
    assert_eq!(transitions.len(), 4);
    assert_eq!(transitions[3]["changes"][0]["path"], "/items/A");
    let (status, body) = api
        .get(&format!("{V1}/streams/order-7/explain?from=3&to=1"))
        .await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "invalid_range");
}

#[tokio::test]
async fn counter_projection_through_the_api() {
    let api = TestApi::new();
    api.create_stream("c").await;
    for by in [5, 3] {
        api.append("c", "counter.incremented", json!({ "by": by }))
            .await;
    }
    api.append("c", "counter.decremented", json!({ "by": 2 }))
        .await;
    let (_, state) = api
        .get(&format!("{V1}/streams/c/state?projection=counter@v1"))
        .await;
    assert_eq!(state["state"], json!({ "value": 6 }));
}

#[tokio::test]
async fn explain_interval_is_bounded() {
    let api = TestApi::new();
    api.create_stream("s").await;
    let (status, body) = api
        .get(&format!("{V1}/streams/s/explain?from=0&to=5000"))
        .await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "interval_too_large");
}

#[tokio::test]
async fn snapshots_are_created_validated_and_damage_is_visible() {
    let api = TestApi::new();
    order_stream(&api).await;
    let uri = format!("{V1}/streams/order-7/snapshots");
    let (status, created) = api.post(&uri, json!({ "at": 3 })).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["created"], true);
    let snapshot = &created["snapshot"];
    assert_eq!(snapshot["sequence"], 3);
    assert_eq!(snapshot["projection"], "merge-patch@v1");
    assert_eq!(snapshot["validation"]["status"], "usable");
    assert_eq!(snapshot["validation"]["checked_by"], "projection");
    let (_, event3) = api.get(&format!("{V1}/streams/order-7/events/3")).await;
    assert_eq!(
        snapshot["event_digest"], event3["digest"],
        "bound to the event it was built from"
    );
    let id = snapshot["snapshot_id"].as_str().unwrap().to_owned();

    let (status, again) = api.post(&uri, json!({ "at": 3 })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["created"], false);
    assert_eq!(again["snapshot"]["snapshot_id"], id);

    // Reconstruction now starts from it.
    let (_, latest) = api.get(&format!("{V1}/streams/order-7/state")).await;
    assert_eq!(latest["report"]["start"]["kind"], "snapshot");
    assert_eq!(latest["report"]["events_applied"], 1);

    let (status, one) = api.get(&format!("{V1}/snapshots/{id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        one["state"],
        json!({ "status": "paid", "items": { "A": 1, "B": 2 }, "total": 30 })
    );
    let (status, body) = api.get(&format!("{V1}/snapshots/not-a-uuid")).await;
    assert_error(
        status,
        &body,
        StatusCode::BAD_REQUEST,
        "invalid_snapshot_id",
    );
    let (status, body) = api
        .get(&format!(
            "{V1}/snapshots/01920000-0000-7000-8000-000000000001"
        ))
        .await;
    assert_error(status, &body, StatusCode::NOT_FOUND, "snapshot_not_found");
    let (status, body) = api.post(&uri, json!({ "at": 0 })).await;
    assert_error(
        status,
        &body,
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    );

    // Tamper with the stored state: the listing reports it, reconstruction falls back and warns.
    api.raw()
        .execute_batch("DROP TRIGGER snapshots_no_update; UPDATE snapshots SET state = '{\"status\":\"forged\"}';")
        .unwrap();
    let (_, list) = api.get(&uri).await;
    assert_eq!(list[0]["validation"]["status"], "unusable");
    assert_eq!(
        list[0]["validation"]["problem"]["kind"],
        "integrity_mismatch"
    );
    let (_, latest) = api.get(&format!("{V1}/streams/order-7/state")).await;
    assert_eq!(latest["report"]["start"]["kind"], "initial");
    assert_eq!(
        latest["report"]["snapshot_warnings"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(latest["state"]["status"], "paid");
    let (status, body) = api
        .get(&format!("{V1}/streams/order-7/state?snapshots=strict"))
        .await;
    assert_error(
        status,
        &body,
        StatusCode::UNPROCESSABLE_ENTITY,
        "snapshot_unusable",
    );
}

#[tokio::test]
async fn integrity_success_and_failure() {
    let api = TestApi::new();
    order_stream(&api).await;
    let (status, report) = api
        .post(&format!("{V1}/streams/order-7/verify"), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(report["total_issues"], 0);
    assert_eq!(report["events_checked"], 4);
    let (status, checkpoint) = api.get(&format!("{V1}/streams/order-7/checkpoint")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(checkpoint["sequence"], 4);
    let (_, report) = api
        .post(
            &format!("{V1}/streams/order-7/verify"),
            json!({ "checkpoints": [checkpoint] }),
        )
        .await;
    assert_eq!(report["total_issues"], 0);
    let wrong = json!({ "checkpoints": [{ "sequence": 2, "digest": "0".repeat(64) }] });
    let (_, report) = api
        .post(&format!("{V1}/streams/order-7/verify"), wrong)
        .await;
    assert_eq!(report["issues"][0]["kind"], "checkpoint_mismatch");
    let (status, body) = api
        .post(
            &format!("{V1}/streams/order-7/verify"),
            json!({ "checkpoints": [{ "sequence": 1, "digest": "xyz" }] }),
        )
        .await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "validation_failed");

    let (_, store) = api.post(&format!("{V1}/verify"), json!({})).await;
    assert_eq!(store["valid"], true);
    assert_eq!(store["sqlite_integrity"], json!(["ok"]));

    api.raw()
        .execute_batch("DROP TRIGGER events_no_update; UPDATE events SET payload = '{\"status\":\"refunded\"}' WHERE sequence = 3;")
        .unwrap();
    let (status, report) = api
        .post(&format!("{V1}/streams/order-7/verify"), json!({}))
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "defects are results, not request failures"
    );
    assert_eq!(report["total_issues"], 1);
    assert_eq!(report["issues"][0]["kind"], "digest_mismatch");
    assert_eq!(report["affected_events"][0]["sequence"], 3);
    let (_, store) = api.post(&format!("{V1}/verify"), json!({})).await;
    assert_eq!(store["valid"], false);
    assert_eq!(store["missing_safeguards"], json!(["events_no_update"]));

    let (status, body) = api.get(&format!("{V1}/streams/ghost/checkpoint")).await;
    assert_error(status, &body, StatusCode::NOT_FOUND, "stream_not_found");
    api.create_stream("empty").await;
    let (status, body) = api.get(&format!("{V1}/streams/empty/checkpoint")).await;
    assert_error(status, &body, StatusCode::NOT_FOUND, "event_not_found");
}

#[tokio::test]
async fn replay_runs_executions_and_divergence_evidence() {
    let api = TestApi::new();
    api.create_stream("c").await;
    for by in [1, 2, 3, 4] {
        api.append("c", "counter.incremented", json!({ "by": by }))
            .await;
    }
    let (status, run) = api
        .post(
            &format!("{V1}/streams/c/replays"),
            json!({ "projection": "counter" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{run}");
    assert_eq!(run["to"], 4);
    assert_eq!(run["trace"].as_array().unwrap().len(), 4);
    let run_id = run["id"].as_str().unwrap().to_owned();

    let (status, execution) = api
        .post(&format!("{V1}/replays/{run_id}/executions"), json!({}))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(execution["outcome"]["status"], "exact_match");
    assert_eq!(
        execution["actual_state_digest"],
        run["expected_state_digest"]
    );

    // Executing with another projection is reported, never substituted.
    let (_, other) = api
        .post(
            &format!("{V1}/replays/{run_id}/executions"),
            json!({ "projection": "merge-patch" }),
        )
        .await;
    assert_eq!(other["outcome"]["status"], "unavailable");
    assert_eq!(other["outcome"]["reason"]["kind"], "projection_mismatch");

    let (_, runs) = api.get(&format!("{V1}/streams/c/replays")).await;
    assert_eq!(runs[0]["execution_count"], 2);
    assert_eq!(runs[0]["last_outcome"], "unavailable");
    let (_, inspection) = api.get(&format!("{V1}/replays/{run_id}")).await;
    assert_eq!(inspection["executions"].as_array().unwrap().len(), 2);
    assert_eq!(inspection["run"]["trace"].as_array().unwrap().len(), 4);

    // History damaged after recording: the execution says integrity_failure.
    api.raw()
        .execute_batch("DROP TRIGGER events_no_update; UPDATE events SET payload = '{\"by\":40}' WHERE sequence = 2;")
        .unwrap();
    let (_, damaged) = api
        .post(&format!("{V1}/replays/{run_id}/executions"), json!({}))
        .await;
    assert_eq!(damaged["outcome"]["status"], "integrity_failure");
    // And recording over damaged history is refused.
    let (status, body) = api
        .post(
            &format!("{V1}/streams/c/replays"),
            json!({ "projection": "counter" }),
        )
        .await;
    assert_error(status, &body, StatusCode::CONFLICT, "history_integrity");

    let (status, body) = api.get(&format!("{V1}/replays/nope")).await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "invalid_run_id");
    let (status, body) = api
        .get(&format!(
            "{V1}/replays/01920000-0000-7000-8000-000000000001"
        ))
        .await;
    assert_error(status, &body, StatusCode::NOT_FOUND, "replay_run_not_found");
    let (status, body) = api
        .post(&format!("{V1}/streams/c/replays"), json!({ "start": { "kind": "reconstructed", "at": 9, "snapshots": { "kind": "prefer" } } }))
        .await;
    assert!(status.is_client_error(), "{status} {body}");
}

#[tokio::test]
async fn experiments_report_every_phase() {
    let api = TestApi::new();
    order_stream(&api).await;
    api.post(
        &format!("{V1}/streams/order-7/snapshots"),
        json!({ "at": 2 }),
    )
    .await;
    let (status, report) = api
        .post(
            &format!("{V1}/streams/order-7/experiments"),
            json!({ "snapshot_policy": "prefer" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{report}");
    assert_eq!(report["integrity"]["valid"], true);
    assert_eq!(report["equivalence"]["equivalent"], true);
    assert_eq!(report["equivalence"]["assisted_start"]["kind"], "snapshot");
    assert_eq!(report["replay_outcome"], "exact_match");
    assert!(report["timings"]["replay_micros"].is_u64());
    let (_, runs) = api.get(&format!("{V1}/streams/order-7/replays")).await;
    assert_eq!(
        runs.as_array().unwrap().len(),
        1,
        "the experiment's run is persisted"
    );
}

#[tokio::test]
async fn overview_reflects_real_data() {
    let api = TestApi::new();
    let (_, empty) = api.get(&format!("{V1}/overview")).await;
    assert_eq!(empty["statistics"]["streams"], 0);
    assert!(empty["recent_events"].as_array().unwrap().is_empty());
    order_stream(&api).await;
    api.create_stream("quiet").await;
    let (_, overview) = api.get(&format!("{V1}/overview")).await;
    assert_eq!(overview["statistics"]["streams"], 2);
    assert_eq!(overview["statistics"]["events"], 4);
    assert_eq!(overview["recent_events"].as_array().unwrap().len(), 4);
    assert_eq!(overview["active_streams"][0]["stream_id"], "order-7");
}

#[tokio::test]
async fn data_persists_across_restarts() {
    let mut api = TestApi::new();
    order_stream(&api).await;
    api.post(&format!("{V1}/streams/order-7/snapshots"), json!({}))
        .await;
    api.restart();
    let (_, stream) = api.get(&format!("{V1}/streams/order-7")).await;
    assert_eq!(stream["head_sequence"], 4);
    assert_eq!(stream["snapshot_count"], 1);
    let (_, state) = api.get(&format!("{V1}/streams/order-7/state")).await;
    assert_eq!(state["report"]["start"]["kind"], "snapshot");
}

#[tokio::test]
async fn benchmarks_are_read_from_criterion_output() {
    let dir = tempfile::tempdir().unwrap();
    let new = dir
        .path()
        .join("verify")
        .join("verify_stream_10k")
        .join("new");
    std::fs::create_dir_all(&new).unwrap();
    std::fs::write(new.join("benchmark.json"), json!({ "group_id": "verify", "function_id": "verify_stream_10k", "full_id": "verify/verify_stream_10k", "throughput": null }).to_string()).unwrap();
    std::fs::write(new.join("estimates.json"), json!({
        "mean": { "point_estimate": 11.5e6, "confidence_interval": { "confidence_level": 0.95, "lower_bound": 11.3e6, "upper_bound": 11.7e6 } },
        "median": { "point_estimate": 11.4e6, "confidence_interval": { "confidence_level": 0.95, "lower_bound": 11.3e6, "upper_bound": 11.5e6 } }
    }).to_string()).unwrap();
    let api = TestApi::with_limits(Limits::default(), Some(dir.path().to_path_buf()));
    let (_, caps) = api.get(&format!("{V1}/capabilities")).await;
    assert_eq!(caps["benchmarks_available"], true);
    let (status, body) = api.get(&format!("{V1}/benchmarks")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["available"], true);
    assert_eq!(body["results"][0]["id"], "verify/verify_stream_10k");
    assert_eq!(body["results"][0]["median_ns"], 11.4e6);
    assert!(
        !body.to_string().contains(dir.path().to_str().unwrap()),
        "no local paths in responses"
    );

    let (_, none) = TestApi::new().get(&format!("{V1}/benchmarks")).await;
    assert_eq!(none["available"], false);
    assert!(none["results"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn cors_allows_only_the_configured_origin() {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;
    let api = TestApi::new();
    for (origin, allowed) in [("http://localhost:3000", true), ("http://evil.test", false)] {
        let response = api
            .app
            .clone()
            .oneshot(
                Request::get("/api/v1/health")
                    .header("origin", origin)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response
                .headers()
                .contains_key("access-control-allow-origin"),
            allowed,
            "{origin}"
        );
        assert!(response.headers().contains_key("x-orbit-elapsed-micros"));
    }
}
