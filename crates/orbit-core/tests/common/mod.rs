//! Shared helpers for integration tests. Every test uses a real SQLite file
//! in its own temporary directory.
#![allow(dead_code, unreachable_pub)]

use std::path::{Path, PathBuf};

use orbit_core::{
    AppendRequest, Durability, EventStore, EventType, Metadata, StoreConfig, StreamId,
};
use serde_json::{Value, json};
use tempfile::TempDir;

pub struct TestDb {
    pub dir: TempDir,
    pub path: PathBuf,
}

impl TestDb {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("orbit.db");
        Self { dir, path }
    }

    pub fn config(&self) -> StoreConfig {
        // Normal durability keeps the suite fast; the Full path is covered separately.
        StoreConfig::new(&self.path).with_durability(Durability::Normal)
    }

    pub fn open(&self) -> EventStore {
        EventStore::open(self.config()).expect("open store")
    }

    /// A raw SQLite connection that bypasses ORBIT, used to inspect or
    /// deliberately damage the file.
    pub fn raw(&self) -> rusqlite::Connection {
        raw_connection(&self.path)
    }
}

pub fn raw_connection(path: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(path).expect("open raw connection")
}

pub fn sid(id: &str) -> StreamId {
    StreamId::new(id).expect("valid stream id")
}

pub fn ty(name: &str) -> EventType {
    EventType::new(name).expect("valid event type")
}

pub fn req(n: u64) -> AppendRequest {
    AppendRequest::new(ty("counter.incremented"), json!({ "n": n }))
}

pub fn meta(value: Value) -> Metadata {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

/// Creates `stream` and appends `count` events numbered 1..=count.
pub fn seed(store: &EventStore, stream: &StreamId, count: u64) {
    store
        .create_stream(stream, Metadata::new())
        .expect("create stream");
    for n in 1..=count {
        store.append(stream, req(n)).expect("append");
    }
}

pub fn inc(by: u64) -> AppendRequest {
    AppendRequest::new(ty("counter.incremented"), json!({ "by": by }))
}

pub fn dec(by: u64) -> AppendRequest {
    AppendRequest::new(ty("counter.decremented"), json!({ "by": by }))
}

pub fn reset() -> AppendRequest {
    AppendRequest::new(ty("counter.reset"), json!({}))
}

pub fn inventory(event_type: &str, payload: Value) -> AppendRequest {
    AppendRequest::new(ty(event_type), payload)
}

/// Creates `stream` and appends `requests` in order.
pub fn seed_with(store: &EventStore, stream: &StreamId, requests: Vec<AppendRequest>) {
    store
        .create_stream(stream, Metadata::new())
        .expect("create stream");
    for request in requests {
        store.append(stream, request).expect("append");
    }
}

/// `count` counter increments where event `n` increments by `n`.
pub fn counter_stream(store: &EventStore, stream: &StreamId, count: u64) {
    seed_with(store, stream, (1..=count).map(inc).collect());
}

pub fn at(n: u64) -> orbit_core::Target {
    orbit_core::Target::At(orbit_core::Sequence::new(n).expect("valid sequence"))
}

pub fn seqn(n: u64) -> orbit_core::Sequence {
    orbit_core::Sequence::new(n).expect("valid sequence")
}

/// Sum 1..=n, the counter value after `counter_stream(.., n)`.
pub fn triangle(n: u64) -> i64 {
    i64::try_from(n * (n + 1) / 2).expect("small")
}
