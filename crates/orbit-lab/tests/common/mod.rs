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

/// `merge-patch@1` that keeps `null` members instead of deleting them
/// (unlike RFC 7386). Differs from `orbit.merge-patch` exactly where a patch
/// sets a member to `null`.
#[derive(Debug, Clone, Copy)]
pub struct KeepNulls;

fn merge_keep_nulls(target: &mut Value, patch: &Value) {
    match patch {
        Value::Object(members) => {
            if !target.is_object() {
                *target = Value::Object(serde_json::Map::new());
            }
            let map = target.as_object_mut().expect("object");
            for (key, value) in members {
                merge_keep_nulls(map.entry(key.clone()).or_insert(Value::Null), value);
            }
        }
        other => *target = other.clone(),
    }
}

impl Projection for KeepNulls {
    type State = Value;
    fn key(&self) -> ProjectionKey {
        ProjectionKey::new("merge-patch", 1).expect("key")
    }
    fn initial_state(&self) -> Value {
        json!({})
    }
    fn apply(&self, mut state: Value, event: &StoredEvent) -> Result<Value, ProjectionError> {
        merge_keep_nulls(&mut state, &event.payload);
        Ok(state)
    }
}

/// `counter@1` whose increments add 1000 while [`SWITCH`] is on. Declared
/// pure, which it is within one run; used to make a re-execution differ.
#[derive(Debug, Clone, Copy)]
pub struct Switchable;

pub static SWITCH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

impl Projection for Switchable {
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
        let next = Counter.apply(state, event)?;
        if SWITCH.load(Ordering::SeqCst) && event.event_type.as_str() == "counter.incremented" {
            return Ok(CounterState {
                value: next.value + 1000,
            });
        }
        Ok(next)
    }
}

/// `counter@1` capped at 10 on increments.
#[derive(Debug, Clone, Copy)]
pub struct CappedAtTen;

impl Projection for CappedAtTen {
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
        let next = Counter.apply(state, event)?;
        Ok(CounterState {
            value: next.value.min(10),
        })
    }
}

/// The test catalog plus the Sprint 8 test implementations.
pub fn sprint8_catalog() -> Catalog {
    let mut catalog = test_catalog();
    let entries: Vec<Arc<dyn orbit_lab::Implementation>> = vec![
        Arc::new(
            ProjectionImplementation::new(
                "test.keep-nulls",
                "Merge patch",
                KeepNulls,
                Determinism::Pure,
            )
            .unwrap(),
        ),
        Arc::new(
            ProjectionImplementation::new(
                "test.switchable",
                "Counter",
                Switchable,
                Determinism::Pure,
            )
            .unwrap(),
        ),
        Arc::new(
            ProjectionImplementation::new(
                "test.capped-at-ten",
                "Counter",
                CappedAtTen,
                Determinism::Pure,
            )
            .unwrap(),
        ),
    ];
    for entry in entries {
        catalog.register(entry).unwrap();
    }
    catalog
}

/// Independent reference: the state after every event (index = boundary),
/// computed by folding the projection directly and encoding with serde_json,
/// stopping at the first error.
pub fn reference_states<P: Projection>(p: &P, events: &[StoredEvent]) -> Vec<Value> {
    let mut state = p.initial_state();
    let mut out = vec![serde_json::to_value(&state).unwrap()];
    for event in events {
        match p.apply(state, event) {
            Ok(next) => state = next,
            Err(_) => break,
        }
        out.push(serde_json::to_value(&state).unwrap());
    }
    out
}

/// Independent reference: maximal runs of differing boundaries in
/// `from..`, as `(first, last, converged_at)`, compared with `Value` equality.
pub fn reference_intervals(
    left: &[Value],
    right: &[Value],
    from: usize,
) -> Vec<(u64, u64, Option<u64>)> {
    let n = left.len().min(right.len());
    let mut out = Vec::new();
    let mut open: Option<(u64, u64)> = None;
    for b in from..n {
        if left[b] == right[b] {
            if let Some((first, last)) = open.take() {
                out.push((first, last, Some(b as u64)));
            }
        } else {
            open = Some(match open {
                Some((first, _)) => (first, b as u64),
                None => (b as u64, b as u64),
            });
        }
    }
    if let Some((first, last)) = open {
        out.push((first, last, None));
    }
    out
}
