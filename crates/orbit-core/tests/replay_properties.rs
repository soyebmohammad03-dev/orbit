//! Property tests for replay determinism, divergence localisation, and
//! fault-injected histories.

mod common;

use std::sync::{Arc, Mutex};

use common::{TestDb, dec, inc, reset, seed_with, sid};
use orbit_core::fault::FaultPoint;
use orbit_core::projection::examples::{Counter, CounterState};
use orbit_core::replay::{ReplayLab, ReplayOutcome, ReplayStart};
use orbit_core::{
    AppendRequest, Metadata, Projection, ProjectionError, ProjectionKey, Sequence, SnapshotPolicy,
    StoreError, StoredEvent, Target,
};
use proptest::prelude::*;

#[derive(Debug, Clone, Copy)]
enum Op {
    Inc(u64),
    Dec(u64),
    Reset,
}

fn ops() -> impl Strategy<Value = Vec<Op>> {
    prop::collection::vec(
        prop_oneof![
            4 => (0u64..50).prop_map(Op::Inc),
            2 => (0u64..50).prop_map(Op::Dec),
            1 => Just(Op::Reset),
        ],
        0..40,
    )
}

fn request(op: Op) -> AppendRequest {
    match op {
        Op::Inc(n) => inc(n),
        Op::Dec(n) => dec(n),
        Op::Reset => reset(),
    }
}

/// `counter@v1` impostor that adds 1 to every increment from `from_sequence` on.
struct Drift(u64);

impl Projection for Drift {
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
        let drift =
            event.sequence.get() >= self.0 && event.event_type.as_str() == "counter.incremented";
        Ok(CounterState {
            value: next.value + i64::from(drift),
        })
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 32, ..ProptestConfig::default() })]

    #[test]
    fn replay_is_exact_and_divergence_is_localised(
        ops in ops(),
        from_pick in any::<u64>(),
        len_pick in any::<u64>(),
        drift_pick in any::<u64>(),
        use_snapshots in any::<bool>(),
    ) {
        let db = TestDb::new();
        let store = db.open();
        let id = sid("p");
        seed_with(&store, &id, ops.iter().copied().map(request).collect());
        let n = ops.len() as u64;
        let from = from_pick % (n + 1);
        let to = from + len_pick % (n - from + 1);
        if use_snapshots && from > 0 {
            orbit_core::Reconstructor::new(&store, Counter)
                .create_snapshot(&id, Target::At(Sequence::new(from).unwrap()), Default::default())
                .unwrap();
        }
        let lab = ReplayLab::new(&store, Counter);
        let start = if from == 0 {
            ReplayStart::Initial
        } else {
            ReplayStart::Reconstructed {
                at: Sequence::new(from).unwrap(),
                snapshots: if use_snapshots { SnapshotPolicy::Strict } else { SnapshotPolicy::Ignore },
            }
        };
        let run = lab.record(&id, start, Target::At(Sequence::new(to).unwrap())).unwrap();
        prop_assert_eq!(run.trace.len() as u64, to - from);

        // Same projection: always an exact match, with identical digests every time.
        let a = lab.execute(run.id).unwrap();
        let b = lab.execute(run.id).unwrap();
        prop_assert_eq!(&a.outcome, &ReplayOutcome::ExactMatch);
        prop_assert_eq!(a.actual_state_digest, b.actual_state_digest);
        prop_assert_eq!(a.actual_state_digest, Some(run.expected_state_digest));

        // Drifting projection: the first divergence is exactly the first
        // increment at or after the drift point inside the interval.
        let drift_from = drift_pick % (n + 2) + 1;
        let expected_first = ((from + 1)..=to).find(|s| {
            *s >= drift_from && matches!(ops[usize::try_from(*s - 1).unwrap()], Op::Inc(_))
        });
        let outcome = ReplayLab::new(&store, Drift(drift_from)).execute(run.id).unwrap().outcome;
        match (expected_first, &outcome) {
            (None, ReplayOutcome::ExactMatch) => {}
            (Some(s), ReplayOutcome::Diverged { first_divergence, .. })
            | (Some(s), ReplayOutcome::Reconverged { first_divergence }) => {
                prop_assert_eq!(first_divergence.sequence.get(), s);
                prop_assert_eq!(first_divergence.matched_events, s - from - 1);
            }
            (expected, other) => prop_assert!(false, "expected first divergence {:?}, got {:?}", expected, other),
        }
    }

    #[test]
    fn random_fault_schedules_never_commit_failed_appends(
        schedule in prop::collection::vec(prop::option::of(0usize..5), 1..40),
    ) {
        const POINTS: [FaultPoint; 5] = [
            FaultPoint::AppendBeforeBegin,
            FaultPoint::AppendAfterBegin,
            FaultPoint::AppendAfterSequenceAllocated,
            FaultPoint::AppendAfterInsert,
            FaultPoint::AppendBeforeCommit,
        ];
        let db = TestDb::new();
        let store = db.open();
        let id = sid("f");
        store.create_stream(&id, Metadata::new()).unwrap();
        let current: Arc<Mutex<Option<FaultPoint>>> = Arc::new(Mutex::new(None));
        let hook_state = Arc::clone(&current);
        store.set_fault_hook(Some(Arc::new(move |p| *hook_state.lock().unwrap() == Some(p))));

        let mut committed = Vec::new();
        for (i, fault) in schedule.iter().enumerate() {
            *current.lock().unwrap() = fault.map(|f| POINTS[f]);
            match store.append(&id, inc(i as u64)) {
                Ok(_) => {
                    prop_assert!(fault.is_none());
                    committed.push(i as u64);
                }
                Err(StoreError::InjectedFault(p)) => prop_assert_eq!(Some(p), fault.map(|f| POINTS[f])),
                Err(other) => prop_assert!(false, "unexpected {}", other),
            }
        }
        drop(store);
        let store = db.open();
        let stored: Vec<u64> = store
            .read_stream(&id)
            .unwrap()
            .iter()
            .map(|e| e.payload["by"].as_u64().unwrap())
            .collect();
        prop_assert_eq!(stored, committed);
        prop_assert!(store.verify_store().unwrap().is_valid());
    }
}
