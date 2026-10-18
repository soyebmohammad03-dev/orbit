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
