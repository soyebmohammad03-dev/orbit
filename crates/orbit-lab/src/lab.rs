//! [`Lab`]: the registry, the artifact store and the catalog together.
//!
//! # Directory layout
//!
//! ```text
//! <lab dir>/lab.db                       registry (SQLite, WAL)
//! <lab dir>/artifacts/sha256/ab/cdef…    artifact bytes, named by SHA-256
//! <lab dir>/artifacts/tmp/               in-progress artifact writes
//! <lab dir>/locks/<experiment id>.lock   held while an experiment runs
//! <lab dir>/locks/<experiment id>.create held by its creator until it starts
//! ```
//!
//! # Interrupted experiments
//!
//! A running experiment holds an exclusive OS file lock on its lock file
//! (`std::fs::File::try_lock`). The OS releases the lock when the process
//! exits, however it exits. [`Lab::open`] (and
//! [`Lab::recover_interrupted`]) looks at every `running` experiment: if
//! its lock can be taken, no process is executing it any more, and it is
//! marked `failed` with kind `interrupted`. An experiment still running in
//! another process keeps its lock and is left alone.
//!
//! # Abandoned experiments
//!
//! [`Lab::create`] (and every other `create_*`) takes an exclusive lock on
//! `locks/<id>.create` *before* the registry row exists, and keeps it until
//! the experiment starts. If the creating process exits first (or drops its
//! `Lab` without running it), the lock is free while the file remains:
//! [`Lab::recover_abandoned`] (called by [`Lab::open`]) then marks the
//! experiment `failed` with kind `abandoned`. It only ever changes an
//! experiment that is still `created`, so a started experiment is never
//! marked abandoned. To let another process run a created experiment, call
//! [`Lab::release`] first: it deletes the file, after which the experiment
//! is unowned and never abandoned automatically. Experiments created by
//! versions without creation locks have no file and are also left alone.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use orbit_core::{Digest, EventStore};
use serde::Serialize;
use serde_json::{Value, json};

use crate::artifacts::{ArtifactState, ArtifactStore};
use crate::catalog::{Catalog, Implementation};
use crate::differential::{self, RunFailure};
use crate::digest::{RESULT_TAG, value_digest};
use crate::error::{LabError, Result};
use crate::identity::BuildInfo;
use crate::registry::{
    ArtifactRef, Completion, ExperimentId, ExperimentRecord, ExperimentSummary, Failure,
    FailureKind, InputRange, NewExperiment, Registry, Status, VerificationKind, VerificationRecord,
};
use crate::reproduction::ReproductionStatus;
use crate::spec::{DifferentialSpec, Side};

/// File name of the registry inside the lab directory.
pub const REGISTRY_FILE: &str = "lab.db";

/// One artifact's verification result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtifactCheck {
    /// The artifact.
    #[serde(flatten)]
    pub artifact: ArtifactRef,
    /// What was found.
    pub state: ArtifactState,
}

/// Outcome of [`Lab::verify_artifacts`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtifactVerification {
    /// `intact` when every check passed, otherwise `damaged`.
    pub status: String,
    /// The stored specification still matches its digest.
    pub spec_digest_ok: bool,
    /// The stored result still matches its digest (`None` without a result).
    pub result_digest_ok: Option<bool>,
    /// Each trace artifact holds exactly 32 bytes per compared boundary.
    pub traces_consistent: Option<bool>,
    /// Each artifact.
    pub artifacts: Vec<ArtifactCheck>,
}

/// Largest page of [`Lab::trace`].
pub const MAX_TRACE_PAGE: usize = 1_000;

/// One boundary of a trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TraceEntry {
    /// Boundary.
    pub boundary: u64,
    /// Left state digest.
    pub left: Digest,
    /// Right state digest.
    pub right: Digest,
    /// Whether they are equal.
    pub equal: bool,
}

/// A page of [`Lab::trace`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TracePage {
    /// Experiment.
    pub experiment_id: ExperimentId,
    /// First compared boundary of the experiment.
    pub first_boundary: u64,
    /// Last compared boundary, if any.
    pub last_boundary: Option<u64>,
    /// Number of compared boundaries.
    pub boundaries_total: u64,
    /// Entries of this page, in boundary order.
    pub entries: Vec<TraceEntry>,
    /// Pass as `from` to get the next page; absent on the last page.
    pub next_from: Option<u64>,
}

/// The lab: registry, artifacts and catalog.
#[derive(Debug)]
pub struct Lab {
    registry: Registry,
    artifacts: ArtifactStore,
    catalog: Catalog,
    dir: PathBuf,
    locks: PathBuf,
    /// Creation locks held for experiments created through this `Lab` and
    /// not yet started or released.
    owned: Mutex<HashMap<ExperimentId, File>>,
}

fn json_of<T: Serialize>(value: &T) -> Result<Value> {
    Ok(serde_json::to_value(value)?)
}

impl Lab {
    /// Opens (creating if needed) the lab in `dir`, then marks interrupted
    /// experiments as failed.
    ///
    /// # Errors
    /// I/O or registry errors.
    pub fn open(dir: &Path, catalog: Catalog) -> Result<Self> {
        let lab = Self::open_without_recovery(dir, catalog)?;
        let recovered = lab.recover_interrupted()?;
        if !recovered.is_empty() {
            tracing::warn!(
                count = recovered.len(),
                "marked interrupted experiments as failed"
            );
        }
        let abandoned = lab.recover_abandoned()?;
        if !abandoned.is_empty() {
            tracing::warn!(
                count = abandoned.len(),
                "marked abandoned experiments as failed"
            );
        }
        let removed = lab.artifacts.remove_stale_temporaries()?;
        if removed > 0 {
            tracing::warn!(
                removed,
                "removed temporary files of interrupted artifact writes"
            );
        }
        Ok(lab)
    }

    /// Opens the lab without recovering interrupted experiments or removing
    /// stale temporary files. For tools and tests that need to observe or
    /// trigger recovery themselves; ordinary callers use [`open`](Self::open).
    ///
    /// # Errors
    /// I/O or registry errors.
    pub fn open_without_recovery(dir: &Path, catalog: Catalog) -> Result<Self> {
        fs::create_dir_all(dir)?;
        let locks = dir.join("locks");
        fs::create_dir_all(&locks)?;
        // Hash the executable in the background, so the first experiment
        // usually does not wait for it (`BuildInfo::current` reuses it).
        std::thread::spawn(crate::identity::executable_digest);
        Ok(Self {
            registry: Registry::open(&dir.join(REGISTRY_FILE))?,
            artifacts: ArtifactStore::open(&dir.join("artifacts"))?,
            catalog,
            dir: dir.to_path_buf(),
            locks,
            owned: Mutex::new(HashMap::new()),
        })
    }

    /// The lab directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Installs (or with `None` removes) the lab's fault hook. See [`crate::fault`].
    #[cfg(feature = "reliability")]
    pub fn set_fault_hook(&self, hook: Option<crate::fault::LabFaultHook>) {
        *self
            .artifacts
            .hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = hook;
    }

    #[cfg(feature = "reliability")]
    fn fault(&self, point: crate::fault::LabFaultPoint) -> Result<()> {
        crate::fault::check(&self.artifacts.hook, point)
    }

    #[cfg(not(feature = "reliability"))]
    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    fn fault(&self, _point: crate::fault::LabFaultPoint) -> Result<()> {
        Ok(())
    }

    /// The implementation catalog.
    #[must_use]
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// The registry (read access for listings and tests).
    #[must_use]
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// The artifact store.
    #[must_use]
    pub fn artifacts(&self) -> &ArtifactStore {
        &self.artifacts
    }

    fn lock_path(&self, id: ExperimentId) -> PathBuf {
        self.locks.join(format!("{id}.lock"))
    }

    fn create_lock_path(&self, id: ExperimentId) -> PathBuf {
        self.locks.join(format!("{id}.create"))
    }

    fn owned(&self) -> std::sync::MutexGuard<'_, HashMap<ExperimentId, File>> {
        self.owned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Records `new` (status `created`) while holding its creation lock,
    /// which this `Lab` keeps until the experiment starts or is released.
    /// Every kind is created through here.
    ///
    /// # Errors
    /// I/O or registry errors.
    pub fn create_experiment(&self, new: &NewExperiment) -> Result<ExperimentRecord> {
        let id = ExperimentId::generate();
        let path = self.create_lock_path(id);
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)?;
        // A fresh file: only a failing lock implementation can refuse this.
        if let Err(e) = file.try_lock() {
            let _ = fs::remove_file(&path);
            return Err(std::io::Error::other(e.to_string()).into());
        }
        if let Err(e) = self.registry.create_with_id(id, new) {
            let _ = fs::remove_file(&path);
            return Err(e);
        }
        self.owned().insert(id, file);
        self.registry.get(id)
    }

    /// Gives up this `Lab`'s creation lock on a `created` experiment so that
    /// another process can run it. The experiment is then unowned: it is
    /// never marked abandoned automatically. Does nothing if this `Lab` does
    /// not hold the lock.
    pub fn release(&self, id: ExperimentId) {
        if let Some(file) = self.owned().remove(&id) {
            // Deleted before unlocking, so a recovering process that waited
            // for the lock finds no file and leaves the experiment alone.
            let _ = fs::remove_file(self.create_lock_path(id));
            drop(file);
        }
    }

    /// Marks every `created` experiment whose creator is gone as `failed`
    /// (`abandoned`): its creation lock file exists and its lock can be
    /// taken. Experiments without a creation lock file (released, or
    /// created by an older version) are left alone, as are experiments
    /// whose creator still holds the lock. Returns their ids.
    ///
    /// # Errors
    /// I/O or registry errors.
    pub fn recover_abandoned(&self) -> Result<Vec<ExperimentId>> {
        let mut recovered = Vec::new();
        for id in self.registry.created()? {
            let path = self.create_lock_path(id);
            let file = match OpenOptions::new().write(true).open(&path) {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            match file.try_lock() {
                Ok(()) => {
                    // Released or started while we waited: the file is gone.
                    if !path.exists() {
                        continue;
                    }
                    let failure = Failure::new(
                        FailureKind::Abandoned,
                        "the process that created this experiment stopped before starting it",
                    );
                    if self.registry.fail_created(id, &failure)? {
                        recovered.push(id);
                    }
                    let _ = fs::remove_file(&path);
                    drop(file);
                }
                Err(std::fs::TryLockError::WouldBlock) => {}
                Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
            }
        }
        Ok(recovered)
    }

    fn open_lock(&self, id: ExperimentId) -> Result<File> {
        Ok(OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.lock_path(id))?)
    }

    /// Marks every `running` experiment whose lock is free as `failed`
    /// (`interrupted`). Returns their ids.
    ///
    /// # Errors
    /// I/O or registry errors.
    pub fn recover_interrupted(&self) -> Result<Vec<ExperimentId>> {
        let mut recovered = Vec::new();
        for id in self.registry.running()? {
            let file = self.open_lock(id)?;
            match file.try_lock() {
                Ok(()) => {
                    let failure = Failure::new(
                        FailureKind::Interrupted,
                        "the process executing this experiment stopped before it finished",
                    );
                    match self.registry.fail(id, &failure, None, None) {
                        Ok(()) => recovered.push(id),
                        Err(LabError::InvalidTransition { .. }) => {}
                        Err(e) => return Err(e),
                    }
                    drop(file);
                    let _ = fs::remove_file(self.lock_path(id));
                }
                Err(std::fs::TryLockError::WouldBlock) => {}
                Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
            }
        }
        Ok(recovered)
    }

    fn implementations(
        &self,
        spec: &DifferentialSpec,
    ) -> Result<[std::sync::Arc<dyn Implementation>; 2]> {
        let find = |id: &str| {
            self.catalog
                .find(id)
                .ok_or_else(|| LabError::UnknownImplementation(id.to_owned()))
        };
        Ok([find(&spec.left)?, find(&spec.right)?])
    }

    /// Validates `spec` and records a new experiment (status `created`).
    ///
    /// # Errors
    /// [`LabError::InvalidSpec`], [`LabError::UnknownImplementation`] or
    /// registry errors.
    pub fn create(&self, spec: &DifferentialSpec) -> Result<ExperimentRecord> {
        spec.validate(&self.catalog)?;
        let [left, right] = self.implementations(spec)?;
        let implementations = json!([
            { "side": Side::Left, "descriptor": left.descriptor() },
            { "side": Side::Right, "descriptor": right.descriptor() },
        ]);
        self.create_experiment(&NewExperiment::differential(
            spec,
            implementations,
            BuildInfo::current(),
        )?)
    }

    /// Fingerprint recorded for `side` in an experiment's descriptors.
    fn recorded_fingerprint(record: &ExperimentRecord, side: Side) -> Option<Digest> {
        record
            .implementations
            .as_array()?
            .iter()
            .find(|e| e["side"] == side.as_str())?
            .pointer("/descriptor/fingerprint")?
            .as_str()
            .and_then(|hex| Digest::from_hex(hex).ok())
    }

    /// Runs a `created` experiment against `store`: `created` → `running` →
    /// `completed` or `failed`. Returns the stored record.
    ///
    /// A problem with the input (missing stream, damaged history, bad
    /// range) or with an implementation that is no longer registered fails
    /// the experiment with a structured [`Failure`]. Implementation errors
    /// and divergences are results, and complete it.
    ///
    /// # Errors
    /// [`LabError::InvalidTransition`] if it is not `created`, or registry
    /// errors.
    pub fn run(&self, id: ExperimentId, store: &EventStore) -> Result<ExperimentRecord> {
        let record = self.registry.get(id)?;
        if record.kind != crate::spec::DIFFERENTIAL_REPLAY {
            return Err(LabError::InvalidState {
                id,
                reason: format!("{} experiments are not run by Lab::run", record.kind),
            });
        }
        self.run_locked(id, |record| self.execute(record, store))
    }

    /// Runs a `created` experiment of any kind: takes its lock, `created` →
    /// `running`, calls `execute`, then records `completed` or `failed`, and
    /// releases the lock. The lab fault points `run_after_start` and
    /// `run_before_complete` are consulted here.
    ///
    /// # Errors
    /// [`LabError::InvalidTransition`] if it is not `created`, or registry
    /// errors.
    pub fn run_locked(
        &self,
        id: ExperimentId,
        execute: impl FnOnce(&ExperimentRecord) -> std::result::Result<Completion, RunFailure>,
    ) -> Result<ExperimentRecord> {
        let record = self.registry.get(id)?;
        if record.status != Status::Created {
            return Err(LabError::InvalidTransition {
                id,
                from: record.status,
                to: Status::Running,
            });
        }
        let lock = self.open_lock(id)?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(LabError::InvalidState {
                    id,
                    reason: "another process is starting this experiment".to_owned(),
                });
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
        }
        self.registry.start(id)?;
        // Started: the creation lock has done its job. The file is deleted
        // before the lock is released (see `release`).
        let _ = fs::remove_file(self.create_lock_path(id));
        drop(self.owned().remove(&id));
        let started = Instant::now();
        let outcome = self
            .fault(crate::fault::LabFaultPoint::RunAfterStart)
            .map_err(|e| RunFailure::from(Failure::new(FailureKind::Internal, e.to_string())))
            .and_then(|()| execute(&record))
            .and_then(|completion| {
                self.fault(crate::fault::LabFaultPoint::RunBeforeComplete)
                    .map(|()| completion)
                    .map_err(|e| {
                        RunFailure::from(Failure::new(FailureKind::Internal, e.to_string()))
                    })
            });
        let elapsed = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        let stored = match outcome {
            Ok(completion) => {
                let completion = Completion {
                    elapsed_micros: elapsed,
                    ..completion
                };
                match self.registry.complete(id, &completion) {
                    Ok(()) => Ok(()),
                    Err(e) => {
                        let failure = Failure::new(
                            FailureKind::Internal,
                            format!("the result could not be recorded: {e}"),
                        );
                        self.registry
                            .fail(id, &failure, completion.input.as_ref(), Some(elapsed))
                            .and(Err(e))
                    }
                }
            }
            Err(RunFailure { failure, input }) => {
                self.registry
                    .fail(id, &failure, input.as_deref(), Some(elapsed))
            }
        };
        drop(lock);
        let _ = fs::remove_file(self.lock_path(id));
        stored?;
        self.registry.get(id)
    }

    fn execute(
        &self,
        record: &ExperimentRecord,
        store: &EventStore,
    ) -> std::result::Result<Completion, RunFailure> {
        let spec = DifferentialSpec::from_json(record.spec.clone())
            .map_err(|e| Failure::new(FailureKind::Internal, e.to_string()))?;
        let mut sides = Vec::new();
        for (side, id) in [(Side::Left, &spec.left), (Side::Right, &spec.right)] {
            let Some(implementation) = self.catalog.find(id) else {
                return Err(Failure::new(
                    FailureKind::ImplementationUnavailable,
                    format!("implementation {id} is not registered in this build"),
                )
                .into());
            };
            if Self::recorded_fingerprint(record, side)
                != Some(implementation.descriptor().fingerprint)
            {
                return Err(Failure::new(
                    FailureKind::ImplementationUnavailable,
                    format!("implementation {id} no longer has the identity recorded for this experiment"),
                )
                .into());
            }
            sides.push(implementation);
        }
        let input = differential::resolve_input(store, &spec)?;
        let run = differential::execute(store, &spec, &input, sides[0].as_ref(), sides[1].as_ref())
            .map_err(|failure| RunFailure {
                failure,
                input: Some(Box::new(input.clone())),
            })?;
        let fail_internal = |e: LabError| RunFailure {
            failure: Failure::new(FailureKind::Internal, e.to_string()),
            input: Some(Box::new(input.clone())),
        };
        self.fault(crate::fault::LabFaultPoint::RunAfterExecute)
            .map_err(fail_internal)?;
        let mut artifacts = Vec::new();
        for (role, media_type, bytes) in &run.artifacts {
            let digest = self.artifacts.put(bytes).map_err(fail_internal)?;
            artifacts.push(ArtifactRef {
                role: role.clone(),
                digest,
                media_type: (*media_type).to_owned(),
                size: bytes.len() as u64,
            });
        }
        let result = json_of(&run.result).map_err(fail_internal)?;
        Ok(Completion {
            input: Some(input),
            outcome: run.result.outcome.label().to_owned(),
            result_digest: value_digest(RESULT_TAG, &result),
            result,
            reproducibility: run.result.reproducibility.status.label().to_owned(),
            artifacts,
            elapsed_micros: 0,
        })
    }

    /// Validates, records and runs an experiment.
    ///
    /// # Errors
    /// As [`create`](Self::create) and [`run`](Self::run).
    pub fn create_and_run(
        &self,
        spec: &DifferentialSpec,
        store: &EventStore,
    ) -> Result<ExperimentRecord> {
        let record = self.create(spec)?;
        self.run(record.id, store)
    }

    /// Loads an experiment.
    ///
    /// # Errors
    /// [`LabError::ExperimentNotFound`].
    pub fn experiment(&self, id: ExperimentId) -> Result<ExperimentRecord> {
        self.registry.get(id)
    }

    /// Lists experiments, newest first.
    ///
    /// # Errors
    /// Registry errors.
    pub fn list(
        &self,
        limit: usize,
        before: Option<ExperimentId>,
    ) -> Result<Vec<ExperimentSummary>> {
        self.registry.list(limit, before)
    }

    /// Reads one of an experiment's artifacts by role, verifying its digest.
    ///
    /// # Errors
    /// [`LabError::ArtifactMissing`], [`LabError::ArtifactCorrupted`], or
    /// [`LabError::InvalidState`] if the experiment has no such artifact.
    pub fn artifact(&self, id: ExperimentId, role: &str) -> Result<Vec<u8>> {
        let record = self.registry.get(id)?;
        let artifact = record
            .artifacts
            .iter()
            .find(|a| a.role == role)
            .ok_or_else(|| LabError::InvalidState {
                id,
                reason: format!("no artifact with role {role:?}"),
            })?;
        self.artifacts.read(&artifact.digest)
    }

    /// Checks the record's own digests and every artifact, and records the
    /// check as a verification.
    ///
    /// # Errors
    /// [`LabError::InvalidState`] unless the experiment is finished.
    pub fn verify_artifacts(
        &self,
        id: ExperimentId,
    ) -> Result<(ArtifactVerification, VerificationRecord)> {
        let record = self.registry.get(id)?;
        let spec_digest_ok =
            crate::spec::envelope_digest(&record.kind, record.spec_version, &record.spec)
                == record.spec_digest;
        let result_digest_ok = record
            .result
            .as_ref()
            .map(|r| Some(value_digest(RESULT_TAG, r)) == record.result_digest);
        let mut artifacts = Vec::new();
        for artifact in &record.artifacts {
            let state = self.artifacts.check(&artifact.digest, artifact.size)?;
            artifacts.push(ArtifactCheck {
                artifact: artifact.clone(),
                state,
            });
        }
        let differential = record.kind == crate::spec::DIFFERENTIAL_REPLAY;
        let traces_consistent = record
            .result
            .as_ref()
            .filter(|_| differential)
            .map(|result| {
                let compared = result["boundaries_compared"].as_u64();
                ["left_trace", "right_trace"].iter().all(|role| {
                    record
                        .artifacts
                        .iter()
                        .find(|a| a.role == *role)
                        .is_some_and(|a| compared.is_some_and(|c| a.size == c * 32))
                })
            });
        let ok = spec_digest_ok
            && result_digest_ok != Some(false)
            && traces_consistent != Some(false)
            && artifacts.iter().all(|a| a.state == ArtifactState::Intact);
        let report = ArtifactVerification {
            status: if ok { "intact" } else { "damaged" }.to_owned(),
            spec_digest_ok,
            result_digest_ok,
            traces_consistent,
            artifacts,
        };
        let stored = self.registry.add_verification(
            id,
            VerificationKind::Artifacts,
            &report.status,
            &json_of(&report)?,
        )?;
        Ok((report, stored))
    }

    /// Executes a completed experiment again from its persisted
    /// specification, against `store`, and compares the new result with the
    /// stored one. Nothing is substituted: the same implementations (by id
    /// and fingerprint), the same pinned history and the same comparison
    /// rules are required, or the attempt is classified instead of run. The
    /// attempt is recorded as a verification.
    ///
    /// Checks, in order, and their statuses:
    ///
    /// | Status | Meaning |
    /// |--------|---------|
    /// | `configuration_mismatch` | The stored spec no longer matches its digest or cannot be read, the result was produced under other comparison rules, or the range exceeds this build's limits |
    /// | `not_reproducible` | The original result was classified `not_reproducible`, or the re-execution was nondeterministic |
    /// | `implementation_unavailable` | An implementation is not registered or its fingerprint changed |
    /// | `history_unavailable` | The stream is missing, too short or fails verification |
    /// | `history_changed` | The events at the recorded boundaries have different digests |
    /// | `reproduced` | Same result: identical traces and, for results of the current layout, an identical result document |
    /// | `differs` | A different result; `detail.differences` names what differs |
    ///
    /// Results recorded by an older layout (Sprint 7, `result_version` 1)
    /// are compared by their traces and a version-independent summary,
    /// because the result document itself gained fields.
    ///
    /// # Errors
    /// [`LabError::InvalidState`] unless the experiment is `completed`.
    pub fn reproduce(&self, id: ExperimentId, store: &EventStore) -> Result<VerificationRecord> {
        let record = self.registry.get(id)?;
        if record.kind != crate::spec::DIFFERENTIAL_REPLAY {
            return Err(LabError::InvalidState {
                id,
                reason: format!(
                    "{} experiments are reproduced with the orbit-reliability tool",
                    record.kind
                ),
            });
        }
        let (Status::Completed, Some(recorded_input), Some(recorded_result)) =
            (record.status, record.input.as_ref(), record.result.as_ref())
        else {
            return Err(LabError::InvalidState {
                id,
                reason: format!(
                    "only completed experiments can be reproduced (status is {})",
                    record.status
                ),
            });
        };
        let current = BuildInfo::current();
        let same_executable = match (record.build.executable_digest, current.executable_digest) {
            (Some(a), Some(b)) => Some(a == b),
            _ => None,
        };
        let (status, mut detail) =
            self.reproduction(&record, recorded_input, recorded_result, store)?;
        detail["build"] = json!({
            "recorded": record.build,
            "current": current,
            "same_executable": same_executable,
        });
        detail["identity_completeness"] = json!(record.identity.completeness);
        self.registry
            .add_verification(id, VerificationKind::Reproduction, status.as_str(), &detail)
    }

    /// The parts of a result that every layout records, used to compare
    /// results of different layouts.
    fn summary(result: &Value) -> Value {
        json!({
            "outcome": result["outcome"],
            "input": result["input"],
            "events_processed": result["events_processed"],
            "events_fed_digest": result["events_fed_digest"],
            "boundaries_compared": result["boundaries_compared"],
            "boundaries_differing": result["boundaries_differing"],
            "first_divergence_boundary": result.pointer("/first_divergence/boundary"),
            "first_divergence_left": result.pointer("/first_divergence/left/digest"),
            "first_divergence_right": result.pointer("/first_divergence/right/digest"),
            "final_left": result.pointer("/final_comparison/left/digest"),
            "final_right": result.pointer("/final_comparison/right/digest"),
            "failures": result["failures"].as_array().map(|f| f.iter().map(|x| json!([x["side"], x["boundary"]])).collect::<Vec<_>>()),
        })
    }

    fn reproduction(
        &self,
        record: &ExperimentRecord,
        recorded_input: &InputRange,
        recorded_result: &Value,
        store: &EventStore,
    ) -> Result<(ReproductionStatus, Value)> {
        // 1. The configuration must be exactly what was recorded.
        let mut spec = match DifferentialSpec::from_json(record.spec.clone()) {
            Ok(spec) => spec,
            Err(e) => {
                return Ok((
                    ReproductionStatus::ConfigurationMismatch,
                    json!({ "reason": format!("the stored specification cannot be read by this version: {e}") }),
                ));
            }
        };
        if spec.digest() != record.spec_digest {
            return Ok((
                ReproductionStatus::ConfigurationMismatch,
                json!({ "reason": "the stored specification no longer matches its digest" }),
            ));
        }
        let recorded_semantics = recorded_result
            .pointer("/semantics/version")
            .and_then(Value::as_u64)
            .unwrap_or(1);
        if recorded_semantics != u64::from(differential::SEMANTICS_VERSION) {
            return Ok((
                ReproductionStatus::ConfigurationMismatch,
                json!({
                    "reason": "the result was produced under different comparison rules",
                    "recorded_semantics_version": recorded_semantics,
                    "current_semantics_version": differential::SEMANTICS_VERSION,
                }),
            ));
        }
        if recorded_input.to > crate::spec::MAX_EVENTS {
            return Ok((
                ReproductionStatus::ConfigurationMismatch,
                json!({ "reason": format!("the recorded range exceeds this build's limit of {} events", crate::spec::MAX_EVENTS) }),
            ));
        }
        // 2. A result that was not reproducible cannot be reproduced.
        if record.reproducibility.as_deref() == Some("not_reproducible") {
            return Ok((
                ReproductionStatus::NotReproducible,
                json!({
                    "reason": "the original result was classified not_reproducible",
                    "reasons": recorded_result.pointer("/reproducibility/reasons"),
                }),
            ));
        }
        // 3. The same implementations, by id and fingerprint.
        for (side, implementation_id) in [(Side::Left, &spec.left), (Side::Right, &spec.right)] {
            let current = self.catalog.find(implementation_id);
            let recorded = Self::recorded_fingerprint(record, side);
            match current {
                None => {
                    return Ok((
                        ReproductionStatus::ImplementationUnavailable,
                        json!({ "reason": format!("implementation {implementation_id} is not registered") }),
                    ));
                }
                Some(i) if Some(i.descriptor().fingerprint) != recorded => {
                    return Ok((
                        ReproductionStatus::ImplementationUnavailable,
                        json!({
                            "reason": format!("implementation {implementation_id} has a different fingerprint"),
                            "recorded": recorded,
                            "current": i.descriptor().fingerprint,
                        }),
                    ));
                }
                Some(_) => {}
            }
        }
        // 4. The same pinned history.
        spec.to = Some(recorded_input.to);
        let input = match differential::resolve_input(store, &spec) {
            Ok(input) => input,
            Err(failure) => {
                return Ok((
                    ReproductionStatus::HistoryUnavailable,
                    json!({ "failure": failure }),
                ));
            }
        };
        if &input != recorded_input {
            return Ok((
                ReproductionStatus::HistoryChanged,
                json!({ "recorded": recorded_input, "current": input }),
            ));
        }
        // 5. The recorded evidence must be complete to compare with.
        let mut damaged = Vec::new();
        for artifact in &record.artifacts {
            let state = self.artifacts.check(&artifact.digest, artifact.size)?;
            if state != ArtifactState::Intact {
                damaged.push(json!({ "role": artifact.role, "state": state }));
            }
        }
        if !damaged.is_empty() {
            return Ok((
                ReproductionStatus::ArtifactMissing,
                json!({ "reason": "recorded artifacts are missing or damaged, so the original evidence cannot be compared", "artifacts": damaged }),
            ));
        }
        // 6. Execute and compare.
        let [left, right] = self.implementations(&spec)?;
        let run = match differential::execute(store, &spec, &input, left.as_ref(), right.as_ref()) {
            Ok(run) => run,
            Err(failure) => {
                return Ok((
                    ReproductionStatus::HistoryUnavailable,
                    json!({ "failure": failure }),
                ));
            }
        };
        if run.result.outcome == differential::Outcome::Nondeterministic {
            return Ok((
                ReproductionStatus::NotReproducible,
                json!({ "reason": "an implementation behaved nondeterministically when executed again", "determinism": run.result.determinism }),
            ));
        }
        let result = json_of(&run.result)?;
        let digest = value_digest(RESULT_TAG, &result);
        let mut differences = Vec::new();
        for role in ["left_trace", "right_trace"] {
            let recorded = record
                .artifacts
                .iter()
                .find(|a| a.role == role)
                .map(|a| a.digest);
            let current = run
                .artifacts
                .iter()
                .find(|(r, _, _)| r == role)
                .map(|(_, _, bytes)| crate::digest::content_digest(bytes));
            if recorded != current {
                differences.push(role.to_owned());
            }
        }
        let recorded_version = recorded_result
            .get("result_version")
            .and_then(Value::as_u64)
            .unwrap_or(1);
        let same_layout = recorded_version == u64::from(differential::RESULT_VERSION);
        if same_layout {
            if Some(digest) != record.result_digest {
                differences.push("result_document".to_owned());
            }
        } else {
            let (old, new) = (Self::summary(recorded_result), Self::summary(&result));
            if let (Some(old), Some(new)) = (old.as_object(), new.as_object()) {
                differences.extend(old.keys().filter(|k| old.get(*k) != new.get(*k)).cloned());
            }
        }
        let detail = json!({
            "compared": if same_layout { "full_result" } else { "traces_and_summary" },
            "recorded_result_version": recorded_version,
            "recorded_result_digest": record.result_digest,
            "reproduced_result_digest": digest,
            "recorded_outcome": record.outcome,
            "reproduced_outcome": run.result.outcome.label(),
            "differences": differences,
        });
        Ok((
            if differences.is_empty() {
                ReproductionStatus::Reproduced
            } else {
                ReproductionStatus::Differs
            },
            detail,
        ))
    }

    /// A page of an experiment's per-boundary trace: both state digests and
    /// whether they are equal, read from the verified trace artifacts.
    ///
    /// # Errors
    /// [`LabError::InvalidState`] unless completed, or artifact errors.
    pub fn trace(&self, id: ExperimentId, from: Option<u64>, limit: usize) -> Result<TracePage> {
        let record = self.registry.get(id)?;
        let (Status::Completed, Some(input)) = (record.status, record.input.as_ref()) else {
            return Err(LabError::InvalidState {
                id,
                reason: "only completed differential replays have a trace".to_owned(),
            });
        };
        let left = self.artifact(id, "left_trace")?;
        let right = self.artifact(id, "right_trace")?;
        if left.len() != right.len() || left.len() % 32 != 0 {
            return Err(LabError::CorruptRecord {
                record: id.to_string(),
                reason: "trace artifacts have inconsistent lengths".to_owned(),
            });
        }
        let total = (left.len() / 32) as u64;
        let limit = limit.clamp(1, MAX_TRACE_PAGE) as u64;
        let start = from.unwrap_or(input.from).max(input.from);
        let first_index = (start - input.from).min(total);
        let end_index = first_index.saturating_add(limit).min(total);
        let digest = |bytes: &[u8], i: u64| {
            let at = usize::try_from(i * 32).unwrap_or(usize::MAX);
            Digest::from_slice(&bytes[at..at + 32])
        };
        let mut entries = Vec::new();
        for i in first_index..end_index {
            let (l, r) = (digest(&left, i), digest(&right, i));
            let (Ok(l), Ok(r)) = (l, r) else {
                return Err(LabError::CorruptRecord {
                    record: id.to_string(),
                    reason: "unreadable trace digest".to_owned(),
                });
            };
            entries.push(TraceEntry {
                boundary: input.from + i,
                left: l,
                right: r,
                equal: l == r,
            });
        }
        Ok(TracePage {
            experiment_id: id,
            first_boundary: input.from,
            last_boundary: (total > 0).then(|| input.from + total - 1),
            boundaries_total: total,
            entries,
            next_from: (end_index < total).then(|| input.from + end_index),
        })
    }

    /// Verifications of an experiment, oldest first.
    ///
    /// # Errors
    /// [`LabError::ExperimentNotFound`].
    pub fn verifications(&self, id: ExperimentId) -> Result<Vec<VerificationRecord>> {
        self.registry.verifications(id)
    }
}
