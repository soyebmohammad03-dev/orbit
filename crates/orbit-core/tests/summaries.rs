//! Listing and counting queries used by observability tooling.

mod common;

use common::{TestDb, counter_stream, sid};
use orbit_core::projection::examples::Counter;
use orbit_core::replay::{ReplayLab, ReplayStart};
use orbit_core::{Metadata, ReconstructOptions, Reconstructor, StoreError, Target};

#[test]
fn summaries_statistics_and_recent_events() {
    let db = TestDb::new();
    let store = db.open();
    let empty = store.statistics().unwrap();
    assert_eq!(
        (empty.streams, empty.events, empty.schema_version),
        (0, 0, 3)
    );
    assert!(store.recent_events(5).unwrap().is_empty());

    counter_stream(&store, &sid("b"), 3);
    counter_stream(&store, &sid("a"), 2);
    store.create_stream(&sid("c"), Metadata::new()).unwrap();
    Reconstructor::new(&store, Counter)
        .create_snapshot(&sid("b"), Target::Latest, ReconstructOptions::default())
        .unwrap();
    let lab = ReplayLab::new(&store, Counter);
    let run = lab
        .record(&sid("a"), ReplayStart::Initial, Target::Latest)
        .unwrap();
    lab.execute(run.id).unwrap();

    let all = store.stream_summaries(None, 10).unwrap();
    let ids: Vec<&str> = all.iter().map(|s| s.stream_id.as_str()).collect();
    assert_eq!(ids, vec!["a", "b", "c"]);
    assert_eq!(all[1].head.sequence.get(), 3);
    assert_eq!(all[1].snapshot_count, 1);
    assert_eq!(all[0].replay_run_count, 1);
    assert!(all[2].last_recorded_at.is_none());
    assert_eq!(
        all[1].last_recorded_at,
        Some(
            store
                .event(&sid("b"), all[1].head.sequence)
                .unwrap()
                .recorded_at
        )
    );

    let page = store.stream_summaries(Some(&sid("a")), 1).unwrap();
    assert_eq!(page[0].stream_id, sid("b"));
    assert_eq!(
        store.stream_summary(&sid("c")).unwrap().head.sequence.get(),
        0
    );
    assert!(matches!(
        store.stream_summary(&sid("zzz")),
        Err(StoreError::StreamNotFound(_))
    ));

    let stats = store.statistics().unwrap();
    assert_eq!(
        (
            stats.streams,
            stats.events,
            stats.snapshots,
            stats.replay_runs,
            stats.replay_executions
        ),
        (3, 5, 1, 1, 1)
    );
    let recent = store.recent_events(3).unwrap();
    assert_eq!(recent.len(), 3);
    assert!(
        recent
            .windows(2)
            .all(|w| w[0].recorded_at >= w[1].recorded_at)
    );
    assert_eq!(
        store
            .latest_snapshot_sequence(&sid("b"))
            .unwrap()
            .map(|s| s.get()),
        Some(3)
    );
    assert_eq!(store.latest_snapshot_sequence(&sid("a")).unwrap(), None);
}
