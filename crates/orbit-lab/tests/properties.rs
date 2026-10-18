//! Property tests: for any counter history, an implementation compared with
//! itself is equivalent, and the lab's state digests equal the core replay
//! trace; the floor-at-zero variant diverges exactly when the value first
//! goes negative.

mod common;

use common::*;
use orbit_core::projection::examples::Counter;
use orbit_core::replay::{ReplayLab, ReplayStart};
use orbit_core::{AppendRequest, Digest, Target};
use proptest::prelude::*;

#[derive(Debug, Clone, Copy)]
enum Op {
    Inc(u8),
    Dec(u8),
    Reset,
}

fn ops() -> impl Strategy<Value = Vec<Op>> {
    prop::collection::vec(
        prop_oneof![
            4 => any::<u8>().prop_map(Op::Inc),
            3 => any::<u8>().prop_map(Op::Dec),
            1 => Just(Op::Reset),
        ],
        0..40,
    )
}

fn request(op: Op) -> AppendRequest {
    match op {
        Op::Inc(n) => inc(u64::from(n)),
        Op::Dec(n) => dec(u64::from(n)),
        Op::Reset => reset(),
    }
}

/// Boundary at which `Counter` and the floor variant first differ, if any.
fn expected_first_divergence(ops: &[Op]) -> Option<u64> {
    let mut value = 0i64;
    for (i, op) in ops.iter().enumerate() {
        match *op {
            Op::Inc(n) => value += i64::from(n),
            Op::Dec(n) => value -= i64::from(n),
            Op::Reset => value = 0,
        }
        if value < 0 {
            return Some(i as u64 + 1);
        }
    }
    None
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    #[test]
    fn self_comparison_is_equivalent_and_matches_core(ops in ops()) {
        let env = Env::new();
        let store = env.store();
        let id = seed(&store, "c", ops.iter().copied().map(request).collect());
        let lab = env.lab();
        let record = lab.create_and_run(&spec("c", "orbit.counter", "orbit.counter"), &store).unwrap();
        prop_assert_eq!(record.outcome.as_deref(), Some("equivalent"));
        prop_assert_eq!(record.reproducibility.as_deref(), Some("reproducible"));
        let run = ReplayLab::new(&store, Counter).record(&id, ReplayStart::Initial, Target::Latest).unwrap();
        let trace: Vec<Digest> = lab.artifact(record.id, "left_trace").unwrap()
            .chunks(32).skip(1).map(|c| Digest::from_slice(c).unwrap()).collect();
        prop_assert_eq!(trace, run.trace);
    }

    #[test]
    fn the_floor_variant_diverges_where_the_value_first_goes_negative(ops in ops()) {
        let env = Env::new();
        let store = env.store();
        seed(&store, "c", ops.iter().copied().map(request).collect());
        let lab = env.lab();
        let record = lab
            .create_and_run(&spec("c", "orbit.counter", "example.counter-floor-at-zero"), &store)
            .unwrap();
        let result = record.result.unwrap();
        let expected = expected_first_divergence(&ops);
        prop_assert_eq!(result["first_divergence"]["boundary"].as_u64(), expected);
        prop_assert_eq!(expected.is_none(), record.outcome.as_deref() == Some("equivalent"));
    }
}

fn cycling_ops() -> impl Strategy<Value = Vec<Op>> {
    prop::collection::vec(
        prop_oneof![
            3 => (0u8..12).prop_map(Op::Inc),
            3 => (0u8..12).prop_map(Op::Dec),
            2 => Just(Op::Reset),
        ],
        0..60,
    )
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 32, ..ProptestConfig::default() })]

    /// Every interval, every convergence, every trace boundary and the counts
    /// agree with an independent reference (direct folds, `Value` equality,
    /// naive interval scan), for two different variants and any window.
    #[test]
    fn the_timeline_matches_an_independent_reference(
        ops in cycling_ops(),
        capped in any::<bool>(),
        window in 0usize..8,
    ) {
        let env = Env::new();
        let store = env.store();
        let id = seed(&store, "c", ops.iter().copied().map(request).collect());
        let events = store.read_stream(&id).unwrap();
        let from = window.min(events.len());
        let lab = env.lab_with(sprint8_catalog());
        let right_id = if capped { "test.capped-at-ten" } else { "example.counter-floor-at-zero" };
        let mut s = spec("c", "orbit.counter", right_id);
        s.from = from as u64;
        let record = lab.create_and_run(&s, &store).unwrap();
        let r = record.result.as_ref().unwrap();

        let left = reference_states(&Counter, &events);
        let right = if capped {
            reference_states(&CappedAtTen, &events)
        } else {
            reference_states(&orbit_lab::variants::CounterFloorAtZero, &events)
        };
        let expected = reference_intervals(&left, &right, from);
        let got: Vec<(u64, u64, Option<u64>)> = r["timeline"]["intervals"].as_array().unwrap().iter()
            .map(|i| (i["first"].as_u64().unwrap(), i["last"].as_u64().unwrap(), i["converged_at"].as_u64()))
            .collect();
        prop_assert_eq!(&got, &expected);
        let differing = (from..left.len()).filter(|b| left[*b] != right[*b]).count() as u64;
        prop_assert_eq!(r["boundaries_differing"].as_u64(), Some(differing));
        prop_assert_eq!(r["boundaries_compared"].as_u64(), Some((left.len() - from) as u64));
        prop_assert_eq!(r["timeline"]["convergences"].as_u64(), Some(expected.iter().filter(|i| i.2.is_some()).count() as u64));
        prop_assert_eq!(r["first_divergence"]["boundary"].as_u64(), expected.first().map(|i| i.0));
        let outcome = match (expected.is_empty(), left.last() == right.last()) {
            (true, _) => "equivalent",
            (false, true) => "reconverged",
            (false, false) => "diverged",
        };
        prop_assert_eq!(record.outcome.as_deref(), Some(outcome));
        let page = lab.trace(record.id, None, 1_000).unwrap();
        prop_assert_eq!(page.entries.len(), left.len() - from);
        for e in &page.entries {
            prop_assert_eq!(e.equal, left[e.boundary as usize] == right[e.boundary as usize]);
        }
        // Entry diffs name the only path, with the reference values.
        for i in r["timeline"]["intervals"].as_array().unwrap() {
            let diagnosed = i["index"].as_u64().unwrap() <= orbit_lab::differential::MAX_DIAGNOSED_INTERVALS as u64;
            prop_assert_eq!(i["entry"].is_object(), diagnosed);
            if diagnosed {
                let b = i["first"].as_u64().unwrap() as usize;
                prop_assert_eq!(&i["entry"]["changes"], &serde_json::json!([{
                    "path": "/value", "op": "modified", "from": left[b]["value"], "to": right[b]["value"]
                }]));
            }
        }
        prop_assert_eq!(lab.reproduce(record.id, &store).unwrap().status, "reproduced");
    }
}
