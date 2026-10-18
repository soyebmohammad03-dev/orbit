//! The experiment registry: a SQLite database of its own (`lab.db`),
//! separate from every event store.
//!
//! # Lifecycle
//!
//! ```text
//! created ──▶ running ──▶ completed
//!    │           │
//!    └───────────┴──────▶ failed
//! ```
//!
//! Only these transitions exist. They are enforced twice: by conditional
//! `UPDATE`s in [`Registry`], which report [`LabError::InvalidTransition`],
//! and by the `experiments_lifecycle` trigger, which rejects any other
//! update even from raw SQL. A `CHECK` constraint ties each status to the
//! columns it requires (a completed experiment always has a result, a
//! failed one always has a failure). Rows are never deleted. Terminal rows
//! are never updated.
//!
//! An experiment interrupted while `running` (its process died) is marked
//! `failed` with kind `interrupted` by [`Lab::open`](crate::Lab::open); see
//! [`crate::lab`]. A partially executed experiment is never `completed`.
//!
//! # Representation
//!
//! Timestamps are integer microseconds since the Unix epoch (UTC), rendered
//! as RFC 3339 in JSON. Identifiers are 16-byte UUIDv7 blobs, digests are
//! 32-byte blobs. JSON columns hold canonical JSON (sorted keys, no
//! whitespace).

use std::fmt;
use std::path::Path;
use std::str::FromStr;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use orbit_core::domain::canonical::to_canonical_string;
use orbit_core::{Digest, StoreError, Timestamp};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::error::{LabError, Result};
use crate::identity::{BuildInfo, IdentityAssessment};
use crate::spec::{DIFFERENTIAL_REPLAY, DifferentialSpec, SPEC_VERSION};

/// `PRAGMA application_id` of lab registries (`"ORLB"`). Distinct from the
/// event store's, so neither can be opened as the other.
pub const APPLICATION_ID: i64 = 0x4F52_4C42;
/// Newest registry schema version.
pub const LATEST_VERSION: i64 = 4;
/// How long a writer waits for a lock held by another connection.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

const MIGRATIONS: [&str; 4] = [
    r"
CREATE TABLE experiments (
    id              BLOB PRIMARY KEY CHECK (length(id) = 16),
    kind            TEXT NOT NULL CHECK (kind IN ('differential_replay')),
    spec_version    INTEGER NOT NULL CHECK (spec_version >= 1),
    spec            TEXT NOT NULL,
    spec_digest     BLOB NOT NULL CHECK (length(spec_digest) = 32),
    stream_id       TEXT NOT NULL,
    left_impl       TEXT NOT NULL,
    right_impl      TEXT NOT NULL,
    implementations TEXT NOT NULL,
    orbit_version   TEXT NOT NULL,
    git_commit      TEXT,
    created_at      INTEGER NOT NULL,
    status          TEXT NOT NULL CHECK (status IN ('created', 'running', 'completed', 'failed')),
    started_at      INTEGER,
    finished_at     INTEGER,
    elapsed_micros  INTEGER,
    input           TEXT,
    outcome         TEXT,
    result          TEXT,
    result_digest   BLOB CHECK (result_digest IS NULL OR length(result_digest) = 32),
    reproducibility TEXT CHECK (reproducibility IS NULL
                                OR reproducibility IN ('reproducible', 'unverified', 'not_reproducible')),
    failure         TEXT,
    CHECK (CASE status
        WHEN 'created' THEN started_at IS NULL AND finished_at IS NULL
                            AND result IS NULL AND failure IS NULL
        WHEN 'running' THEN started_at IS NOT NULL AND finished_at IS NULL
                            AND result IS NULL AND failure IS NULL
        WHEN 'completed' THEN started_at IS NOT NULL AND finished_at IS NOT NULL
                            AND input IS NOT NULL AND outcome IS NOT NULL AND result IS NOT NULL
                            AND result_digest IS NOT NULL AND reproducibility IS NOT NULL
                            AND failure IS NULL
        WHEN 'failed' THEN finished_at IS NOT NULL AND failure IS NOT NULL AND result IS NULL
    END)
);
CREATE INDEX experiments_by_stream ON experiments (stream_id, id);

CREATE TRIGGER experiments_no_delete BEFORE DELETE ON experiments
BEGIN SELECT RAISE(ABORT, 'experiments are never deleted'); END;

CREATE TRIGGER experiments_lifecycle BEFORE UPDATE ON experiments
WHEN NOT ((OLD.status = 'created' AND NEW.status IN ('running', 'failed'))
       OR (OLD.status = 'running' AND NEW.status IN ('completed', 'failed')))
BEGIN SELECT RAISE(ABORT, 'invalid experiment status transition'); END;

CREATE TRIGGER experiments_identity_is_immutable BEFORE UPDATE ON experiments
WHEN NEW.id IS NOT OLD.id OR NEW.kind IS NOT OLD.kind
  OR NEW.spec_version IS NOT OLD.spec_version OR NEW.spec IS NOT OLD.spec
  OR NEW.spec_digest IS NOT OLD.spec_digest OR NEW.stream_id IS NOT OLD.stream_id
  OR NEW.left_impl IS NOT OLD.left_impl OR NEW.right_impl IS NOT OLD.right_impl
  OR NEW.implementations IS NOT OLD.implementations
  OR NEW.orbit_version IS NOT OLD.orbit_version OR NEW.git_commit IS NOT OLD.git_commit
  OR NEW.created_at IS NOT OLD.created_at
  OR (OLD.started_at IS NOT NULL AND NEW.started_at IS NOT OLD.started_at)
BEGIN SELECT RAISE(ABORT, 'experiment identity columns are immutable'); END;

CREATE TABLE artifacts (
    digest     BLOB PRIMARY KEY CHECK (length(digest) = 32),
    media_type TEXT NOT NULL,
    size       INTEGER NOT NULL CHECK (size >= 0),
    created_at INTEGER NOT NULL
);
CREATE TRIGGER artifacts_no_update BEFORE UPDATE ON artifacts
BEGIN SELECT RAISE(ABORT, 'artifacts are immutable'); END;
CREATE TRIGGER artifacts_no_delete BEFORE DELETE ON artifacts
BEGIN SELECT RAISE(ABORT, 'artifacts are never deleted'); END;

CREATE TABLE experiment_artifacts (
    experiment_id BLOB NOT NULL REFERENCES experiments (id),
    role          TEXT NOT NULL,
    digest        BLOB NOT NULL REFERENCES artifacts (digest),
    PRIMARY KEY (experiment_id, role)
);
CREATE TRIGGER experiment_artifacts_only_while_running BEFORE INSERT ON experiment_artifacts
WHEN (SELECT status FROM experiments WHERE id = NEW.experiment_id) IS NOT 'running'
BEGIN SELECT RAISE(ABORT, 'artifacts can only be attached to a running experiment'); END;
CREATE TRIGGER experiment_artifacts_no_update BEFORE UPDATE ON experiment_artifacts
BEGIN SELECT RAISE(ABORT, 'artifact links are immutable'); END;
CREATE TRIGGER experiment_artifacts_no_delete BEFORE DELETE ON experiment_artifacts
BEGIN SELECT RAISE(ABORT, 'artifact links are never deleted'); END;

CREATE TABLE verifications (
    id            BLOB PRIMARY KEY CHECK (length(id) = 16),
    experiment_id BLOB NOT NULL REFERENCES experiments (id),
    kind          TEXT NOT NULL CHECK (kind IN ('artifacts', 'reproduction')),
    status        TEXT NOT NULL,
    detail        TEXT NOT NULL,
    verified_at   INTEGER NOT NULL
);
CREATE INDEX verifications_by_experiment ON verifications (experiment_id, id);
CREATE TRIGGER verifications_only_when_finished BEFORE INSERT ON verifications
WHEN (SELECT status FROM experiments WHERE id = NEW.experiment_id) NOT IN ('completed', 'failed')
BEGIN SELECT RAISE(ABORT, 'only finished experiments can be verified'); END;
CREATE TRIGGER verifications_no_update BEFORE UPDATE ON verifications
BEGIN SELECT RAISE(ABORT, 'verifications are immutable'); END;
CREATE TRIGGER verifications_no_delete BEFORE DELETE ON verifications
BEGIN SELECT RAISE(ABORT, 'verifications are never deleted'); END;
",
    r"
-- v2 (Sprint 8): record the digest of the executable that created an
-- experiment, and keep it immutable like the other identity columns.
ALTER TABLE experiments ADD COLUMN executable_digest BLOB
    CHECK (executable_digest IS NULL OR length(executable_digest) = 32);

DROP TRIGGER experiments_identity_is_immutable;
CREATE TRIGGER experiments_identity_is_immutable BEFORE UPDATE ON experiments
WHEN NEW.id IS NOT OLD.id OR NEW.kind IS NOT OLD.kind
  OR NEW.spec_version IS NOT OLD.spec_version OR NEW.spec IS NOT OLD.spec
  OR NEW.spec_digest IS NOT OLD.spec_digest OR NEW.stream_id IS NOT OLD.stream_id
  OR NEW.left_impl IS NOT OLD.left_impl OR NEW.right_impl IS NOT OLD.right_impl
  OR NEW.implementations IS NOT OLD.implementations
  OR NEW.orbit_version IS NOT OLD.orbit_version OR NEW.git_commit IS NOT OLD.git_commit
  OR NEW.executable_digest IS NOT OLD.executable_digest
  OR NEW.created_at IS NOT OLD.created_at
  OR (OLD.started_at IS NOT NULL AND NEW.started_at IS NOT OLD.started_at)
BEGIN SELECT RAISE(ABORT, 'experiment identity columns are immutable'); END;
",
    r"
-- v3 (Sprint 9): admit reliability experiments. They have no stream or
-- implementations and no single input range, so those columns become
-- optional for them (still required for differential replays). SQLite
-- cannot alter a CHECK constraint, so the table is rebuilt; triggers that
-- mention it are dropped first and recreated unchanged afterwards. Runs with
-- foreign keys disabled (see `Registry::open`), followed by a foreign-key check.
DROP TRIGGER experiment_artifacts_only_while_running;
DROP TRIGGER verifications_only_when_finished;
DROP TRIGGER experiments_no_delete;
DROP TRIGGER experiments_lifecycle;
DROP TRIGGER experiments_identity_is_immutable;
DROP INDEX experiments_by_stream;

CREATE TABLE experiments_v3 (
    id              BLOB PRIMARY KEY CHECK (length(id) = 16),
    kind            TEXT NOT NULL CHECK (kind IN ('differential_replay', 'reliability')),
    spec_version    INTEGER NOT NULL CHECK (spec_version >= 1),
    spec            TEXT NOT NULL,
    spec_digest     BLOB NOT NULL CHECK (length(spec_digest) = 32),
    stream_id       TEXT,
    left_impl       TEXT,
    right_impl      TEXT,
    implementations TEXT NOT NULL,
    orbit_version   TEXT NOT NULL,
    git_commit      TEXT,
    created_at      INTEGER NOT NULL,
    status          TEXT NOT NULL CHECK (status IN ('created', 'running', 'completed', 'failed')),
    started_at      INTEGER,
    finished_at     INTEGER,
    elapsed_micros  INTEGER,
    input           TEXT,
    outcome         TEXT,
    result          TEXT,
    result_digest   BLOB CHECK (result_digest IS NULL OR length(result_digest) = 32),
    reproducibility TEXT CHECK (reproducibility IS NULL
                                OR reproducibility IN ('reproducible', 'unverified', 'not_reproducible')),
    failure         TEXT,
    executable_digest BLOB CHECK (executable_digest IS NULL OR length(executable_digest) = 32),
    CHECK (kind <> 'differential_replay'
           OR (stream_id IS NOT NULL AND left_impl IS NOT NULL AND right_impl IS NOT NULL)),
    CHECK (CASE status
        WHEN 'created' THEN started_at IS NULL AND finished_at IS NULL
                            AND result IS NULL AND failure IS NULL
        WHEN 'running' THEN started_at IS NOT NULL AND finished_at IS NULL
                            AND result IS NULL AND failure IS NULL
        WHEN 'completed' THEN started_at IS NOT NULL AND finished_at IS NOT NULL
                            AND (input IS NOT NULL OR kind <> 'differential_replay')
                            AND outcome IS NOT NULL AND result IS NOT NULL
                            AND result_digest IS NOT NULL AND reproducibility IS NOT NULL
                            AND failure IS NULL
        WHEN 'failed' THEN finished_at IS NOT NULL AND failure IS NOT NULL AND result IS NULL
    END)
);
INSERT INTO experiments_v3 (id, kind, spec_version, spec, spec_digest, stream_id, left_impl,
    right_impl, implementations, orbit_version, git_commit, created_at, status, started_at,
    finished_at, elapsed_micros, input, outcome, result, result_digest, reproducibility, failure,
    executable_digest)
SELECT id, kind, spec_version, spec, spec_digest, stream_id, left_impl,
    right_impl, implementations, orbit_version, git_commit, created_at, status, started_at,
    finished_at, elapsed_micros, input, outcome, result, result_digest, reproducibility, failure,
    executable_digest
FROM experiments;
DROP TABLE experiments;
ALTER TABLE experiments_v3 RENAME TO experiments;
CREATE INDEX experiments_by_stream ON experiments (stream_id, id);

CREATE TRIGGER experiments_no_delete BEFORE DELETE ON experiments
BEGIN SELECT RAISE(ABORT, 'experiments are never deleted'); END;

CREATE TRIGGER experiments_lifecycle BEFORE UPDATE ON experiments
WHEN NOT ((OLD.status = 'created' AND NEW.status IN ('running', 'failed'))
       OR (OLD.status = 'running' AND NEW.status IN ('completed', 'failed')))
BEGIN SELECT RAISE(ABORT, 'invalid experiment status transition'); END;

CREATE TRIGGER experiments_identity_is_immutable BEFORE UPDATE ON experiments
WHEN NEW.id IS NOT OLD.id OR NEW.kind IS NOT OLD.kind
  OR NEW.spec_version IS NOT OLD.spec_version OR NEW.spec IS NOT OLD.spec
  OR NEW.spec_digest IS NOT OLD.spec_digest OR NEW.stream_id IS NOT OLD.stream_id
  OR NEW.left_impl IS NOT OLD.left_impl OR NEW.right_impl IS NOT OLD.right_impl
  OR NEW.implementations IS NOT OLD.implementations
  OR NEW.orbit_version IS NOT OLD.orbit_version OR NEW.git_commit IS NOT OLD.git_commit
  OR NEW.executable_digest IS NOT OLD.executable_digest
  OR NEW.created_at IS NOT OLD.created_at
  OR (OLD.started_at IS NOT NULL AND NEW.started_at IS NOT OLD.started_at)
BEGIN SELECT RAISE(ABORT, 'experiment identity columns are immutable'); END;

CREATE TRIGGER experiment_artifacts_only_while_running BEFORE INSERT ON experiment_artifacts
WHEN (SELECT status FROM experiments WHERE id = NEW.experiment_id) IS NOT 'running'
BEGIN SELECT RAISE(ABORT, 'artifacts can only be attached to a running experiment'); END;

CREATE TRIGGER verifications_only_when_finished BEFORE INSERT ON verifications
WHEN (SELECT status FROM experiments WHERE id = NEW.experiment_id) NOT IN ('completed', 'failed')
BEGIN SELECT RAISE(ABORT, 'only finished experiments can be verified'); END;
",
    r"
-- v4 (Phase 9): admit performance experiments (no stream, implementations or
-- input range, like reliability experiments), and record imported evidence
-- bundles apart from locally executed experiments. The experiments table is
-- rebuilt as in v3 because SQLite cannot alter a CHECK constraint.
DROP TRIGGER experiment_artifacts_only_while_running;
DROP TRIGGER verifications_only_when_finished;
DROP TRIGGER experiments_no_delete;
DROP TRIGGER experiments_lifecycle;
DROP TRIGGER experiments_identity_is_immutable;
DROP INDEX experiments_by_stream;

CREATE TABLE experiments_v4 (
    id              BLOB PRIMARY KEY CHECK (length(id) = 16),
    kind            TEXT NOT NULL CHECK (kind IN ('differential_replay', 'reliability', 'performance')),
    spec_version    INTEGER NOT NULL CHECK (spec_version >= 1),
    spec            TEXT NOT NULL,
    spec_digest     BLOB NOT NULL CHECK (length(spec_digest) = 32),
    stream_id       TEXT,
    left_impl       TEXT,
    right_impl      TEXT,
    implementations TEXT NOT NULL,
    orbit_version   TEXT NOT NULL,
    git_commit      TEXT,
    created_at      INTEGER NOT NULL,
    status          TEXT NOT NULL CHECK (status IN ('created', 'running', 'completed', 'failed')),
    started_at      INTEGER,
    finished_at     INTEGER,
    elapsed_micros  INTEGER,
    input           TEXT,
    outcome         TEXT,
    result          TEXT,
    result_digest   BLOB CHECK (result_digest IS NULL OR length(result_digest) = 32),
    reproducibility TEXT CHECK (reproducibility IS NULL
                                OR reproducibility IN ('reproducible', 'unverified', 'not_reproducible')),
    failure         TEXT,
    executable_digest BLOB CHECK (executable_digest IS NULL OR length(executable_digest) = 32),
    CHECK (kind <> 'differential_replay'
           OR (stream_id IS NOT NULL AND left_impl IS NOT NULL AND right_impl IS NOT NULL)),
    CHECK (CASE status
        WHEN 'created' THEN started_at IS NULL AND finished_at IS NULL
                            AND result IS NULL AND failure IS NULL
        WHEN 'running' THEN started_at IS NOT NULL AND finished_at IS NULL
                            AND result IS NULL AND failure IS NULL
        WHEN 'completed' THEN started_at IS NOT NULL AND finished_at IS NOT NULL
                            AND (input IS NOT NULL OR kind <> 'differential_replay')
                            AND outcome IS NOT NULL AND result IS NOT NULL
                            AND result_digest IS NOT NULL AND reproducibility IS NOT NULL
                            AND failure IS NULL
        WHEN 'failed' THEN finished_at IS NOT NULL AND failure IS NOT NULL AND result IS NULL
    END)
);
INSERT INTO experiments_v4 (id, kind, spec_version, spec, spec_digest, stream_id, left_impl,
    right_impl, implementations, orbit_version, git_commit, created_at, status, started_at,
    finished_at, elapsed_micros, input, outcome, result, result_digest, reproducibility, failure,
    executable_digest)
SELECT id, kind, spec_version, spec, spec_digest, stream_id, left_impl,
    right_impl, implementations, orbit_version, git_commit, created_at, status, started_at,
    finished_at, elapsed_micros, input, outcome, result, result_digest, reproducibility, failure,
    executable_digest
FROM experiments;
DROP TABLE experiments;
ALTER TABLE experiments_v4 RENAME TO experiments;
CREATE INDEX experiments_by_stream ON experiments (stream_id, id);
CREATE INDEX experiments_by_kind ON experiments (kind, id);

CREATE TRIGGER experiments_no_delete BEFORE DELETE ON experiments
BEGIN SELECT RAISE(ABORT, 'experiments are never deleted'); END;

CREATE TRIGGER experiments_lifecycle BEFORE UPDATE ON experiments
WHEN NOT ((OLD.status = 'created' AND NEW.status IN ('running', 'failed'))
       OR (OLD.status = 'running' AND NEW.status IN ('completed', 'failed')))
BEGIN SELECT RAISE(ABORT, 'invalid experiment status transition'); END;

CREATE TRIGGER experiments_identity_is_immutable BEFORE UPDATE ON experiments
WHEN NEW.id IS NOT OLD.id OR NEW.kind IS NOT OLD.kind
  OR NEW.spec_version IS NOT OLD.spec_version OR NEW.spec IS NOT OLD.spec
  OR NEW.spec_digest IS NOT OLD.spec_digest OR NEW.stream_id IS NOT OLD.stream_id
  OR NEW.left_impl IS NOT OLD.left_impl OR NEW.right_impl IS NOT OLD.right_impl
  OR NEW.implementations IS NOT OLD.implementations
  OR NEW.orbit_version IS NOT OLD.orbit_version OR NEW.git_commit IS NOT OLD.git_commit
  OR NEW.executable_digest IS NOT OLD.executable_digest
  OR NEW.created_at IS NOT OLD.created_at
  OR (OLD.started_at IS NOT NULL AND NEW.started_at IS NOT OLD.started_at)
BEGIN SELECT RAISE(ABORT, 'experiment identity columns are immutable'); END;

CREATE TRIGGER experiment_artifacts_only_while_running BEFORE INSERT ON experiment_artifacts
WHEN (SELECT status FROM experiments WHERE id = NEW.experiment_id) IS NOT 'running'
BEGIN SELECT RAISE(ABORT, 'artifacts can only be attached to a running experiment'); END;

CREATE TRIGGER verifications_only_when_finished BEFORE INSERT ON verifications
WHEN (SELECT status FROM experiments WHERE id = NEW.experiment_id) NOT IN ('completed', 'failed')
BEGIN SELECT RAISE(ABORT, 'only finished experiments can be verified'); END;

-- Imported evidence bundles. Kept apart from `experiments`: an imported
-- record was not executed here, so it never takes part in the local
-- lifecycle and can never replace a local experiment. `document` is the
-- bundle without entry contents (those are stored as artifacts by digest).
CREATE TABLE imported_bundles (
    bundle_digest  BLOB PRIMARY KEY CHECK (length(bundle_digest) = 32),
    format_version INTEGER NOT NULL CHECK (format_version >= 1),
    experiment_id  BLOB NOT NULL CHECK (length(experiment_id) = 16),
    kind           TEXT NOT NULL,
    status         TEXT NOT NULL,
    outcome        TEXT,
    document       TEXT NOT NULL,
    validation     TEXT NOT NULL,
    imported_at    INTEGER NOT NULL
);
CREATE INDEX imported_bundles_by_experiment ON imported_bundles (experiment_id);
CREATE TRIGGER imported_bundles_no_update BEFORE UPDATE ON imported_bundles
BEGIN SELECT RAISE(ABORT, 'imported bundles are immutable'); END;
CREATE TRIGGER imported_bundles_no_delete BEFORE DELETE ON imported_bundles
BEGIN SELECT RAISE(ABORT, 'imported bundles are never deleted'); END;

CREATE TABLE bundle_verifications (
    id            BLOB PRIMARY KEY CHECK (length(id) = 16),
    bundle_digest BLOB NOT NULL REFERENCES imported_bundles (bundle_digest),
    kind          TEXT NOT NULL CHECK (kind IN ('reproduction')),
    status        TEXT NOT NULL,
    detail        TEXT NOT NULL,
    verified_at   INTEGER NOT NULL
);
CREATE INDEX bundle_verifications_by_bundle ON bundle_verifications (bundle_digest, id);
CREATE TRIGGER bundle_verifications_no_update BEFORE UPDATE ON bundle_verifications
BEGIN SELECT RAISE(ABORT, 'verifications are immutable'); END;
CREATE TRIGGER bundle_verifications_no_delete BEFORE DELETE ON bundle_verifications
BEGIN SELECT RAISE(ABORT, 'verifications are never deleted'); END;
",
];

/// Identifier of an experiment (UUIDv7, so ids sort by creation time).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExperimentId(Uuid);

impl ExperimentId {
    /// Generates a new id.
    #[must_use]
    pub fn generate() -> Self {
        Self(Uuid::now_v7())
    }

    /// The underlying UUID.
    #[must_use]
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }

    fn from_slice(bytes: &[u8]) -> Option<Self> {
        Uuid::from_slice(bytes)
            .ok()
            .filter(|u| !u.is_nil())
            .map(Self)
    }
}

impl fmt::Display for ExperimentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for ExperimentId {
    type Err = LabError;
    fn from_str(s: &str) -> Result<Self> {
        Uuid::parse_str(s)
            .ok()
            .filter(|u| !u.is_nil())
            .map(Self)
            .ok_or_else(|| LabError::InvalidSpec {
                field: "experiment_id",
                reason: format!("{s:?} is not a UUID"),
            })
    }
}

/// Lifecycle status of an experiment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Recorded and validated, not started.
    Created,
    /// Executing.
    Running,
    /// Finished; a result is stored.
    Completed,
    /// Could not be carried out; a structured failure is stored.
    Failed,
}

impl Status {
    /// Stored form.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "created" => Self::Created,
            "running" => Self::Running,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            _ => return None,
        })
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The exact input history of an experiment, resolved when it ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputRange {
    /// Stream.
    pub stream: String,
    /// First compared boundary.
    pub from: u64,
    /// Last compared boundary (number of events folded).
    pub to: u64,
    /// Digest of event `from` (zeros for 0).
    pub from_event_digest: Digest,
    /// Digest of event `to` (zeros for 0). Because event digests are
    /// chained, this pins every event `1..=to`.
    pub to_event_digest: Digest,
    /// `ORBIT-LAB-HISTORY-V1` digest over stream, range and both boundary digests.
    pub history_fingerprint: Digest,
}

/// Category of an experiment failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// The stream does not exist or lacks the requested events.
    HistoryUnavailable,
    /// The stream failed integrity verification; nothing was replayed.
    IntegrityFailure,
    /// The range is not valid for the stream (for example `from` beyond the head).
    InvalidRange,
    /// A named implementation is not registered in this build.
    ImplementationUnavailable,
    /// The process stopped while the experiment was running.
    Interrupted,
    /// The process that created the experiment stopped before starting it
    /// (detected through its creation lock; see [`crate::lab`]).
    Abandoned,
    /// Anything else (registry or artifact I/O, store errors).
    Internal,
    /// A reliability scenario could not set up or observe its experiment
    /// (for example a child process could not be started). Says nothing
    /// about the reliability of the system under test.
    Infrastructure,
}

/// Why an experiment failed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Failure {
    /// Category.
    pub kind: FailureKind,
    /// Human-readable description. Contains no local paths.
    pub message: String,
    /// Structured details (may be `null`).
    #[serde(default)]
    pub details: Value,
}

impl Failure {
    /// Builds a failure without details.
    pub fn new(kind: FailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            details: Value::Null,
        }
    }
}

/// A stored artifact attached to an experiment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    /// What the artifact is (`left_trace`, `final_states`, …). Unique per experiment.
    pub role: String,
    /// SHA-256 of the content.
    pub digest: Digest,
    /// Media type.
    pub media_type: String,
    /// Size in bytes.
    pub size: u64,
}

/// Everything recorded when an experiment completes.
#[derive(Debug, Clone)]
pub struct Completion {
    /// Resolved input (differential replays).
    pub input: Option<InputRange>,
    /// Outcome label.
    pub outcome: String,
    /// The result document.
    pub result: Value,
    /// Its digest.
    pub result_digest: Digest,
    /// `reproducible`, `unverified` or `not_reproducible`.
    pub reproducibility: String,
    /// Attached artifacts (already written to the artifact store).
    pub artifacts: Vec<ArtifactRef>,
    /// Wall-clock execution time. Informational; not part of the result.
    pub elapsed_micros: u64,
}

/// A new experiment to record.
#[derive(Debug, Clone)]
pub struct NewExperiment {
    /// Experiment kind (`differential_replay` or `reliability`).
    pub kind: &'static str,
    /// Specification format version.
    pub spec_version: u32,
    /// The validated specification, every default written out.
    pub spec: Value,
    /// Stream, for differential replays.
    pub stream: Option<String>,
    /// Left implementation id, for differential replays.
    pub left: Option<String>,
    /// Right implementation id, for differential replays.
    pub right: Option<String>,
    /// Descriptors of the implementations, as `{"side": …, "descriptor": …}`
    /// objects (an empty array for reliability experiments).
    pub implementations: Value,
    /// The build creating it.
    pub build: BuildInfo,
}

impl NewExperiment {
    /// A differential replay.
    ///
    /// # Errors
    /// If the spec cannot be serialized.
    pub fn differential(
        spec: &DifferentialSpec,
        implementations: Value,
        build: BuildInfo,
    ) -> Result<Self> {
        Ok(Self {
            kind: DIFFERENTIAL_REPLAY,
            spec_version: SPEC_VERSION,
            spec: serde_json::to_value(spec)?,
            stream: Some(spec.stream.clone()),
            left: Some(spec.left.clone()),
            right: Some(spec.right.clone()),
            implementations,
            build,
        })
    }
}

/// A stored experiment.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExperimentRecord {
    /// Identifier.
    pub id: ExperimentId,
    /// Experiment kind.
    pub kind: String,
    /// Specification format version.
    pub spec_version: u32,
    /// The canonical specification.
    pub spec: Value,
    /// Digest of the specification envelope.
    pub spec_digest: Digest,
    /// Implementation descriptors, by side.
    pub implementations: Value,
    /// Build that created it.
    pub build: BuildInfo,
    /// Lifecycle status.
    pub status: Status,
    /// When it was created.
    pub created_at: Timestamp,
    /// When it started running.
    pub started_at: Option<Timestamp>,
    /// When it completed or failed.
    pub finished_at: Option<Timestamp>,
    /// Wall-clock execution time (informational).
    pub elapsed_micros: Option<u64>,
    /// Resolved input history.
    pub input: Option<InputRange>,
    /// Outcome label.
    pub outcome: Option<String>,
    /// Result document.
    pub result: Option<Value>,
    /// Digest of `result`.
    pub result_digest: Option<Digest>,
    /// Reproducibility status.
    pub reproducibility: Option<String>,
    /// Failure, when `status` is `failed`.
    pub failure: Option<Failure>,
    /// Attached artifacts.
    pub artifacts: Vec<ArtifactRef>,
    /// Declared, configuration and build identity with its completeness.
    /// Derived from the stored descriptors and build when loaded.
    pub identity: IdentityAssessment,
}

/// A row of [`Registry::list`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExperimentSummary {
    /// Identifier.
    pub id: ExperimentId,
    /// Experiment kind.
    pub kind: String,
    /// Lifecycle status.
    pub status: Status,
    /// When it was created.
    pub created_at: Timestamp,
    /// Input stream (differential replays).
    pub stream: Option<String>,
    /// Left implementation id (differential replays).
    pub left: Option<String>,
    /// Right implementation id (differential replays).
    pub right: Option<String>,
    /// Outcome label, once completed.
    pub outcome: Option<String>,
    /// Reproducibility status, once completed.
    pub reproducibility: Option<String>,
}

/// Kind of a [`VerificationRecord`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationKind {
    /// Artifact and record digests were checked.
    Artifacts,
    /// The experiment was executed again and compared.
    Reproduction,
}

impl VerificationKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Artifacts => "artifacts",
            Self::Reproduction => "reproduction",
        }
    }
}

/// One stored verification attempt. Verifications are append-only; an
/// experiment's history of checks is never rewritten.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VerificationRecord {
    /// Identifier.
    pub id: Uuid,
    /// Experiment checked.
    pub experiment_id: ExperimentId,
    /// What was checked.
    pub kind: VerificationKind,
    /// Outcome label.
    pub status: String,
    /// Structured detail.
    pub detail: Value,
    /// When.
    pub verified_at: Timestamp,
}

fn now() -> Result<i64> {
    Ok(Timestamp::now().map_err(StoreError::from)?.unix_micros())
}

fn corrupt(record: &str, reason: impl fmt::Display) -> LabError {
    LabError::CorruptRecord {
        record: record.to_owned(),
        reason: reason.to_string(),
    }
}

fn to_json<T: Serialize>(value: &T) -> Result<String> {
    Ok(to_canonical_string(&serde_json::to_value(value)?))
}

/// The registry database. One connection behind a mutex; safe to share
/// between threads. Several `Registry` values (or processes) may open the
/// same file: writes use `BEGIN IMMEDIATE` and wait up to [`BUSY_TIMEOUT`].
#[derive(Debug)]
pub struct Registry {
    conn: Mutex<Connection>,
}

impl Registry {
    /// Opens (creating if needed) the registry at `path` and applies migrations.
    ///
    /// Initialization is deterministic: an empty or new file gets schema
    /// version 1; a file with any other schema, or another application id,
    /// is refused; a newer version is refused.
    ///
    /// # Errors
    /// [`LabError::NotALabRegistry`], [`LabError::UnsupportedSchemaVersion`],
    /// or a database error.
    pub fn open(path: &Path) -> Result<Self> {
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_EXRESCODE;
        let mut conn = Connection::open_with_flags(path, flags)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(LabError::NotALabRegistry(format!(
                "could not enable WAL journal mode (got {mode})"
            )));
        }
        conn.pragma_update(None, "synchronous", "FULL")?;
        // Migrations that rebuild a table need foreign keys off; the pragma
        // can only change outside a transaction. They are checked afterwards.
        conn.pragma_update(None, "foreign_keys", false)?;
        migrate(&mut conn)?;
        let violations: i64 =
            conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })?;
        if violations != 0 {
            return Err(LabError::CorruptRecord {
                record: "registry".to_owned(),
                reason: format!("{violations} foreign-key violations after migration"),
            });
        }
        conn.pragma_update(None, "foreign_keys", true)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock cannot leave a transaction open:
        // rusqlite rolls back on drop.
        self.conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Records a new experiment in status `created`.
    ///
    /// # Errors
    /// Database errors.
    pub fn create(&self, new: &NewExperiment) -> Result<ExperimentId> {
        let id = ExperimentId::generate();
        self.create_with_id(id, new)?;
        Ok(id)
    }

    /// Records a new experiment with a caller-chosen, freshly generated id
    /// (so the caller can take the experiment's creation lock first).
    ///
    /// # Errors
    /// Database errors, including a duplicate id.
    pub fn create_with_id(&self, id: ExperimentId, new: &NewExperiment) -> Result<()> {
        let spec = to_canonical_string(&new.spec);
        let digest = crate::spec::envelope_digest(new.kind, new.spec_version, &new.spec);
        let conn = self.conn();
        conn.execute(
            "INSERT INTO experiments (id, kind, spec_version, spec, spec_digest, stream_id,
                 left_impl, right_impl, implementations, orbit_version, git_commit,
                 executable_digest, created_at, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 'created')",
            params![
                id.0.as_bytes().as_slice(),
                new.kind,
                new.spec_version,
                spec,
                digest.as_bytes().as_slice(),
                new.stream,
                new.left,
                new.right,
                to_canonical_string(&new.implementations),
                new.build.orbit_version,
                new.build.git_commit,
                new.build.executable_digest.map(|d| d.as_bytes().to_vec()),
                now()?,
            ],
        )?;
        Ok(())
    }

    fn status_of(conn: &Connection, id: ExperimentId) -> Result<Status> {
        let text: Option<String> = conn
            .query_row(
                "SELECT status FROM experiments WHERE id = ?1",
                [id.0.as_bytes().as_slice()],
                |r| r.get(0),
            )
            .optional()?;
        let text = text.ok_or(LabError::ExperimentNotFound(id))?;
        Status::parse(&text).ok_or_else(|| corrupt(&id.to_string(), format!("status {text:?}")))
    }

    fn transition_error(conn: &Connection, id: ExperimentId, to: Status) -> LabError {
        match Self::status_of(conn, id) {
            Ok(from) => LabError::InvalidTransition { id, from, to },
            Err(e) => e,
        }
    }

    /// `created` → `running`.
    ///
    /// # Errors
    /// [`LabError::InvalidTransition`] from any other status.
    pub fn start(&self, id: ExperimentId) -> Result<()> {
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE experiments SET status = 'running', started_at = ?2
             WHERE id = ?1 AND status = 'created'",
            params![id.0.as_bytes().as_slice(), now()?],
        )?;
        if changed == 1 {
            Ok(())
        } else {
            Err(Self::transition_error(&conn, id, Status::Running))
        }
    }

    /// `running` → `completed`, attaching the result and artifacts in one
    /// transaction. Either everything is recorded or nothing is.
    ///
    /// # Errors
    /// [`LabError::InvalidTransition`] unless running; database errors.
    pub fn complete(&self, id: ExperimentId, completion: &Completion) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let status = Self::status_of(&tx, id)?;
        if status != Status::Running {
            return Err(LabError::InvalidTransition {
                id,
                from: status,
                to: Status::Completed,
            });
        }
        let at = now()?;
        for artifact in &completion.artifacts {
            let size = i64::try_from(artifact.size).map_err(|_| corrupt("artifact", "size"))?;
            tx.execute(
                "INSERT OR IGNORE INTO artifacts (digest, media_type, size, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    artifact.digest.as_bytes().as_slice(),
                    artifact.media_type,
                    size,
                    at
                ],
            )?;
            let (stored_size, stored_type): (i64, String) = tx.query_row(
                "SELECT size, media_type FROM artifacts WHERE digest = ?1",
                [artifact.digest.as_bytes().as_slice()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if stored_size != size || stored_type != artifact.media_type {
                return Err(corrupt(
                    &format!("artifact {}", artifact.digest),
                    "registered with a different size or media type",
                ));
            }
            tx.execute(
                "INSERT INTO experiment_artifacts (experiment_id, role, digest) VALUES (?1, ?2, ?3)",
                params![
                    id.0.as_bytes().as_slice(),
                    artifact.role,
                    artifact.digest.as_bytes().as_slice()
                ],
            )?;
        }
        tx.execute(
            "UPDATE experiments SET status = 'completed', finished_at = ?2, elapsed_micros = ?3,
                 input = ?4, outcome = ?5, result = ?6, result_digest = ?7, reproducibility = ?8
             WHERE id = ?1 AND status = 'running'",
            params![
                id.0.as_bytes().as_slice(),
                at,
                i64::try_from(completion.elapsed_micros).unwrap_or(i64::MAX),
                completion.input.as_ref().map(to_json).transpose()?,
                completion.outcome,
                to_canonical_string(&completion.result),
                completion.result_digest.as_bytes().as_slice(),
                completion.reproducibility,
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// `created` or `running` → `failed`.
    ///
    /// # Errors
    /// [`LabError::InvalidTransition`] from a terminal status.
    pub fn fail(
        &self,
        id: ExperimentId,
        failure: &Failure,
        input: Option<&InputRange>,
        elapsed_micros: Option<u64>,
    ) -> Result<()> {
        let conn = self.conn();
        let input = input.map(to_json).transpose()?;
        let changed = conn.execute(
            "UPDATE experiments SET status = 'failed', finished_at = ?2, failure = ?3,
                 input = ?4, elapsed_micros = ?5
             WHERE id = ?1 AND status IN ('created', 'running')",
            params![
                id.0.as_bytes().as_slice(),
                now()?,
                to_json(failure)?,
                input,
                elapsed_micros.map(|e| i64::try_from(e).unwrap_or(i64::MAX)),
            ],
        )?;
        if changed == 1 {
            Ok(())
        } else {
            Err(Self::transition_error(&conn, id, Status::Failed))
        }
    }

    /// `created` → `failed`, only if the experiment is still `created`.
    /// Returns whether it changed. Used for abandoned experiments, so an
    /// experiment that has started in the meantime is never touched.
    ///
    /// # Errors
    /// Database errors.
    pub fn fail_created(&self, id: ExperimentId, failure: &Failure) -> Result<bool> {
        let changed = self.conn().execute(
            "UPDATE experiments SET status = 'failed', finished_at = ?2, failure = ?3
             WHERE id = ?1 AND status = 'created'",
            params![id.0.as_bytes().as_slice(), now()?, to_json(failure)?],
        )?;
        Ok(changed == 1)
    }

    /// Loads one experiment with its artifacts.
    ///
    /// # Errors
    /// [`LabError::ExperimentNotFound`] or [`LabError::CorruptRecord`].
    pub fn get(&self, id: ExperimentId) -> Result<ExperimentRecord> {
        let conn = self.conn();
        let raw = conn
            .query_row(
                "SELECT id, kind, spec_version, spec, spec_digest, implementations, orbit_version,
                        git_commit, created_at, status, started_at, finished_at, elapsed_micros,
                        input, outcome, result, result_digest, reproducibility, failure,
                        executable_digest
                 FROM experiments WHERE id = ?1",
                [id.0.as_bytes().as_slice()],
                RawExperiment::read,
            )
            .optional()?
            .ok_or(LabError::ExperimentNotFound(id))?;
        let mut stmt = conn.prepare(
            "SELECT ea.role, a.digest, a.media_type, a.size
             FROM experiment_artifacts ea JOIN artifacts a ON a.digest = ea.digest
             WHERE ea.experiment_id = ?1 ORDER BY ea.role",
        )?;
        let artifacts = stmt
            .query_map([id.0.as_bytes().as_slice()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Vec<u8>>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let record = id.to_string();
        let artifacts = artifacts
            .into_iter()
            .map(|(role, digest, media_type, size)| {
                Ok(ArtifactRef {
                    role,
                    digest: Digest::from_slice(&digest).map_err(|e| corrupt(&record, e))?,
                    media_type,
                    size: u64::try_from(size).map_err(|e| corrupt(&record, e))?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        raw.decode(artifacts)
    }

    /// Up to `limit` experiments, newest first, strictly older than `before`
    /// when given (cursor pagination).
    ///
    /// # Errors
    /// Database errors.
    pub fn list(
        &self,
        limit: usize,
        before: Option<ExperimentId>,
    ) -> Result<Vec<ExperimentSummary>> {
        self.list_kind(limit, before, None)
    }

    /// Like [`list`](Self::list), restricted to one experiment kind when given.
    ///
    /// # Errors
    /// Database errors.
    pub fn list_kind(
        &self,
        limit: usize,
        before: Option<ExperimentId>,
        kind: Option<&str>,
    ) -> Result<Vec<ExperimentSummary>> {
        let conn = self.conn();
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut stmt = conn.prepare(
            "SELECT id, kind, status, created_at, stream_id, left_impl, right_impl, outcome,
                    reproducibility
             FROM experiments WHERE (?1 IS NULL OR id < ?1) AND (?3 IS NULL OR kind = ?3)
             ORDER BY id DESC LIMIT ?2",
        )?;
        let before = before.map(|b| b.0.as_bytes().to_vec());
        let rows = stmt
            .query_map(params![before, limit, kind], |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, Option<String>>(7)?,
                    r.get::<_, Option<String>>(8)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(
                |(id, kind, status, created, stream, left, right, outcome, repro)| {
                    let id =
                        ExperimentId::from_slice(&id).ok_or_else(|| corrupt("experiment", "id"))?;
                    let record = id.to_string();
                    Ok(ExperimentSummary {
                        id,
                        kind,
                        status: Status::parse(&status)
                            .ok_or_else(|| corrupt(&record, format!("status {status:?}")))?,
                        created_at: Timestamp::from_unix_micros(created)
                            .map_err(|e| corrupt(&record, e))?,
                        stream,
                        left,
                        right,
                        outcome,
                        reproducibility: repro,
                    })
                },
            )
            .collect()
    }

    /// Digests of every artifact linked to some experiment.
    ///
    /// # Errors
    /// Database errors.
    pub fn linked_artifact_digests(&self) -> Result<std::collections::BTreeSet<Digest>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT DISTINCT digest FROM experiment_artifacts")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, Vec<u8>>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.iter()
            .map(|b| Digest::from_slice(b).map_err(|e| corrupt("artifact link", e)))
            .collect()
    }

    /// Number of rows in the `artifacts` table.
    ///
    /// # Errors
    /// Database errors.
    pub fn artifact_count(&self) -> Result<u64> {
        let n: i64 = self
            .conn()
            .query_row("SELECT count(*) FROM artifacts", [], |r| r.get(0))?;
        Ok(u64::try_from(n).unwrap_or_default())
    }

    /// Ids of experiments currently in status `running`.
    ///
    /// # Errors
    /// Database errors.
    pub fn running(&self) -> Result<Vec<ExperimentId>> {
        self.with_status(Status::Running)
    }

    /// Ids of experiments currently in status `created`.
    ///
    /// # Errors
    /// Database errors.
    pub fn created(&self) -> Result<Vec<ExperimentId>> {
        self.with_status(Status::Created)
    }

    fn with_status(&self, status: Status) -> Result<Vec<ExperimentId>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT id FROM experiments WHERE status = ?1")?;
        let ids = stmt
            .query_map([status.as_str()], |r| r.get::<_, Vec<u8>>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.iter()
            .map(|b| ExperimentId::from_slice(b).ok_or_else(|| corrupt("experiment", "id")))
            .collect()
    }

    /// Appends a verification record for a finished experiment.
    ///
    /// # Errors
    /// [`LabError::InvalidState`] if the experiment is not finished.
    pub fn add_verification(
        &self,
        experiment: ExperimentId,
        kind: VerificationKind,
        status: &str,
        detail: &Value,
    ) -> Result<VerificationRecord> {
        let conn = self.conn();
        let current = Self::status_of(&conn, experiment)?;
        if !matches!(current, Status::Completed | Status::Failed) {
            return Err(LabError::InvalidState {
                id: experiment,
                reason: format!("only finished experiments can be verified (status is {current})"),
            });
        }
        let id = Uuid::now_v7();
        let at = now()?;
        conn.execute(
            "INSERT INTO verifications (id, experiment_id, kind, status, detail, verified_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id.as_bytes().as_slice(),
                experiment.0.as_bytes().as_slice(),
                kind.as_str(),
                status,
                to_canonical_string(detail),
                at
            ],
        )?;
        Ok(VerificationRecord {
            id,
            experiment_id: experiment,
            kind,
            status: status.to_owned(),
            detail: detail.clone(),
            verified_at: Timestamp::from_unix_micros(at).map_err(StoreError::from)?,
        })
    }

    /// Verifications of an experiment, oldest first.
    ///
    /// # Errors
    /// [`LabError::ExperimentNotFound`] or database errors.
    pub fn verifications(&self, experiment: ExperimentId) -> Result<Vec<VerificationRecord>> {
        let conn = self.conn();
        Self::status_of(&conn, experiment)?;
        let mut stmt = conn.prepare(
            "SELECT id, kind, status, detail, verified_at FROM verifications
             WHERE experiment_id = ?1 ORDER BY id",
        )?;
        let rows = stmt
            .query_map([experiment.0.as_bytes().as_slice()], |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let record = experiment.to_string();
        rows.into_iter()
            .map(|(id, kind, status, detail, at)| {
                Ok(VerificationRecord {
                    id: Uuid::from_slice(&id).map_err(|e| corrupt(&record, e))?,
                    experiment_id: experiment,
                    kind: match kind.as_str() {
                        "artifacts" => VerificationKind::Artifacts,
                        "reproduction" => VerificationKind::Reproduction,
                        other => {
                            return Err(corrupt(&record, format!("verification kind {other:?}")));
                        }
                    },
                    status,
                    detail: serde_json::from_str(&detail).map_err(|e| corrupt(&record, e))?,
                    verified_at: Timestamp::from_unix_micros(at)
                        .map_err(|e| corrupt(&record, e))?,
                })
            })
            .collect()
    }
}

struct RawExperiment {
    id: Vec<u8>,
    kind: String,
    spec_version: i64,
    spec: String,
    spec_digest: Vec<u8>,
    implementations: String,
    orbit_version: String,
    git_commit: Option<String>,
    created_at: i64,
    status: String,
    started_at: Option<i64>,
    finished_at: Option<i64>,
    elapsed_micros: Option<i64>,
    input: Option<String>,
    outcome: Option<String>,
    result: Option<String>,
    result_digest: Option<Vec<u8>>,
    reproducibility: Option<String>,
    failure: Option<String>,
    executable_digest: Option<Vec<u8>>,
}

impl RawExperiment {
    fn read(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            kind: r.get(1)?,
            spec_version: r.get(2)?,
            spec: r.get(3)?,
            spec_digest: r.get(4)?,
            implementations: r.get(5)?,
            orbit_version: r.get(6)?,
            git_commit: r.get(7)?,
            created_at: r.get(8)?,
            status: r.get(9)?,
            started_at: r.get(10)?,
            finished_at: r.get(11)?,
            elapsed_micros: r.get(12)?,
            input: r.get(13)?,
            outcome: r.get(14)?,
            result: r.get(15)?,
            result_digest: r.get(16)?,
            reproducibility: r.get(17)?,
            failure: r.get(18)?,
            executable_digest: r.get(19)?,
        })
    }

    fn decode(self, artifacts: Vec<ArtifactRef>) -> Result<ExperimentRecord> {
        let id = ExperimentId::from_slice(&self.id).ok_or_else(|| corrupt("experiment", "id"))?;
        let record = id.to_string();
        let c = |e: &dyn fmt::Display| corrupt(&record, e);
        let json = |text: &str| -> Result<Value> {
            serde_json::from_str(text).map_err(|e| corrupt(&record, e))
        };
        let ts = |micros: i64| Timestamp::from_unix_micros(micros).map_err(|e| corrupt(&record, e));
        let implementations = json(&self.implementations)?;
        let build = BuildInfo {
            orbit_version: self.orbit_version,
            git_commit: self.git_commit,
            executable_digest: self
                .executable_digest
                .map(|b| Digest::from_slice(&b).map_err(|e| c(&e)))
                .transpose()?,
        };
        Ok(ExperimentRecord {
            id,
            kind: self.kind,
            spec_version: u32::try_from(self.spec_version).map_err(|e| c(&e))?,
            spec: json(&self.spec)?,
            spec_digest: Digest::from_slice(&self.spec_digest).map_err(|e| c(&e))?,
            identity: IdentityAssessment::assess(&implementations, &build),
            implementations,
            build,
            status: Status::parse(&self.status)
                .ok_or_else(|| c(&format!("status {:?}", self.status)))?,
            created_at: ts(self.created_at)?,
            started_at: self.started_at.map(ts).transpose()?,
            finished_at: self.finished_at.map(ts).transpose()?,
            elapsed_micros: self
                .elapsed_micros
                .map(|e| u64::try_from(e).map_err(|e| c(&e)))
                .transpose()?,
            input: self
                .input
                .map(|t| serde_json::from_str(&t).map_err(|e| c(&e)))
                .transpose()?,
            outcome: self.outcome,
            result: self.result.map(|t| json(&t)).transpose()?,
            result_digest: self
                .result_digest
                .map(|b| Digest::from_slice(&b).map_err(|e| c(&e)))
                .transpose()?,
            reproducibility: self.reproducibility,
            failure: self
                .failure
                .map(|t| serde_json::from_str(&t).map_err(|e| c(&e)))
                .transpose()?,
            artifacts,
        })
    }
}

fn migrate(conn: &mut Connection) -> Result<()> {
    loop {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let app_id: i64 = tx.query_row("PRAGMA application_id", [], |r| r.get(0))?;
        let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version == 0 {
            let objects: i64 =
                tx.query_row("SELECT count(*) FROM sqlite_schema", [], |r| r.get(0))?;
            if app_id != 0 || objects != 0 {
                return Err(LabError::NotALabRegistry(
                    "file contains a schema that was not created by the ORBIT lab".to_owned(),
                ));
            }
        } else if app_id != APPLICATION_ID {
            return Err(LabError::NotALabRegistry(format!(
                "unexpected application_id {app_id:#x}"
            )));
        }
        if version > LATEST_VERSION {
            return Err(LabError::UnsupportedSchemaVersion {
                found: version,
                supported: LATEST_VERSION,
            });
        }
        if version == LATEST_VERSION {
            return Ok(());
        }
        let index = usize::try_from(version).map_err(|_| LabError::UnsupportedSchemaVersion {
            found: version,
            supported: LATEST_VERSION,
        })?;
        tracing::info!(
            from = version,
            to = version + 1,
            "applying lab registry migration"
        );
        tx.execute_batch(MIGRATIONS[index])?;
        tx.pragma_update(None, "application_id", APPLICATION_ID)?;
        tx.pragma_update(None, "user_version", version + 1)?;
        tx.commit()?;
    }
}
