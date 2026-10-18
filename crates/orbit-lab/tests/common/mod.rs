//! Shared helpers. Every test uses its own temporary directory holding an
//! event store (`orbit.db`) and a separate lab directory (`lab/`).
#![allow(dead_code, unreachable_pub)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use orbit_core::projection::examples::{Counter, CounterState};
use orbit_core::{
    AppendRequest, Durability, EventStore, EventType, Metadata, Projection, ProjectionError,
    ProjectionKey, StoreConfig, StoredEvent, StreamId,
};
use orbit_lab::{Catalog, Determinism, DifferentialSpec, Lab, ProjectionImplementation};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tempfile::TempDir;

pub struct Env {
    pub dir: TempDir,
    pub db: PathBuf,
    pub lab_dir: PathBuf,
}

impl Env {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = dir.path().join("orbit.db");
        let lab_dir = dir.path().join("lab");
        Self { dir, db, lab_dir }
    }

    pub fn store(&self) -> EventStore {
        EventStore::open(StoreConfig::new(&self.db).with_durability(Durability::Normal))
            .expect("open store")
    }

    pub fn lab(&self) -> Lab {
        Lab::open(&self.lab_dir, test_catalog()).expect("open lab")
    }

    pub fn lab_with(&self, catalog: Catalog) -> Lab {
        Lab::open(&self.lab_dir, catalog).expect("open lab")
    }

    /// A raw connection to the event store, bypassing ORBIT.
    pub fn raw_store(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(&self.db).expect("raw store")
    }

    /// A raw connection to the lab registry, bypassing the lab.
    pub fn raw_registry(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.lab_dir.join("lab.db")).expect("raw registry")
    }
}

pub fn sid(id: &str) -> StreamId {
    StreamId::new(id).expect("stream id")
}

fn ty(name: &str) -> EventType {
    EventType::new(name).expect("event type")
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

pub fn event(event_type: &str, payload: Value) -> AppendRequest {
    AppendRequest::new(ty(event_type), payload)
}

pub fn seed(store: &EventStore, stream: &str, requests: Vec<AppendRequest>) -> StreamId {
    let id = sid(stream);
    store
        .create_stream(&id, Metadata::new())
        .expect("create stream");
    for request in requests {
        store.append(&id, request).expect("append");
    }
    id
}

pub fn spec(stream: &str, left: &str, right: &str) -> DifferentialSpec {
    DifferentialSpec::from_json(json!({ "stream": stream, "left": left, "right": right }))
        .expect("valid spec")
}

/// `counter@1` that rejects the event at `sequence` (a deliberate failure).
#[derive(Debug, Clone, Copy)]
pub struct RejectsAt(pub u64);

impl Projection for RejectsAt {
    type State = CounterState;
    fn key(&self) -> ProjectionKey {
        Counter.key()
    }
    fn initial_state(&self) -> CounterState {
        CounterState::default()
    }
    fn apply(
        &self,
        state: CounterState,
        event: &StoredEvent,
    ) -> Result<CounterState, ProjectionError> {
        if event.sequence.get() == self.0 {
            return Err(ProjectionError::rejected(format!(
                "refusing event {}",
                self.0
            )));
        }
        Counter.apply(state, event)
    }
}

/// `counter@1` that panics at `sequence`.
#[derive(Debug, Clone, Copy)]
pub struct PanicsAt(pub u64);

impl Projection for PanicsAt {
    type State = CounterState;
    fn key(&self) -> ProjectionKey {
        Counter.key()
    }
    fn initial_state(&self) -> CounterState {
        CounterState::default()
    }
    fn apply(
        &self,
        state: CounterState,
        event: &StoredEvent,
    ) -> Result<CounterState, ProjectionError> {
        assert!(
            event.sequence.get() != self.0,
            "deliberate panic at {}",
            self.0
        );
        Counter.apply(state, event)
    }
}

/// Breaks the projection contract: every execution adds a process-global
/// number to the state, so two executions never agree.
#[derive(Debug, Clone, Copy)]
pub struct GlobalCounter;

static CALLS: AtomicU64 = AtomicU64::new(0);

impl Projection for GlobalCounter {
    type State = CounterState;
    fn key(&self) -> ProjectionKey {
        Counter.key()
    }
    fn initial_state(&self) -> CounterState {
        let n = CALLS.fetch_add(1, Ordering::SeqCst);
        CounterState {
            value: i64::try_from(n).unwrap_or(0),
        }
    }
    fn apply(
        &self,
        state: CounterState,
        event: &StoredEvent,
    ) -> Result<CounterState, ProjectionError> {
        Counter.apply(state, event)
    }
}

/// A word-count projection over `{"word": str}` events, keeping its counts
/// in a `HashMap` (whose iteration order is randomised per map).
#[derive(Debug, Clone, Copy)]
pub struct HashWords;

/// The same projection with a `BTreeMap`.
#[derive(Debug, Clone, Copy)]
pub struct TreeWords;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HashState {
    pub counts: HashMap<String, u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TreeState {
    pub counts: std::collections::BTreeMap<String, u64>,
}

fn word(event: &StoredEvent) -> Result<String, ProjectionError> {
    event.payload["word"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| ProjectionError::rejected("missing word"))
}

impl Projection for HashWords {
    type State = HashState;
    fn key(&self) -> ProjectionKey {
        ProjectionKey::new("words", 1).expect("key")
    }
    fn initial_state(&self) -> HashState {
        HashState::default()
    }
    fn apply(
        &self,
        mut state: HashState,
        event: &StoredEvent,
    ) -> Result<HashState, ProjectionError> {
        *state.counts.entry(word(event)?).or_default() += 1;
        Ok(state)
    }
}

impl Projection for TreeWords {
    type State = TreeState;
    fn key(&self) -> ProjectionKey {
        ProjectionKey::new("words", 1).expect("key")
    }
    fn initial_state(&self) -> TreeState {
        TreeState::default()
    }
    fn apply(
        &self,
        mut state: TreeState,
        event: &StoredEvent,
    ) -> Result<TreeState, ProjectionError> {
        *state.counts.entry(word(event)?).or_default() += 1;
        Ok(state)
    }
}

/// The built-in catalog plus the test implementations above.
pub fn test_catalog() -> Catalog {
    let mut catalog = Catalog::builtin();
    let entries: Vec<Arc<dyn orbit_lab::Implementation>> = vec![
        Arc::new(
            ProjectionImplementation::new(
                "test.rejects-at-3",
                "Counter",
                RejectsAt(3),
                Determinism::Pure,
            )
            .unwrap(),
        ),
        Arc::new(
            ProjectionImplementation::new(
                "test.panics-at-2",
                "Counter",
                PanicsAt(2),
                Determinism::Pure,
            )
            .unwrap(),
        ),
        Arc::new(
            ProjectionImplementation::new(
                "test.global-counter",
                "Counter",
                GlobalCounter,
                Determinism::Pure,
            )
            .unwrap(),
        ),
        Arc::new(
            ProjectionImplementation::new(
                "test.counter-uncontrolled",
                "Counter",
                Counter,
                Determinism::Uncontrolled,
            )
            .unwrap(),
        ),
        Arc::new(
            ProjectionImplementation::new("test.hash-words", "Words", HashWords, Determinism::Pure)
                .unwrap(),
        ),
        Arc::new(
            ProjectionImplementation::new("test.tree-words", "Words", TreeWords, Determinism::Pure)
                .unwrap(),
        ),
    ];
    for entry in entries {
        catalog.register(entry).unwrap();
    }
    catalog
}
