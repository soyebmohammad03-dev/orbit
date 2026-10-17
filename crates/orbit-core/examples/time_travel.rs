//! Historical reconstruction, snapshots and state diffs with the demonstration
//! `Inventory` projection.
//!
//! ```sh
//! cargo run -p orbit-core --example time_travel
//! ```

use orbit_core::projection::examples::Inventory;
use orbit_core::{
    AppendRequest, EventStore, EventType, Metadata, ReconstructOptions, Reconstructor, Sequence,
    StoreConfig, StreamId, Target,
};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store = EventStore::open(StoreConfig::new(dir.path().join("time_travel.db")))?;
    let warehouse = StreamId::new("warehouse-1")?;
    store.create_stream(&warehouse, Metadata::new())?;

    let history = [
        ("inventory.warehouse_named", json!({"name": "North"})),
        (
            "inventory.item_added",
            json!({"sku": "A-1", "name": "Bolt", "quantity": 10}),
        ),
        (
            "inventory.item_added",
            json!({"sku": "B-2", "name": "Nut", "quantity": 4}),
        ),
        ("inventory.reserved", json!({"sku": "A-1", "quantity": 3})),
        ("inventory.shipped", json!({"sku": "A-1", "quantity": 2})),
        ("inventory.discontinued", json!({"sku": "B-2"})),
    ];
    for (event_type, payload) in history {
        store.append(
            &warehouse,
            AppendRequest::new(EventType::new(event_type)?, payload),
        )?;
    }

    let inventory = Reconstructor::new(&store, Inventory);
    let options = ReconstructOptions::default();

    // 1. State at a historical position.
    let at_3 = inventory.reconstruct(&warehouse, Target::At(Sequence::new(3)?), options)?;
    println!(
        "items after event 3: {:?}",
        at_3.state.items.keys().collect::<Vec<_>>()
    );

    // 2. Snapshot it, then reconstruct the latest state starting from the snapshot.
    let created = inventory.create_snapshot(&warehouse, Target::At(Sequence::new(3)?), options)?;
    println!(
        "snapshot {} at sequence {}",
        created.saved.snapshot.id, created.saved.snapshot.sequence
    );
    let latest = inventory.reconstruct(&warehouse, Target::Latest, options)?;
    println!(
        "latest = #{}, started from {:?}, applied {} events",
        latest.report.reconstructed, latest.report.start, latest.report.events_applied
    );

    // 3. Snapshot-assisted and full replay agree.
    let check =
        inventory.compare_with_full_replay(&warehouse, Target::Latest, options.snapshots)?;
    println!("snapshot-assisted == full replay: {}", check.equivalent);

    // 4. What changed between event 3 and the head, and which events did it.
    let diff = inventory.diff(
        &warehouse,
        Sequence::new(3)?,
        latest.report.reconstructed,
        options,
    )?;
    println!("changes from #3 to #{}:", diff.to);
    println!("{}", serde_json::to_string_pretty(&diff.changes)?);

    let explanation = inventory.explain(&warehouse, Sequence::new(3)?, diff.to, options)?;
    for transition in &explanation.transitions {
        let paths: Vec<&str> = transition.changes.iter().map(|c| c.path.as_str()).collect();
        println!(
            "#{} {} -> {:?}",
            transition.event.sequence, transition.event.event_type, paths
        );
    }
    Ok(())
}
