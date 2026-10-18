//! Builds a counter stream of N events (default 100,000, the lab's limit) in
//! a temporary directory and runs one differential replay over it, printing
//! the sizes of the result and the artifacts and the wall-clock time.
//!
//! ```text
//! cargo run --release -p orbit-lab --example large_history -- 100000
//! ```
//!
//! Every tenth event dips below zero and the next resets, so the counter
//! and its floor-at-zero variant diverge and converge N / 10 times.
#![allow(missing_docs)]

use std::time::Instant;

use orbit_core::{
    AppendRequest, Durability, EventStore, EventType, Metadata, StoreConfig, StreamId,
};
use orbit_lab::{Catalog, DifferentialSpec, Lab};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let n: u64 = std::env::args()
        .nth(1)
        .map(|a| a.parse())
        .transpose()?
        .unwrap_or(100_000);
    let dir = tempfile::tempdir()?;
    let store = EventStore::open(
        StoreConfig::new(dir.path().join("orbit.db")).with_durability(Durability::Normal),
    )?;
    let stream = StreamId::new("large")?;
    store.create_stream(&stream, Metadata::new())?;
    let ty = |t: &str| EventType::new(t);
    let started = Instant::now();
    for i in 0..n {
        let request = match i % 10 {
            8 => AppendRequest::new(ty("counter.decremented")?, json!({ "by": 100 })),
            9 => AppendRequest::new(ty("counter.reset")?, json!({})),
            _ => AppendRequest::new(ty("counter.incremented")?, json!({ "by": 1 })),
        };
        store.append(&stream, request)?;
    }
    println!("appended {n} events in {:.2?}", started.elapsed());

    let lab = Lab::open(&dir.path().join("lab"), Catalog::builtin())?;
    let spec = DifferentialSpec::from_json(json!({
        "stream": "large",
        "left": "orbit.counter",
        "right": "example.counter-floor-at-zero",
    }))?;
    let started = Instant::now();
    let record = lab.create_and_run(&spec, &store)?;
    let elapsed = started.elapsed();
    let result = record
        .result
        .as_ref()
        .ok_or("experiment did not complete")?;
    println!(
        "differential replay: {} in {elapsed:.2?}",
        record.outcome.as_deref().unwrap_or("-")
    );
    println!(
        "boundaries compared {}, differing {}, intervals {} (listed {})",
        result["boundaries_compared"],
        result["boundaries_differing"],
        result["timeline"]["intervals_total"],
        result["timeline"]["intervals"]
            .as_array()
            .map_or(0, Vec::len),
    );
    println!(
        "result document: {} bytes",
        serde_json::to_string(result)?.len()
    );
    for a in &record.artifacts {
        println!("artifact {}: {} bytes", a.role, a.size);
    }
    Ok(())
}
