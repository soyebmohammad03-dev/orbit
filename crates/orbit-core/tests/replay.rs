//! Replay runs: recording, persistence, re-execution, determinism, and every
//! divergence classification on controlled examples.

mod common;

use common::{TestDb, at, counter_stream, dec, inc, reset, seed_with, seqn, sid, triangle};
use orbit_core::projection::examples::{Counter, CounterState};
use orbit_core::reconstruct::StartPoint;
use orbit_core::replay::{
    IntegrityViolation, ReplayError, ReplayLab, ReplayOutcome, ReplayRunId, ReplayStart,
    ReplayStartSource, UnavailableReason, state_digest,
};
use orbit_core::{
    IntegrityCategory, Projection, ProjectionError, ProjectionKey, ReconstructOptions,
    ReconstructionError, Reconstructor, Sequence, SnapshotPolicy, StoreError, StoredEvent, Target,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Claims to be `counter@v1` but, from `from_sequence` on, adds one extra on
/// every increment: a behaviour change shipped without a version bump.
struct DriftingCounter {
    from_sequence: u64,
}

impl Projection for DriftingCounter {
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
        if event.sequence.get() >= self.from_sequence
            && event.event_type.as_str() == "counter.incremented"
        {
            Ok(CounterState {
                value: next.value + 1,
            })
        } else {
            Ok(next)
        }
    }
}

/// Claims to be `counter@v1` but no longer accepts resets.
struct NoResetCounter;

impl Projection for NoResetCounter {
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
        if event.event_type.as_str() == "counter.reset" {
            return Err(ProjectionError::unsupported(event));
        }
        Counter.apply(state, event)
    }
}

/// Claims to be `counter@v1` with an incompatible state type.
struct WrongStateCounter;

#[derive(Clone, Serialize, Deserialize)]
struct Tally {
    tally: Vec<u64>,
}

impl Projection for WrongStateCounter {
    type State = Tally;
    fn key(&self) -> ProjectionKey {
        Counter.key()
    }
    fn initial_state(&self) -> Tally {
        Tally { tally: Vec::new() }
    }
    fn apply(&self, state: Tally, _: &StoredEvent) -> Result<Tally, ProjectionError> {
        Ok(state)
    }
}

/// A properly versioned successor.
struct CounterV2;

impl Projection for CounterV2 {
    type State = CounterState;
    fn key(&self) -> ProjectionKey {
        ProjectionKey::new("counter", 2).unwrap()
    }
    fn initial_state(&self) -> CounterState {
        CounterState::default()
    }
    fn apply(
        &self,
        state: CounterState,
        event: &StoredEvent,
    ) -> Result<CounterState, ProjectionError> {
        Counter.apply(state, event)
    }
}

fn unlock(db: &TestDb) {
    db.raw()
        .execute_batch(
            "DROP TRIGGER events_no_update;
             DROP TRIGGER events_no_delete;
             DROP TRIGGER streams_head_only_advances;
             DROP TRIGGER snapshots_no_update;
             DROP TRIGGER replay_runs_no_update;",
        )
        .unwrap();
}

#[test]
fn record_execute_and_inspect() {
    let db = TestDb::new();
    let id = sid("c");
    let run_id = {
        let store = db.open();
        counter_stream(&store, &id, 5);
        let lab = ReplayLab::new(&store, Counter);
        let run = lab
            .record(&id, ReplayStart::Initial, Target::Latest)
            .unwrap();
        assert_eq!((run.from, run.to), (Sequence::ZERO, seqn(5)));
        assert_eq!(run.start_source, ReplayStartSource::Initial);
        assert_eq!(run.trace.len(), 5);
        assert_eq!(run.expected_state, json!({"value": triangle(5)}));
        assert_eq!(run.expected_state_digest, state_digest(&run.expected_state));
        assert_eq!(run.trace[4], run.expected_state_digest);
        assert_eq!(run.to_event_digest, store.head(&id).unwrap().digest);
        for n in 1..=5u64 {
            assert_eq!(
                run.trace[n as usize - 1],
                state_digest(&json!({"value": triangle(n)}))
            );
        }

        let execution = lab.execute(run.id).unwrap();
        assert_eq!(execution.outcome, ReplayOutcome::ExactMatch);
        assert_eq!(
            execution.actual_state_digest,
            Some(run.expected_state_digest)
        );
        assert_eq!(execution.actual_position, Some(seqn(5)));
        assert_eq!(execution.events_applied, 5);
        assert!(execution.warnings.is_empty());
        lab.execute(run.id).unwrap();

        let inspection = lab.inspect(run.id).unwrap();
        assert_eq!(inspection.run, run);
        assert_eq!(inspection.executions.len(), 2);
        assert_eq!(inspection.executions[0].outcome, "exact_match");
        assert_eq!(
            inspection.executions[0].report["outcome"]["status"],
            "exact_match"
        );
        assert_eq!(lab.runs(&id).unwrap(), vec![run.clone()]);
        run.id
    };
    // Runs and executions persist across reopen and can be re-executed.
    let store = db.open();
    let lab = ReplayLab::new(&store, Counter);
    assert_eq!(lab.inspect(run_id).unwrap().executions.len(), 2);
    assert_eq!(
        lab.execute(run_id).unwrap().outcome,
        ReplayOutcome::ExactMatch
    );
    assert_eq!(store.replay_executions(run_id).unwrap().len(), 3);
    assert!(store.verify_store().unwrap().is_valid());
}

#[test]
fn replay_is_deterministic() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("c");
    seed_with(
        &store,
        &id,
        vec![inc(4), dec(9), reset(), inc(2), inc(8), dec(1)],
    );
    let lab = ReplayLab::new(&store, Counter);
    let a = lab
        .record(&id, ReplayStart::Initial, Target::Latest)
        .unwrap();
    let b = lab
        .record(&id, ReplayStart::Initial, Target::Latest)
        .unwrap();
    assert_ne!(a.id, b.id);
    assert_eq!(a.trace, b.trace);
    assert_eq!(a.expected_state_digest, b.expected_state_digest);
    let outcomes: Vec<_> = (0..3).map(|_| lab.execute(a.id).unwrap()).collect();
    for e in &outcomes {
        assert_eq!(e.outcome, ReplayOutcome::ExactMatch);
        assert_eq!(e.actual_state_digest, outcomes[0].actual_state_digest);
    }
}

#[test]
fn runs_can_start_from_snapshots_or_reconstructed_states() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("c");
    counter_stream(&store, &id, 8);
    let snapshot = Reconstructor::new(&store, Counter)
        .create_snapshot(&id, at(3), ReconstructOptions::default())
        .unwrap()
        .saved
        .snapshot;
    let lab = ReplayLab::new(&store, Counter);

    let from_snapshot = lab
        .record(
            &id,
            ReplayStart::Snapshot {
                snapshot_id: snapshot.id,
            },
            Target::Latest,
        )
        .unwrap();
    assert_eq!((from_snapshot.from, from_snapshot.to), (seqn(3), seqn(8)));
    assert_eq!(
        from_snapshot.start_source,
        ReplayStartSource::Snapshot {
            snapshot_id: snapshot.id
        }
    );
    assert_eq!(from_snapshot.start_state, snapshot.state);
    assert_eq!(from_snapshot.trace.len(), 5);
    assert_eq!(
        lab.execute(from_snapshot.id).unwrap().outcome,
        ReplayOutcome::ExactMatch
    );

    let reconstructed = lab
        .record(
            &id,
            ReplayStart::Reconstructed {
                at: seqn(5),
                snapshots: SnapshotPolicy::Prefer,
            },
            at(7),
        )
        .unwrap();
    assert_eq!((reconstructed.from, reconstructed.to), (seqn(5), seqn(7)));
    assert_eq!(
        reconstructed.start_source,
        ReplayStartSource::Reconstructed {
            via_snapshot: Some(snapshot.id)
        }
    );
    assert_eq!(reconstructed.start_state, json!({"value": triangle(5)}));
    assert_eq!(
        lab.execute(reconstructed.id).unwrap().outcome,
        ReplayOutcome::ExactMatch
    );

    // An empty interval is a valid (trivial) run.
    let empty = lab
        .record(
            &id,
            ReplayStart::Reconstructed {
                at: seqn(8),
                snapshots: SnapshotPolicy::Ignore,
            },
            Target::Latest,
        )
        .unwrap();
    assert!(empty.trace.is_empty());
    assert_eq!(empty.start_state_digest, empty.expected_state_digest);
    assert_eq!(
        lab.execute(empty.id).unwrap().outcome,
        ReplayOutcome::ExactMatch
    );
}

#[test]
fn recording_rejects_invalid_intervals_and_damaged_inputs() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("c");
    counter_stream(&store, &id, 4);
    let lab = ReplayLab::new(&store, Counter);
    assert!(matches!(
        lab.record(&id, ReplayStart::Initial, at(5)),
        Err(ReplayError::Reconstruction(
            ReconstructionError::TargetBeyondHead { .. }
        ))
    ));
    assert!(matches!(
        lab.record(
            &id,
            ReplayStart::Reconstructed {
                at: seqn(3),
                snapshots: SnapshotPolicy::Ignore
            },
            at(2)
        ),
        Err(ReplayError::InvalidRange { .. })
    ));
    assert!(matches!(
        lab.record(&sid("ghost"), ReplayStart::Initial, Target::Latest),
        Err(ReplayError::Store(StoreError::StreamNotFound(_)))
    ));
    assert!(matches!(
        lab.execute(ReplayRunId::generate()),
        Err(ReplayError::Store(StoreError::ReplayRunNotFound(_)))
    ));

    // A corrupt snapshot cannot be used as a start state.
    let snapshot = Reconstructor::new(&store, Counter)
        .create_snapshot(&id, at(2), ReconstructOptions::default())
        .unwrap()
        .saved
        .snapshot;
    unlock(&db);
    db.raw()
        .execute(
            "UPDATE snapshots SET state = '{\"value\":99}' WHERE snapshot_id = ?1",
            [snapshot.id.as_bytes().as_slice()],
        )
        .unwrap();
    assert!(matches!(
        lab.record(
            &id,
            ReplayStart::Snapshot {
                snapshot_id: snapshot.id
            },
            Target::Latest
        ),
        Err(ReplayError::Reconstruction(
            ReconstructionError::Snapshot { .. }
        ))
    ));

    // Damaged history is refused rather than baked into an expectation.
    db.raw()
        .execute(
            "UPDATE events SET payload = '{\"by\":0}' WHERE sequence = 3",
            [],
        )
        .unwrap();
    match lab.record(&id, ReplayStart::Initial, Target::Latest) {
        Err(ReplayError::HistoryIntegrity(report)) => {
            assert_eq!(report.categories(), vec![IntegrityCategory::ModifiedEvent]);
        }
        other => panic!("expected a history integrity error, got {other:?}"),
    }
    assert!(lab.runs(&id).unwrap().is_empty());
}

#[test]
fn genuine_divergence_reports_the_first_divergent_event() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("c");
    counter_stream(&store, &id, 6);
    let run = ReplayLab::new(&store, Counter)
        .record(&id, ReplayStart::Initial, Target::Latest)
        .unwrap();
    let fourth = store.event(&id, seqn(4)).unwrap();

    let execution = ReplayLab::new(&store, DriftingCounter { from_sequence: 4 })
        .execute(run.id)
        .unwrap();
    match &execution.outcome {
        ReplayOutcome::Diverged {
            first_divergence,
            changes,
        } => {
            assert_eq!(first_divergence.sequence, seqn(4));
            assert_eq!(first_divergence.event_id, fourth.event_id);
            assert_eq!(first_divergence.event_type.as_str(), "counter.incremented");
            assert_eq!(first_divergence.matched_events, 3);
            assert_eq!(first_divergence.expected_state_digest, run.trace[3]);
            assert_ne!(first_divergence.actual_state_digest, run.trace[3]);
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].path, "/value");
        }
        other => panic!("expected divergence, got {other:?}"),
    }
    assert_ne!(
        execution.actual_state_digest,
        Some(run.expected_state_digest)
    );
    assert_eq!(
        store.replay_executions(run.id).unwrap()[0].outcome,
        "diverged"
    );
}

#[test]
fn intermediate_divergence_that_reconverges_is_distinguished() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("c");
    seed_with(&store, &id, vec![inc(1), inc(2), reset(), dec(5)]);
    let run = ReplayLab::new(&store, Counter)
        .record(&id, ReplayStart::Initial, Target::Latest)
        .unwrap();
    let execution = ReplayLab::new(&store, DriftingCounter { from_sequence: 1 })
        .execute(run.id)
        .unwrap();
    match execution.outcome {
        ReplayOutcome::Reconverged { first_divergence } => {
            assert_eq!(first_divergence.sequence, seqn(1))
        }
        other => panic!("expected reconvergence, got {other:?}"),
    }
    assert_eq!(
        execution.actual_state_digest,
        Some(run.expected_state_digest)
    );
}

#[test]
fn projection_failure_is_its_own_outcome() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("c");
    seed_with(&store, &id, vec![inc(1), reset(), inc(2)]);
    let run = ReplayLab::new(&store, Counter)
        .record(&id, ReplayStart::Initial, Target::Latest)
        .unwrap();
    let execution = ReplayLab::new(&store, NoResetCounter)
        .execute(run.id)
        .unwrap();
    match execution.outcome {
        ReplayOutcome::ProjectionFailed {
            failure,
            first_divergence,
        } => {
            assert_eq!(failure.sequence, seqn(2));
            assert!(first_divergence.is_none());
        }
        other => panic!("expected projection failure, got {other:?}"),
    }
    assert_eq!(execution.actual_state_digest, None);
}

#[test]
fn projection_version_mismatch_is_never_substituted() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("c");
    counter_stream(&store, &id, 3);
    let run = ReplayLab::new(&store, Counter)
        .record(&id, ReplayStart::Initial, Target::Latest)
        .unwrap();
    let execution = ReplayLab::new(&store, CounterV2).execute(run.id).unwrap();
    assert_eq!(
        execution.outcome,
        ReplayOutcome::Unavailable {
            reason: UnavailableReason::ProjectionMismatch {
                recorded: Counter.key(),
                current: CounterV2.key(),
            }
        }
    );
    assert_eq!(execution.events_applied, 0);
}

#[test]
fn undecodable_start_state_is_invalid_input() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("c");
    counter_stream(&store, &id, 2);
    let run = ReplayLab::new(&store, Counter)
        .record(&id, ReplayStart::Initial, Target::Latest)
        .unwrap();
    let execution = ReplayLab::new(&store, WrongStateCounter)
        .execute(run.id)
        .unwrap();
    assert!(
        matches!(execution.outcome, ReplayOutcome::InvalidInput { .. }),
        "{:?}",
        execution.outcome
    );
}

#[test]
fn altered_expected_state_is_an_integrity_failure() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("c");
    counter_stream(&store, &id, 3);
    let lab = ReplayLab::new(&store, Counter);
    let run = lab
        .record(&id, ReplayStart::Initial, Target::Latest)
        .unwrap();
    unlock(&db);
    db.raw()
        .execute(
            "UPDATE replay_runs SET expected_state = '{\"value\":7}' WHERE run_id = ?1",
            [run.id.as_bytes().as_slice()],
        )
        .unwrap();
    let execution = lab.execute(run.id).unwrap();
    assert!(
        matches!(&execution.outcome, ReplayOutcome::IntegrityFailure { violations }
            if matches!(violations[..], [IntegrityViolation::RunRecord { .. }])),
        "{:?}",
        execution.outcome
    );
    // Decoding the tampered run for inspection is an explicit error too: the
    // expected state no longer matches its recorded digest.
    assert!(matches!(
        lab.run(run.id),
        Err(ReplayError::Store(StoreError::CorruptReplayRecord { .. }))
    ));
}

#[test]
fn damaged_source_history_is_an_integrity_failure() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("c");
    counter_stream(&store, &id, 6);
    let snapshot = Reconstructor::new(&store, Counter)
        .create_snapshot(&id, at(2), ReconstructOptions::default())
        .unwrap()
        .saved
        .snapshot;
    let lab = ReplayLab::new(&store, Counter);
    let inside = lab
        .record(
            &id,
            ReplayStart::Snapshot {
                snapshot_id: snapshot.id,
            },
            Target::Latest,
        )
        .unwrap();
    let boundary = lab
        .record(
            &id,
            ReplayStart::Snapshot {
                snapshot_id: snapshot.id,
            },
            Target::Latest,
        )
        .unwrap();
    unlock(&db);

    // An event inside the interval changes.
    db.raw()
        .execute(
            "UPDATE events SET payload = '{\"by\":100}' WHERE sequence = 4",
            [],
        )
        .unwrap();
    let execution = lab.execute(inside.id).unwrap();
    match &execution.outcome {
        ReplayOutcome::IntegrityFailure { violations } => {
            assert!(violations.iter().any(|v| matches!(v,
                IntegrityViolation::History { categories, .. } if categories == &vec![IntegrityCategory::ModifiedEvent])));
        }
        other => panic!("expected integrity failure, got {other:?}"),
    }

    // The boundary event the run starts from changes digest.
    db.raw()
        .execute(
            "UPDATE events SET digest = zeroblob(32) WHERE sequence = 2",
            [],
        )
        .unwrap();
    let execution = lab.execute(boundary.id).unwrap();
    match &execution.outcome {
        ReplayOutcome::IntegrityFailure { violations } => {
            assert!(
                violations
                    .iter()
                    .any(|v| matches!(v, IntegrityViolation::Boundary { which: "from", .. }))
            );
        }
        other => panic!("expected integrity failure, got {other:?}"),
    }
}

#[test]
fn truncated_history_makes_replay_unavailable() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("c");
    counter_stream(&store, &id, 5);
    let lab = ReplayLab::new(&store, Counter);
    let run = lab
        .record(&id, ReplayStart::Initial, Target::Latest)
        .unwrap();
    unlock(&db);
    db.raw()
        .execute_batch(
            "DELETE FROM events WHERE sequence = 5;
             UPDATE streams SET head_sequence = 4, head_digest = (SELECT digest FROM events WHERE sequence = 4);",
        )
        .unwrap();
    let execution = lab.execute(run.id).unwrap();
    assert_eq!(
        execution.outcome,
        ReplayOutcome::Unavailable {
            reason: UnavailableReason::HistoryUnavailable {
                required: seqn(5),
                head: seqn(4)
            }
        }
    );
}

#[test]
fn corrupt_start_snapshot_is_a_warning_not_a_different_result() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("c");
    counter_stream(&store, &id, 5);
    let snapshot = Reconstructor::new(&store, Counter)
        .create_snapshot(&id, at(2), ReconstructOptions::default())
        .unwrap()
        .saved
        .snapshot;
    let lab = ReplayLab::new(&store, Counter);
    let run = lab
        .record(
            &id,
            ReplayStart::Snapshot {
                snapshot_id: snapshot.id,
            },
            Target::Latest,
        )
        .unwrap();
    unlock(&db);
    db.raw()
        .execute(
            "UPDATE snapshots SET state = '{\"value\":0}' WHERE snapshot_id = ?1",
            [snapshot.id.as_bytes().as_slice()],
        )
        .unwrap();
    // The run stores its own start state, so the replay is still exact,
    // and the snapshot's corruption is reported.
    let execution = lab.execute(run.id).unwrap();
    assert_eq!(execution.outcome, ReplayOutcome::ExactMatch);
    assert_eq!(execution.warnings.len(), 1);
    assert!(
        execution.warnings[0].contains("unusable"),
        "{:?}",
        execution.warnings
    );

    db.raw().execute("DELETE FROM snapshots", []).unwrap();
    let execution = lab.execute(run.id).unwrap();
    assert!(execution.warnings[0].contains("no longer exists"));
}

#[test]
fn experiment_reports_are_structured() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("c");
    counter_stream(&store, &id, 30);
    Reconstructor::new(&store, Counter)
        .create_snapshot(&id, at(20), ReconstructOptions::default())
        .unwrap();
    let lab = ReplayLab::new(&store, Counter);
    let report = lab
        .experiment(&id, Target::Latest, SnapshotPolicy::Prefer)
        .unwrap();
    assert!(report.integrity.valid);
    assert_eq!(report.integrity.events_checked, 30);
    let eq = report.equivalence.as_ref().unwrap();
    assert!(eq.equivalent);
    assert!(
        matches!(eq.assisted_start, StartPoint::Snapshot { sequence, .. } if sequence == seqn(20))
    );
    assert_eq!(eq.assisted_events_applied, 10);
    assert_eq!(eq.full_state_digest, eq.assisted_state_digest);
    assert_eq!(report.replay_outcome.as_deref(), Some("exact_match"));
    let run_id = report.run_id.unwrap();
    assert_eq!(
        store.replay_executions(run_id).unwrap()[0].id,
        report.experiment_id.unwrap()
    );
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["snapshot_policy"], json!({"kind": "prefer"}));
    assert!(json["timings"]["replay_micros"].is_u64());

    // With damaged history the experiment stops after verification and says why.
    unlock(&db);
    db.raw()
        .execute(
            "UPDATE events SET payload = '{\"by\":0}' WHERE sequence = 7",
            [],
        )
        .unwrap();
    let report = lab
        .experiment(&id, Target::Latest, SnapshotPolicy::Prefer)
        .unwrap();
    assert!(!report.integrity.valid);
    assert!(report.run_id.is_none() && report.equivalence.is_none());
    assert_eq!(report.notes.len(), 1);
}
