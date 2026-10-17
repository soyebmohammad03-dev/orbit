//! Bounded concurrency stress tests against real SQLite files, checking
//! invariants rather than success alone.
//!
//! * `mixed_workload_*`: threads in one process, each with its own
//!   connection: blind writers, optimistic writers with retry, readers and
//!   a snapshotter, all on the same file at once.
//! * `optimistic_writers_in_separate_processes_*`: true process concurrency
//!   (child processes), coordinated only by SQLite's file locking.

mod common;

use std::collections::{BTreeSet, HashMap};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use common::{TestDb, meta, seqn, sid, ty};
use orbit_core::projection::examples::Counter;
use orbit_core::{
    AppendRequest, Digest, EventStore, Metadata, ReconstructOptions, Reconstructor, Sequence,
    SnapshotPolicy, StoreConfig, StoreError, StreamId, Target,
};
use serde_json::json;

const STREAMS: usize = 3;

fn tagged(writer: &str, n: u64) -> AppendRequest {
    AppendRequest::new(ty("counter.incremented"), json!({ "by": n % 7 }))
        .with_metadata(meta(json!({ "writer": writer, "n": n })))
}

fn open(db: &TestDb) -> EventStore {
    EventStore::open(db.config().with_busy_timeout(Duration::from_secs(60))).unwrap()
}

fn stream(i: usize) -> StreamId {
    sid(&format!("stress-{i}"))
}

#[test]
fn mixed_workload_preserves_every_invariant() {
    const BLIND_WRITERS: usize = 3;
    const OCC_WRITERS: usize = 2;
    const APPENDS: u64 = 50;
    let db = TestDb::new();
    {
        let store = open(&db);
        for i in 0..STREAMS {
            store.create_stream(&stream(i), Metadata::new()).unwrap();
        }
    }
    let done = Arc::new(AtomicBool::new(false));
    let start = Arc::new(Barrier::new(BLIND_WRITERS + OCC_WRITERS + 3));

    // Blind writers: ExpectedSequence::Any, round-robin over streams.
    let blind: Vec<_> = (0..BLIND_WRITERS)
        .map(|w| {
            let (store, start) = (open(&db), Arc::clone(&start));
            thread::spawn(move || {
                start.wait();
                let name = format!("blind-{w}");
                (0..APPENDS)
                    .map(|n| {
                        let target = stream((w + n as usize) % STREAMS);
                        let e = store.append(&target, tagged(&name, n)).unwrap();
                        (name.clone(), n, target, e.sequence)
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect();

    // Optimistic writers: read head, append with Exact(head), retry on conflict.
    let occ: Vec<_> = (0..OCC_WRITERS)
        .map(|w| {
            let (store, start) = (open(&db), Arc::clone(&start));
            thread::spawn(move || {
                start.wait();
                let name = format!("occ-{w}");
                let target = stream(0);
                let (mut attempts, mut conflicts, mut written) = (0u64, 0u64, Vec::new());
                for n in 0..APPENDS {
                    loop {
                        attempts += 1;
                        let head = store.head(&target).unwrap().sequence;
                        match store.append(&target, tagged(&name, n).expect_sequence(head)) {
                            Ok(e) => {
                                assert_eq!(
                                    e.sequence,
                                    head.next().unwrap(),
                                    "OCC append must land at head + 1"
                                );
                                written.push((name.clone(), n, target.clone(), e.sequence));
                                break;
                            }
                            Err(StoreError::SequenceConflict {
                                expected, actual, ..
                            }) => {
                                assert_eq!(expected, head);
                                assert!(actual > expected, "conflict must report a newer head");
                                conflicts += 1;
                            }
                            Err(other) => panic!("unexpected {other}"),
                        }
                    }
                }
                (written, attempts, conflicts)
            })
        })
        .collect();

    // Readers: heads never move backwards; an event, once seen, never changes.
    let readers: Vec<_> = (0..2)
        .map(|_| {
            let (store, start, done) = (open(&db), Arc::clone(&start), Arc::clone(&done));
            thread::spawn(move || {
                start.wait();
                let mut seen: HashMap<(usize, Sequence), Digest> = HashMap::new();
                let mut heads = [Sequence::ZERO; STREAMS];
                let mut rounds = 0u64;
                while !done.load(Ordering::SeqCst) || rounds == 0 {
                    for (i, last) in heads.iter_mut().enumerate() {
                        let head = store.head(&stream(i)).unwrap().sequence;
                        assert!(head >= *last, "head of stream {i} moved backwards");
                        *last = head;
                        let mut after = Sequence::ZERO;
                        loop {
                            let page = store.read_after(&stream(i), after, 64).unwrap();
                            let Some(tail) = page.last() else { break };
                            after = tail.sequence;
                            for e in page {
                                if let Some(previous) = seen.insert((i, e.sequence), e.digest) {
                                    assert_eq!(
                                        previous, e.digest,
                                        "event {i}/{} changed",
                                        e.sequence
                                    );
                                }
                            }
                        }
                    }
                    rounds += 1;
                }
                rounds
            })
        })
        .collect();

    // Snapshotter: repeatedly snapshots stream 0 at its current head.
    let snapshotter = {
        let (store, start, done) = (open(&db), Arc::clone(&start), Arc::clone(&done));
        thread::spawn(move || {
            start.wait();
            let counter = Reconstructor::new(&store, Counter);
            let mut taken = Vec::new();
            while !done.load(Ordering::SeqCst) || taken.is_empty() {
                if store.head(&stream(0)).unwrap().is_empty() {
                    continue;
                }
                let s = counter
                    .create_snapshot(&stream(0), Target::Latest, ReconstructOptions::default())
                    .unwrap();
                taken.push(s.saved.snapshot);
            }
            taken
        })
    };

    let mut expected_tags = BTreeSet::new();
    let mut placed = Vec::new();
    for handle in blind {
        for (writer, n, target, sequence) in handle.join().unwrap() {
            assert!(expected_tags.insert((writer, n)));
            placed.push((target, sequence));
        }
    }
    let (mut total_attempts, mut total_conflicts) = (0, 0);
    for handle in occ {
        let (written, attempts, conflicts) = handle.join().unwrap();
        assert_eq!(
            attempts,
            APPENDS + conflicts,
            "every attempt is a success or a reported conflict"
        );
        total_attempts += attempts;
        total_conflicts += conflicts;
        for (writer, n, target, sequence) in written {
            assert!(expected_tags.insert((writer, n)));
            placed.push((target, sequence));
        }
    }
    done.store(true, Ordering::SeqCst);
    for reader in readers {
        assert!(reader.join().unwrap() > 0);
    }
    let snapshots = snapshotter.join().unwrap();
    assert!(total_attempts >= (OCC_WRITERS as u64) * APPENDS);
    let _ = total_conflicts; // contention-dependent; correctness is asserted per attempt above

    // Reopen and check the committed history as a whole.
    let store = open(&db);
    let mut found_tags = BTreeSet::new();
    let mut event_ids = BTreeSet::new();
    let mut total = 0;
    for i in 0..STREAMS {
        let events = store.read_stream(&stream(i)).unwrap();
        for (k, e) in events.iter().enumerate() {
            assert_eq!(e.sequence.get(), k as u64 + 1, "stream {i} is not dense");
            assert!(event_ids.insert(e.event_id), "duplicate event id");
            let tag = (
                e.metadata["writer"].as_str().unwrap().to_owned(),
                e.metadata["n"].as_u64().unwrap(),
            );
            assert!(found_tags.insert(tag), "an append was stored twice");
            assert!(
                placed.contains(&(stream(i), e.sequence)),
                "event not reported by any writer"
            );
        }
        total += events.len();
    }
    assert_eq!(
        found_tags, expected_tags,
        "every acknowledged append is stored exactly once"
    );
    assert_eq!(total, placed.len());
    let report = store.verify_store().unwrap();
    assert!(report.is_valid(), "{report:#?}");

    // Every snapshot describes exactly the position it claims.
    let counter = Reconstructor::new(&store, Counter);
    for snapshot in &snapshots {
        let full = counter
            .reconstruct(
                &stream(0),
                Target::At(snapshot.sequence),
                ReconstructOptions::with_snapshots(SnapshotPolicy::Ignore),
            )
            .unwrap();
        assert_eq!(snapshot.state, serde_json::to_value(full.state).unwrap());
        assert_eq!(
            snapshot.event_digest,
            store.event(&stream(0), snapshot.sequence).unwrap().digest
        );
    }
}

const CHILD_ENV: &str = "ORBIT_STRESS_CHILD";

/// Child-process entry point for the multi-process test; a no-op otherwise.
#[test]
fn stress_child_entrypoint() {
    let Ok(spec) = std::env::var(CHILD_ENV) else {
        return;
    };
    let mut parts = spec.split('|');
    let (name, path, count) = (
        parts.next().unwrap().to_owned(),
        parts.next().unwrap().to_owned(),
        parts.next().unwrap().parse::<u64>().unwrap(),
    );
    let store = EventStore::open(StoreConfig::new(path).with_busy_timeout(Duration::from_secs(60)))
        .unwrap();
    let target = stream(0);
    let mut conflicts = 0;
    for n in 0..count {
        loop {
            let head = store.head(&target).unwrap().sequence;
            match store.append(&target, tagged(&name, n).expect_sequence(head)) {
                Ok(_) => break,
                Err(StoreError::SequenceConflict { .. }) => conflicts += 1,
                Err(other) => panic!("{other}"),
            }
        }
    }
    println!("conflicts {conflicts}");
}

#[test]
fn optimistic_writers_in_separate_processes_never_duplicate_or_lose_events() {
    const PROCESSES: usize = 3;
    const PER_PROCESS: u64 = 40;
    let db = TestDb::new();
    open(&db)
        .create_stream(&stream(0), Metadata::new())
        .unwrap();

    let children: Vec<_> = (0..PROCESSES)
        .map(|p| {
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "stress_child_entrypoint",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(
                    CHILD_ENV,
                    format!("proc-{p}|{}|{PER_PROCESS}", db.path.display()),
                )
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    let mut conflicts = 0;
    for mut child in children {
        let stdout = child.stdout.take().unwrap();
        for line in BufReader::new(stdout).lines() {
            if let Some(n) = line.unwrap().strip_prefix("conflicts ") {
                conflicts += n.parse::<u64>().unwrap();
            }
        }
        assert!(child.wait().unwrap().success());
    }

    let store = open(&db);
    let events = store.read_stream(&stream(0)).unwrap();
    assert_eq!(events.len() as u64, PROCESSES as u64 * PER_PROCESS);
    let tags: BTreeSet<(String, u64)> = events
        .iter()
        .map(|e| {
            (
                e.metadata["writer"].as_str().unwrap().to_owned(),
                e.metadata["n"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(tags.len(), events.len(), "no append stored twice");
    for (i, e) in events.iter().enumerate() {
        assert_eq!(e.sequence, seqn(i as u64 + 1));
    }
    // Each process's own appends are in its program order.
    for p in 0..PROCESSES {
        let mine: Vec<u64> = events
            .iter()
            .filter(|e| e.metadata["writer"] == format!("proc-{p}"))
            .map(|e| e.metadata["n"].as_u64().unwrap())
            .collect();
        assert_eq!(mine, (0..PER_PROCESS).collect::<Vec<_>>());
    }
    assert!(store.verify_store().unwrap().is_valid());
    let _ = conflicts; // observed contention; zero is legal if the scheduler serialised them
}
