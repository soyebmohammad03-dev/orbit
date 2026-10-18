//! Child-process roles of the reliability scenarios.
//!
//! The runner starts `orbit-reliability __child '<json>'`; [`main`] reads the
//! role from that argument (never from the environment) and performs it.
//! Children talk to the parent by lines:
//!
//! - stdout: `ready` before a barrier, then results (`committed N`,
//!   `paused`, `done <status>`, `won`, `lost <reason>`, or one JSON line);
//! - stdin: one line releases a barrier or a pause.
//!
//! A child never chooses its own files: the parent passes paths inside the
//! scenario's work directory.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;

use orbit_core::fault::FaultPoint;
use orbit_core::projection::examples::Counter;
use orbit_core::{
    AppendRequest, Durability, EventStore, EventType, ReconstructOptions, Reconstructor,
    StoreConfig, StreamId, Target,
};
use serde::Deserialize;
use serde_json::json;

use crate::fault::LabFaultPoint;
use crate::{Catalog, DifferentialSpec, ExperimentId, Lab};

/// A child role, as passed on the command line.
#[derive(Debug, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case", deny_unknown_fields)]
pub enum Role {
    /// Abort inside one append at `point`.
    AppendAbort {
        /// Store file.
        db: PathBuf,
        /// Core fault point (Debug name).
        point: String,
    },
    /// Append forever, printing `committed N` after each commit.
    AppendForever {
        /// Store file.
        db: PathBuf,
    },
    /// Abort inside one snapshot save at `point`.
    SnapshotAbort {
        /// Store file.
        db: PathBuf,
        /// Core fault point (Debug name).
        point: String,
    },
    /// Run one created experiment, aborting or pausing at `point`.
    LabRun {
        /// Store file.
        db: PathBuf,
        /// Lab directory.
        lab: PathBuf,
        /// Experiment.
        experiment: String,
        /// Lab fault point.
        point: String,
        /// `abort` or `pause`.
        action: String,
    },
    /// After the barrier, create and run `count` experiments.
    RegistryWorker {
        /// Store file.
        db: PathBuf,
        /// Lab directory.
        lab: PathBuf,
        /// Stream.
        stream: String,
        /// How many.
        count: u64,
    },
    /// After the barrier, try to start one experiment.
    StartRace {
        /// Lab directory.
        lab: PathBuf,
        /// Experiment.
        experiment: String,
    },
    /// Create one experiment, print `created <id>`, then wait (until killed
    /// or released by a line on stdin).
    CreateAndWait {
        /// Lab directory.
        lab: PathBuf,
    },
    /// After the barrier, recover interrupted experiments.
    RecoverRace {
        /// Lab directory.
        lab: PathBuf,
    },
}

/// The stream every scenario uses.
pub const STREAM: &str = "reliability";

/// Writes one line to the parent. If the parent is gone (its end of the
/// pipe is closed), this child exits instead of running on unobserved:
/// otherwise a continuously appending child would outlive a killed runner.
fn say(line: &str) {
    let mut out = std::io::stdout().lock();
    if writeln!(out, "{line}").and_then(|()| out.flush()).is_err() {
        std::process::exit(4);
    }
}

fn wait_for_parent() {
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
}

fn store(db: &PathBuf) -> EventStore {
    EventStore::open(StoreConfig::new(db).with_durability(Durability::Full))
        .unwrap_or_else(|e| fail(&format!("cannot open store: {e}")))
}

fn fail(message: &str) -> ! {
    eprintln!("child error: {message}");
    std::process::exit(3);
}

fn core_point(name: &str) -> FaultPoint {
    FaultPoint::ALL
        .into_iter()
        .find(|p| p.to_string() == name)
        .unwrap_or_else(|| fail(&format!("unknown fault point {name}")))
}

fn abort_at(point: FaultPoint) -> orbit_core::fault::FaultHook {
    Arc::new(move |p| {
        if p == point {
            std::process::abort();
        }
        false
    })
}

/// Runs a role. `arg` is the JSON role description.
pub fn main(arg: &str) -> ! {
    let role: Role = serde_json::from_str(arg).unwrap_or_else(|e| fail(&format!("bad role: {e}")));
    let stream = StreamId::new(STREAM).unwrap_or_else(|e| fail(&e.to_string()));
    match role {
        Role::AppendAbort { db, point } => {
            let store = store(&db);
            store.set_fault_hook(Some(abort_at(core_point(&point))));
            let ty = EventType::new("counter.incremented").unwrap_or_else(|e| fail(&e.to_string()));
            match store.append(&stream, AppendRequest::new(ty, json!({ "by": 1 }))) {
                Ok(e) => say(&format!("not-faulted committed {}", e.sequence)),
                Err(e) => say(&format!("not-faulted error {e}")),
            }
        }
        Role::AppendForever { db } => {
            let store = store(&db);
            let ty = EventType::new("counter.incremented").unwrap_or_else(|e| fail(&e.to_string()));
            loop {
                match store.append(&stream, AppendRequest::new(ty.clone(), json!({ "by": 1 }))) {
                    // Printed only after append returned, i.e. after COMMIT.
                    Ok(e) => say(&format!("committed {}", e.sequence)),
                    Err(e) => fail(&e.to_string()),
                }
            }
        }
        Role::SnapshotAbort { db, point } => {
            let store = store(&db);
            store.set_fault_hook(Some(abort_at(core_point(&point))));
            let result = Reconstructor::new(&store, Counter).create_snapshot(
                &stream,
                Target::Latest,
                ReconstructOptions::default(),
            );
            say(&format!("not-faulted {}", result.is_ok()));
        }
        Role::LabRun {
            db,
            lab,
            experiment,
            point,
            action,
        } => {
            let store = store(&db);
            // No recovery here: the parent decides when recovery happens.
            let lab = Lab::open_without_recovery(&lab, Catalog::builtin())
                .unwrap_or_else(|e| fail(&e.to_string()));
            let id: ExperimentId = experiment
                .parse()
                .unwrap_or_else(|_| fail("bad experiment id"));
            let point = LabFaultPoint::parse(&point).unwrap_or_else(|| fail("bad lab fault point"));
            let pause = action == "pause";
            lab.set_fault_hook(Some(Arc::new(move |p| {
                if p == point {
                    if pause {
                        say("paused");
                        wait_for_parent();
                    } else {
                        std::process::abort();
                    }
                }
                false
            })));
            match lab.run(id, &store) {
                Ok(record) => say(&format!("done {}", record.status)),
                Err(e) => say(&format!("error {e}")),
            }
        }
        Role::RegistryWorker {
            db,
            lab,
            stream,
            count,
        } => {
            let store = store(&db);
            let lab = Lab::open_without_recovery(&lab, Catalog::builtin())
                .unwrap_or_else(|e| fail(&e.to_string()));
            let spec = DifferentialSpec::from_json(json!({
                "stream": stream,
                "left": "orbit.counter",
                "right": "example.counter-floor-at-zero",
            }))
            .unwrap_or_else(|e| fail(&e.to_string()));
            say("ready");
            wait_for_parent();
            let mut ids = Vec::new();
            let mut errors = Vec::new();
            for _ in 0..count {
                match lab.create_and_run(&spec, &store) {
                    Ok(record) => ids.push(record.id.to_string()),
                    Err(e) => errors.push(e.to_string()),
                }
            }
            say(&json!({ "ids": ids, "errors": errors }).to_string());
        }
        Role::StartRace { lab, experiment } => {
            let lab = Lab::open_without_recovery(&lab, Catalog::builtin())
                .unwrap_or_else(|e| fail(&e.to_string()));
            let id: ExperimentId = experiment
                .parse()
                .unwrap_or_else(|_| fail("bad experiment id"));
            say("ready");
            wait_for_parent();
            match lab.registry().start(id) {
                Ok(()) => say("won"),
                Err(crate::LabError::InvalidTransition { .. }) => say("lost invalid_transition"),
                Err(e) => say(&format!("lost {e}")),
            }
        }
        Role::CreateAndWait { lab } => {
            let lab = Lab::open_without_recovery(&lab, Catalog::builtin())
                .unwrap_or_else(|e| fail(&e.to_string()));
            let spec = DifferentialSpec::from_json(json!({
                "stream": STREAM,
                "left": "orbit.counter",
                "right": "example.counter-floor-at-zero",
            }))
            .unwrap_or_else(|e| fail(&e.to_string()));
            let record = lab.create(&spec).unwrap_or_else(|e| fail(&e.to_string()));
            say(&format!("created {}", record.id));
            wait_for_parent();
        }
        Role::RecoverRace { lab } => {
            let lab = Lab::open_without_recovery(&lab, Catalog::builtin())
                .unwrap_or_else(|e| fail(&e.to_string()));
            say("ready");
            wait_for_parent();
            match lab.recover_interrupted() {
                Ok(ids) => say(&json!({ "recovered": ids }).to_string()),
                Err(e) => say(&json!({ "error": e.to_string() }).to_string()),
            }
        }
    }
    std::process::exit(0);
}
