//! The measured workloads.
//!
//! Each workload prepares its own files in the experiment's work directory
//! (never a user's store), then runs iterations. Only [`Workload::measure`]
//! is timed. After every iteration, untimed, [`Workload::check`] confirms
//! that the operation produced the expected result: an iteration that did
//! the wrong work is an invalid measurement and fails the experiment
//! instead of producing a number.
//!
//! Inputs are generated from the spec's `seed` with a fixed xorshift
//! generator, so equal specs measure equal inputs.

use std::hint::black_box;
use std::path::Path;

use orbit_core::projection::examples::Counter;
use orbit_core::{
    AppendRequest, Durability, EventStore, EventType, Metadata, ReconstructOptions, Reconstructor,
    SnapshotPolicy, StoreConfig, StreamId, Target,
};
use serde_json::{Value, json};

use crate::catalog::Catalog;
use crate::differential;
use crate::lab::Lab;
use crate::spec::DifferentialSpec;

/// Deterministic xorshift64* generator.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    /// A generator for `seed` (0 is mapped to a fixed non-zero state).
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    /// Next value.
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// A workload instance, prepared and ready to be iterated.
pub trait Workload {
    /// Untimed preparation before iteration `i`.
    ///
    /// # Errors
    /// A description of what went wrong.
    fn before(&mut self, _i: u64) -> Result<(), String> {
        Ok(())
    }
    /// The timed operation of iteration `i`.
    ///
    /// # Errors
    /// A description of what went wrong.
    fn measure(&mut self, i: u64) -> Result<(), String>;
    /// Untimed validation of iteration `i`.
    ///
    /// # Errors
    /// A description of the wrong result.
    fn check(&mut self, i: u64) -> Result<(), String>;
}

fn store(dir: &Path) -> Result<EventStore, String> {
    EventStore::open(StoreConfig::new(dir.join("bench.db")).with_durability(Durability::Normal))
        .map_err(|e| e.to_string())
}

fn sid(name: &str) -> Result<StreamId, String> {
    StreamId::new(name).map_err(|e| e.to_string())
}

/// Counter events (the projection used by the reconstruction workloads).
fn counter_requests(n: u64, seed: u64) -> Result<Vec<AppendRequest>, String> {
    let mut rng = Rng::new(seed);
    let inc = EventType::new("counter.incremented").map_err(|e| e.to_string())?;
    let dec = EventType::new("counter.decremented").map_err(|e| e.to_string())?;
    Ok((0..n)
        .map(|i| {
            let by = rng.next_u64() % 9 + 1;
            if i % 5 == 4 {
                AppendRequest::new(dec.clone(), json!({ "by": by }))
            } else {
                AppendRequest::new(inc.clone(), json!({ "by": by }))
            }
        })
        .collect())
}

fn seeded(dir: &Path, n: u64, seed: u64) -> Result<(EventStore, StreamId), String> {
    let store = store(dir)?;
    let stream = sid("bench")?;
    store
        .create_stream(&stream, Metadata::new())
        .map_err(|e| e.to_string())?;
    for request in counter_requests(n, seed)? {
        store.append(&stream, request).map_err(|e| e.to_string())?;
    }
    Ok((store, stream))
}

/// Appends `n` ledger events to a new stream per iteration.
struct Append {
    store: EventStore,
    n: u64,
    rng: Rng,
    ty: EventType,
    stream: Option<StreamId>,
}

impl Workload for Append {
    fn before(&mut self, i: u64) -> Result<(), String> {
        let stream = sid(&format!("append-{i}"))?;
        self.store
            .create_stream(&stream, Metadata::new())
            .map_err(|e| e.to_string())?;
        self.stream = Some(stream);
        Ok(())
    }
    fn measure(&mut self, _i: u64) -> Result<(), String> {
        let stream = self.stream.as_ref().ok_or("no stream")?;
        for k in 0..self.n {
            let amount = self.rng.next_u64() % 100_000;
            let request = AppendRequest::new(
                self.ty.clone(),
                json!({ "account": "acct-000042", "amount_cents": amount, "currency": "EUR", "n": k }),
            );
            black_box(
                self.store
                    .append(stream, request)
                    .map_err(|e| e.to_string())?,
            );
        }
        Ok(())
    }
    fn check(&mut self, _i: u64) -> Result<(), String> {
        let stream = self.stream.as_ref().ok_or("no stream")?;
        let head = self.store.head(stream).map_err(|e| e.to_string())?;
        if head.sequence.get() == self.n {
            Ok(())
        } else {
            Err(format!(
                "expected {} events, found {}",
                self.n, head.sequence
            ))
        }
    }
}

/// Reconstructs the latest state, with or without a snapshot.
struct Reconstruct {
    store: EventStore,
    stream: StreamId,
    policy: SnapshotPolicy,
    expected_state: Value,
    expected_applied: u64,
    last: Option<(Value, u64)>,
}

impl Workload for Reconstruct {
    fn measure(&mut self, _i: u64) -> Result<(), String> {
        let r = Reconstructor::new(&self.store, Counter)
            .reconstruct(
                &self.stream,
                Target::Latest,
                ReconstructOptions::with_snapshots(self.policy),
            )
            .map_err(|e| e.to_string())?;
        let r = black_box(r);
        self.last = Some((
            serde_json::to_value(r.state).map_err(|e| e.to_string())?,
            r.report.events_applied,
        ));
        Ok(())
    }
    fn check(&mut self, _i: u64) -> Result<(), String> {
        match self.last.take() {
            Some((state, applied))
                if state == self.expected_state && applied == self.expected_applied =>
            {
                Ok(())
            }
            other => Err(format!(
                "reconstruction returned {other:?}, expected {} after applying {}",
                self.expected_state, self.expected_applied
            )),
        }
    }
}

/// Verifies the whole stream.
struct Verify {
    store: EventStore,
    stream: StreamId,
    valid: Option<(bool, u64)>,
    n: u64,
}

impl Workload for Verify {
    fn measure(&mut self, _i: u64) -> Result<(), String> {
        let report = self
            .store
            .verify_stream(&self.stream)
            .map_err(|e| e.to_string())?;
        let report = black_box(report);
        self.valid = Some((report.is_valid(), report.events_checked));
        Ok(())
    }
    fn check(&mut self, _i: u64) -> Result<(), String> {
        match self.valid.take() {
            Some((true, checked)) if checked == self.n => Ok(()),
            other => Err(format!(
                "verification returned {other:?} for {} events",
                self.n
            )),
        }
    }
}

/// One differential replay (no registry writes).
struct Differential {
    store: EventStore,
    catalog: Catalog,
    spec: DifferentialSpec,
    outcome: Option<String>,
    expected: Option<String>,
}

impl Workload for Differential {
    fn measure(&mut self, _i: u64) -> Result<(), String> {
        let input = differential::resolve_input(&self.store, &self.spec).map_err(|f| f.message)?;
        let left = self.catalog.find(&self.spec.left).ok_or("left missing")?;
        let right = self.catalog.find(&self.spec.right).ok_or("right missing")?;
        let run = differential::execute(
            &self.store,
            &self.spec,
            &input,
            left.as_ref(),
            right.as_ref(),
        )
        .map_err(|f| f.message)?;
        let run = black_box(run);
        self.outcome = Some(format!(
            "{} {} {:?}",
            run.result.outcome.label(),
            run.result.boundaries_compared,
            run.result
                .first_divergence
                .as_ref()
                .map(|d| d.comparison.boundary)
        ));
        Ok(())
    }
    fn check(&mut self, _i: u64) -> Result<(), String> {
        let outcome = self.outcome.take().ok_or("no outcome")?;
        match &self.expected {
            None => {
                self.expected = Some(outcome);
                Ok(())
            }
            Some(e) if *e == outcome => Ok(()),
            Some(e) => Err(format!("outcome {outcome} differs from the first, {e}")),
        }
    }
}

/// Creates and runs `n` small experiments per iteration in a scratch lab.
struct Registry {
    lab: Lab,
    store: EventStore,
    spec: DifferentialSpec,
    n: u64,
    completed: u64,
}

impl Workload for Registry {
    fn measure(&mut self, _i: u64) -> Result<(), String> {
        self.completed = 0;
        for _ in 0..self.n {
            let record = self
                .lab
                .create_and_run(&self.spec, &self.store)
                .map_err(|e| e.to_string())?;
            if record.status == crate::Status::Completed {
                self.completed += 1;
            }
        }
        Ok(())
    }
    fn check(&mut self, _i: u64) -> Result<(), String> {
        if self.completed == self.n {
            Ok(())
        } else {
            Err(format!(
                "{} of {} experiments completed",
                self.completed, self.n
            ))
        }
    }
}

/// Writes one artifact of `n` KiB per iteration and reads it back verified.
struct Artifacts {
    store: crate::artifacts::ArtifactStore,
    bytes: Vec<u8>,
    digest: Option<orbit_core::Digest>,
    read_back: bool,
}

impl Workload for Artifacts {
    fn before(&mut self, i: u64) -> Result<(), String> {
        // Different content per iteration: identical content is deduplicated
        // and would not be written again.
        self.bytes[..8].copy_from_slice(&i.to_be_bytes());
        Ok(())
    }
    fn measure(&mut self, _i: u64) -> Result<(), String> {
        let digest = self.store.put(&self.bytes).map_err(|e| e.to_string())?;
        let back = self.store.read(&digest).map_err(|e| e.to_string())?;
        self.read_back = black_box(back).len() == self.bytes.len();
        self.digest = Some(digest);
        Ok(())
    }
    fn check(&mut self, _i: u64) -> Result<(), String> {
        let expected = crate::digest::content_digest(&self.bytes);
        if self.read_back && self.digest == Some(expected) {
            Ok(())
        } else {
            Err("the artifact read back does not match what was written".to_owned())
        }
    }
}

/// Prepares workload `id` with `n` as its input size.
///
/// # Errors
/// An unknown id or a preparation failure.
pub fn prepare(id: &str, dir: &Path, n: u64, seed: u64) -> Result<Box<dyn Workload>, String> {
    Ok(match id {
        "event-append" => Box::new(Append {
            store: store(dir)?,
            n,
            rng: Rng::new(seed),
            ty: EventType::new("ledger.posted").map_err(|e| e.to_string())?,
            stream: None,
        }),
        "historical-reconstruction" | "snapshot-reconstruction" => {
            let (store, stream) = seeded(dir, n, seed)?;
            let full = Reconstructor::new(&store, Counter)
                .reconstruct(
                    &stream,
                    Target::Latest,
                    ReconstructOptions::with_snapshots(SnapshotPolicy::Ignore),
                )
                .map_err(|e| e.to_string())?;
            let expected_state = serde_json::to_value(full.state).map_err(|e| e.to_string())?;
            let (policy, expected_applied) = if id == "snapshot-reconstruction" {
                // A snapshot 90 % of the way: the last tenth is replayed.
                let at = (n - n / 10).max(1);
                let sequence = orbit_core::Sequence::new(at).map_err(|e| e.to_string())?;
                Reconstructor::new(&store, Counter)
                    .create_snapshot(&stream, Target::At(sequence), ReconstructOptions::default())
                    .map_err(|e| e.to_string())?;
                (SnapshotPolicy::Strict, n - at)
            } else {
                (SnapshotPolicy::Ignore, n)
            };
            Box::new(Reconstruct {
                store,
                stream,
                policy,
                expected_state,
                expected_applied,
                last: None,
            })
        }
        "integrity-verification" => {
            let (store, stream) = seeded(dir, n, seed)?;
            Box::new(Verify {
                store,
                stream,
                valid: None,
                n,
            })
        }
        "differential-replay" => {
            let (store, _) = seeded(dir, n, seed)?;
            let spec = DifferentialSpec::from_json(json!({
                "stream": "bench",
                "left": "orbit.counter",
                "right": "example.counter-floor-at-zero",
                "determinism_check": false,
            }))
            .map_err(|e| e.to_string())?;
            Box::new(Differential {
                store,
                catalog: Catalog::builtin(),
                spec,
                outcome: None,
                expected: None,
            })
        }
        "registry-operations" => {
            let (store, _) = seeded(dir, 10, seed)?;
            let lab = Lab::open(&dir.join("lab"), Catalog::builtin()).map_err(|e| e.to_string())?;
            let spec = DifferentialSpec::from_json(json!({
                "stream": "bench",
                "left": "orbit.counter",
                "right": "example.counter-floor-at-zero",
            }))
            .map_err(|e| e.to_string())?;
            Box::new(Registry {
                lab,
                store,
                spec,
                n,
                completed: 0,
            })
        }
        "artifact-persistence" => {
            let mut rng = Rng::new(seed);
            let len = usize::try_from(n * 1024).map_err(|e| e.to_string())?;
            let mut bytes = Vec::with_capacity(len);
            while bytes.len() < len {
                bytes.extend_from_slice(&rng.next_u64().to_le_bytes());
            }
            bytes.truncate(len.max(8));
            Box::new(Artifacts {
                store: crate::artifacts::ArtifactStore::open(&dir.join("artifacts"))
                    .map_err(|e| e.to_string())?,
                bytes,
                digest: None,
                read_back: false,
            })
        }
        other => return Err(format!("workload {other} has no implementation")),
    })
}

#[cfg(test)]
mod tests {
    use super::Rng;

    #[test]
    fn the_generator_is_deterministic_and_seed_dependent() {
        let a: Vec<u64> = std::iter::repeat_with({
            let mut r = Rng::new(7);
            move || r.next_u64()
        })
        .take(4)
        .collect();
        let b: Vec<u64> = std::iter::repeat_with({
            let mut r = Rng::new(7);
            move || r.next_u64()
        })
        .take(4)
        .collect();
        let mut other = Rng::new(8);
        assert_eq!(a, b);
        assert_ne!(a[0], other.next_u64());
        assert_ne!(Rng::new(0).next_u64(), 0);
    }
}
