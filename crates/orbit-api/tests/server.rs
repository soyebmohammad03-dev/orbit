//! The API over a real TCP socket: concurrency through HTTP and persistence.

mod common;

use std::sync::Arc;

use orbit_api::{AppState, router};
use orbit_core::{Durability, EventStore, StoreConfig};
use serde_json::{Value, json};

async fn serve(path: &std::path::Path) -> (String, tokio::task::JoinHandle<()>) {
    let store =
        EventStore::open(StoreConfig::new(path).with_durability(Durability::Normal)).unwrap();
    let app = router(AppState::new(store, None), &common::origins());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/api/v1", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (base, handle)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_optimistic_appends_over_http_have_exactly_one_winner() {
    let dir = tempfile::tempdir().unwrap();
    let (base, server) = serve(&dir.path().join("orbit.db")).await;
    let client = Arc::new(reqwest::Client::new());
    let status = client
        .post(format!("{base}/streams"))
        .json(&json!({ "stream_id": "race" }))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, 201);

    let attempts: Vec<_> = (0..8)
        .map(|n| {
            let (client, base) = (Arc::clone(&client), base.clone());
            tokio::spawn(async move {
                client
                    .post(format!("{base}/streams/race/events"))
                    .json(&json!({ "event_type": "attempt", "payload": { "n": n }, "expected_sequence": 0 }))
                    .send()
                    .await
                    .unwrap()
                    .status()
                    .as_u16()
            })
        })
        .collect();
    let mut statuses = Vec::new();
    for a in attempts {
        statuses.push(a.await.unwrap());
    }
    assert_eq!(
        statuses.iter().filter(|s| **s == 201).count(),
        1,
        "{statuses:?}"
    );
    assert_eq!(
        statuses.iter().filter(|s| **s == 409).count(),
        7,
        "{statuses:?}"
    );

    // Unconditional appends from many tasks never duplicate sequences.
    let blind: Vec<_> = (0..20)
        .map(|n| {
            let (client, base) = (Arc::clone(&client), base.clone());
            tokio::spawn(async move {
                client
                    .post(format!("{base}/streams/race/events"))
                    .json(&json!({ "event_type": "blind", "payload": n }))
                    .send()
                    .await
                    .unwrap()
                    .status()
                    .as_u16()
            })
        })
        .collect();
    for b in blind {
        assert_eq!(b.await.unwrap(), 201);
    }
    let page: Value = client
        .get(format!("{base}/streams/race/events?limit=500"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let seqs: Vec<u64> = page["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["sequence"].as_u64().unwrap())
        .collect();
    assert_eq!(seqs, (1..=21).collect::<Vec<_>>());
    let verify: Value = client
        .post(format!("{base}/verify"))
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(verify["valid"], true);
    server.abort();

    // A new server on the same file sees the same history.
    let (base, server) = serve(&dir.path().join("orbit.db")).await;
    let stream: Value = client
        .get(format!("{base}/streams/race"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stream["head_sequence"], 21);
    server.abort();
}
