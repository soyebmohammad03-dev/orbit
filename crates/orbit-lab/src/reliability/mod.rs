//! Reliability scenarios: controlled failures of the event store and the lab
//! itself, followed by checks of what must still hold.
//!
//! This module is always compiled: the catalog, the specification and the
//! result model describe scenarios and their results, so the API can list
//! and display them. **Executing** a scenario needs fault hooks, so the
//! runner (`runner`) and the child-process roles (`child`) exist only
//! with the `reliability` cargo feature, and only the `orbit-reliability`
//! tool runs them. `orbit-api` cannot.
//!
//! Every scenario works on files it creates in a work directory inside the
//! lab directory (`<lab dir>/reliability-work/<experiment id>/`), removed
//! afterwards. It never opens a user's event store.
//!
//! # Classification
//!
//! Each case records whether its fault was *observed* to happen and a list
//! of invariant checks, each with its expected and observed value. A case is
//! - `inconclusive` if the fault was not observed (for example a child
//!   process exited normally instead of aborting): the checks are written
//!   for the faulted path, so they prove nothing either way;
//! - `invariant_violated` if the fault happened and any check failed;
//! - `invariant_preserved` if the fault happened and every check held.
//!
//! If the scenario cannot set itself up or observe a case (a child process
//! cannot be started, the work directory cannot be written), the experiment
//! *fails* with kind `infrastructure`; that is never reported as a result.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{LabError, Result};
use crate::identity::BuildInfo;
use crate::registry::NewExperiment;
use crate::{ExperimentRecord, Lab};

#[cfg(feature = "reliability")]
pub mod child;
#[cfg(feature = "reliability")]
pub mod runner;

/// Experiment kind of reliability experiments.
pub const RELIABILITY: &str = "reliability";
/// Specification format version.
pub const SPEC_VERSION: u32 = 1;
/// Result layout version.
pub const RESULT_VERSION: u32 = 1;

/// A bounded integer parameter of a scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ParameterInfo {
    /// Name.
    pub name: &'static str,
    /// What it controls.
    pub description: &'static str,
    /// Default value.
    pub default: u64,
    /// Smallest accepted value.
    pub min: u64,
    /// Largest accepted value.
    pub max: u64,
}

/// A supported scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ScenarioInfo {
    /// Stable identifier.
    pub id: &'static str,
    /// Bumped whenever the scenario's cases or checks change.
    pub version: u32,
    /// Short title.
    pub title: &'static str,
    /// What it does.
    pub summary: &'static str,
    /// Failures it produces.
    pub simulates: &'static [&'static str],
    /// Failures it does not produce (never claimed).
    pub does_not_simulate: &'static [&'static str],
    /// What it checks.
    pub invariants: &'static [&'static str],
    /// Parameters.
    pub parameters: &'static [ParameterInfo],
}

const NO_POWER_LOSS: &[&str] = &[
    "power loss or an operating-system crash (unflushed OS or disk caches are not lost)",
    "storage-device faults, torn sector writes or bit rot",
];

const EVENTS: ParameterInfo = ParameterInfo {
    name: "events",
    description: "events committed before the fault",
    default: 10,
    min: 1,
    max: 500,
};

/// The supported scenarios. Nothing outside this list can be run.
pub const SCENARIOS: &[ScenarioInfo] = &[
    ScenarioInfo {
        id: "event-append-interruption",
        version: 1,
        title: "Interrupted event appends",
        summary: "An append is interrupted at each of the five append fault points, once by an injected error in-process and once by aborting a child process; a child that appends continuously is killed with SIGKILL; and a deliberately corrupted copy is checked for detection.",
        simulates: &[
            "an error returned at a transaction stage (simulated, in-process)",
            "a process abort at a transaction stage (std::process::abort in a child process)",
            "an external SIGKILL of a process that is appending",
            "a database file edited behind ORBIT's back (in the scenario's own copy)",
        ],
        does_not_simulate: NO_POWER_LOSS,
        invariants: &[
            "an interrupted append commits nothing: head, events and digests are unchanged",
            "every acknowledged event survives a kill",
            "sequences stay dense and the whole store verifies after reopening",
            "the next append continues at the next sequence",
            "deliberate corruption is detected by verification and refused by differential replay",
        ],
        parameters: &[EVENTS],
    },
    ScenarioInfo {
        id: "snapshot-write-interruption",
        version: 1,
        title: "Interrupted snapshot writes",
        summary: "Saving a snapshot is interrupted at each of the three snapshot fault points, by an injected error and by aborting a child process.",
        simulates: &[
            "an error returned at a snapshot transaction stage (simulated, in-process)",
            "a process abort at a snapshot transaction stage (child process)",
        ],
        does_not_simulate: NO_POWER_LOSS,
        invariants: &[
            "no snapshot is stored by an interrupted save",
            "the store verifies; snapshot-assisted reconstruction equals full replay",
            "a later snapshot save succeeds and validates",
        ],
        parameters: &[EVENTS],
    },
    ScenarioInfo {
        id: "experiment-interruption",
        version: 1,
        title: "Interrupted lab experiments and artifact writes",
        summary: "A differential replay running in a child process is aborted at each lab fault point (including halfway through an artifact write), fails by an injected error, and is paused while another process opens the registry; finally a completed artifact is corrupted on disk.",
        simulates: &[
            "a process abort at each stage of an experiment run (child process)",
            "a process abort halfway through writing an artifact file",
            "an error returned before the result is recorded (simulated, in-process)",
            "a live experiment process while another process opens and recovers the registry",
            "an artifact file corrupted on disk (in the scenario's own lab)",
        ],
        does_not_simulate: NO_POWER_LOSS,
        invariants: &[
            "an interrupted experiment is failed/interrupted: never completed, no result, no artifact links",
            "a live experiment is not marked interrupted while its process runs, and completes afterwards",
            "earlier completed evidence stays completed, intact and reproducible",
            "temporary files of interrupted writes are removed on recovery; no partial file is stored under an artifact name",
            "a corrupted artifact is reported damaged and never read as valid",
        ],
        parameters: &[ParameterInfo {
            default: 8,
            min: 6,
            ..EVENTS
        }],
    },
    ScenarioInfo {
        id: "registry-concurrency",
        version: 1,
        title: "Registry under concurrent processes",
        summary: "Several OS processes, released together by a barrier, create and run experiments on one registry; race to start the same experiment; and race to recover the same interrupted experiment.",
        simulates: &[
            "concurrent writers in separate OS processes with separate SQLite connections",
            "lock contention resolved by SQLite's busy timeout",
        ],
        does_not_simulate: &[
            "a registry on a network file system (unsupported)",
            "processes on different machines",
            "power loss or an operating-system crash",
        ],
        invariants: &[
            "no experiment is lost or duplicated; every id is unique",
            "every experiment completes, with intact artifacts and identical results for identical inputs",
            "exactly one process wins a race to start an experiment",
            "exactly one process recovers an interrupted experiment",
        ],
        parameters: &[
            ParameterInfo {
                name: "processes",
                description: "concurrent child processes",
                default: 4,
                min: 2,
                max: 8,
            },
            ParameterInfo {
                name: "experiments_per_process",
                description: "experiments each process creates and runs",
                default: 5,
                min: 1,
                max: 25,
            },
        ],
    },
];

/// The scenario with this id.
#[must_use]
pub fn scenario(id: &str) -> Option<&'static ScenarioInfo> {
    SCENARIOS.iter().find(|s| s.id == id)
}

/// What to run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReliabilitySpec {
    /// Scenario id from [`SCENARIOS`].
    pub scenario: String,
    /// Scenario version; filled in from the catalog when validated.
    #[serde(default)]
    pub scenario_version: Option<u32>,
    /// Parameter values; missing ones get their defaults.
    #[serde(default)]
    pub parameters: BTreeMap<String, u64>,
}

impl ReliabilitySpec {
    /// Parses a JSON spec, rejecting unknown fields.
    ///
    /// # Errors
    /// [`LabError::InvalidSpec`].
    pub fn from_json(value: Value) -> Result<Self> {
        serde_json::from_value(value).map_err(|e| LabError::InvalidSpec {
            field: "spec",
            reason: e.to_string(),
        })
    }

    /// Checks the spec against the catalog and returns it with the scenario
    /// version and every parameter filled in.
    ///
    /// # Errors
    /// [`LabError::InvalidSpec`] for an unknown scenario or version, an
    /// unknown parameter, or a value out of range.
    pub fn validated(&self) -> Result<(Self, &'static ScenarioInfo)> {
        let info = scenario(&self.scenario).ok_or_else(|| LabError::InvalidSpec {
            field: "scenario",
            reason: format!(
                "unknown scenario {:?}; supported: {}",
                self.scenario,
                SCENARIOS
                    .iter()
                    .map(|s| s.id)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        })?;
        if let Some(v) = self.scenario_version
            && v != info.version
        {
            return Err(LabError::InvalidSpec {
                field: "scenario_version",
                reason: format!("{} is version {}, not {v}", info.id, info.version),
            });
        }
        for name in self.parameters.keys() {
            if !info.parameters.iter().any(|p| p.name == name) {
                return Err(LabError::InvalidSpec {
                    field: "parameters",
                    reason: format!("{} has no parameter {name:?}", info.id),
                });
            }
        }
        let mut parameters = BTreeMap::new();
        for p in info.parameters {
            let value = self.parameters.get(p.name).copied().unwrap_or(p.default);
            if !(p.min..=p.max).contains(&value) {
                return Err(LabError::InvalidSpec {
                    field: "parameters",
                    reason: format!("{} must be between {} and {}", p.name, p.min, p.max),
                });
            }
            parameters.insert(p.name.to_owned(), value);
        }
        Ok((
            Self {
                scenario: info.id.to_owned(),
                scenario_version: Some(info.version),
                parameters,
            },
            info,
        ))
    }

    /// A parameter's value (after [`validated`](Self::validated)).
    #[must_use]
    pub fn parameter(&self, name: &str) -> u64 {
        self.parameters.get(name).copied().unwrap_or_default()
    }
}

/// How a case produced its failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureMode {
    /// A fault hook returned an error at the point (in-process, simulated).
    SimulatedError,
    /// A child process aborted itself at the point (`std::process::abort`).
    ProcessAbort,
    /// The parent killed a child process with `SIGKILL`.
    ProcessKilled,
    /// A child process was held at the point while the parent acted.
    ProcessPaused,
    /// Files were deliberately altered (the scenario's own copies only).
    DeliberateCorruption,
    /// Several child processes worked concurrently.
    ConcurrentProcesses,
}

impl FailureMode {
    /// Whether the failure involves a real OS process (as opposed to an
    /// error simulated inside the running process).
    #[must_use]
    pub fn is_real_process(self) -> bool {
        matches!(
            self,
            Self::ProcessAbort
                | Self::ProcessKilled
                | Self::ProcessPaused
                | Self::ConcurrentProcesses
        )
    }
}

/// One invariant check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvariantCheck {
    /// Stable name.
    pub name: String,
    /// What must hold.
    pub expected: String,
    /// What was observed.
    pub observed: String,
    /// Whether it held.
    pub held: bool,
}

impl InvariantCheck {
    /// A check that holds iff `held`.
    pub fn new(
        name: &str,
        expected: impl Into<String>,
        observed: impl Into<String>,
        held: bool,
    ) -> Self {
        Self {
            name: name.to_owned(),
            expected: expected.into(),
            observed: observed.into(),
            held,
        }
    }
}

/// Verdict of one case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseVerdict {
    /// The fault was observed and every invariant held.
    InvariantPreserved,
    /// At least one invariant did not hold.
    InvariantViolated,
    /// The fault was not observed, so the checks say nothing about it.
    Inconclusive,
}

/// Classifies a case (see the module documentation).
#[must_use]
pub fn classify(fault_observed: bool, checks: &[InvariantCheck]) -> CaseVerdict {
    if !fault_observed {
        CaseVerdict::Inconclusive
    } else if checks.iter().any(|c| !c.held) {
        CaseVerdict::InvariantViolated
    } else {
        CaseVerdict::InvariantPreserved
    }
}

/// One case of a scenario.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaseResult {
    /// Stable id within the scenario, e.g. `append_after_insert/process_abort`.
    pub id: String,
    /// Fault point, if the case uses one.
    pub fault_point: Option<String>,
    /// How the failure was produced.
    pub mode: FailureMode,
    /// Whether a real OS process failed or ran (see [`FailureMode::is_real_process`]).
    pub real_process: bool,
    /// Whether the planned fault was observed to happen.
    pub fault_observed: bool,
    /// How it was observed (or why not).
    pub fault_evidence: String,
    /// Invariant checks.
    pub invariants: Vec<InvariantCheck>,
    /// What recovery did (reopen, registry recovery, cleanup).
    pub recovery: Value,
    /// Integrity verification after recovery.
    pub integrity: Value,
    /// Verdict.
    pub verdict: CaseVerdict,
}

impl CaseResult {
    /// Builds a case and classifies it.
    #[must_use]
    // One argument per recorded field; a builder would add nothing.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: String,
        fault_point: Option<String>,
        mode: FailureMode,
        fault_observed: bool,
        fault_evidence: String,
        invariants: Vec<InvariantCheck>,
        recovery: Value,
        integrity: Value,
    ) -> Self {
        let verdict = classify(fault_observed, &invariants);
        Self {
            id,
            fault_point,
            mode,
            real_process: mode.is_real_process(),
            fault_observed,
            fault_evidence,
            invariants,
            recovery,
            integrity,
            verdict,
        }
    }
}

/// Overall verdict of a scenario run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverallVerdict {
    /// Every case preserved its invariants.
    InvariantsPreserved,
    /// At least one case violated an invariant.
    InvariantViolated,
    /// No violation, but at least one case was inconclusive.
    Inconclusive,
}

impl OverallVerdict {
    /// Stored outcome label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::InvariantsPreserved => "invariants_preserved",
            Self::InvariantViolated => "invariant_violated",
            Self::Inconclusive => "inconclusive",
        }
    }

    /// Combines case verdicts.
    #[must_use]
    pub fn of(cases: &[CaseResult]) -> Self {
        if cases
            .iter()
            .any(|c| c.verdict == CaseVerdict::InvariantViolated)
        {
            Self::InvariantViolated
        } else if cases.is_empty() || cases.iter().any(|c| c.verdict == CaseVerdict::Inconclusive) {
            Self::Inconclusive
        } else {
            Self::InvariantsPreserved
        }
    }
}

/// The result of a reliability experiment.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReliabilityResult {
    /// [`RESULT_VERSION`].
    pub result_version: u32,
    /// Scenario id, version and title.
    pub scenario: Value,
    /// Parameters used.
    pub parameters: BTreeMap<String, u64>,
    /// The synthetic input history: number of events and a digest of the
    /// requests (event types and payloads), identical on every run. Event
    /// ids and timestamps differ per run, so event digests do too.
    pub history: Value,
    /// Every case.
    pub cases: Vec<CaseResult>,
    /// Counts by verdict.
    pub summary: Value,
    /// Overall verdict.
    pub verdict: OverallVerdict,
    /// Failures produced.
    pub simulates: Vec<&'static str>,
    /// Failures not produced.
    pub does_not_simulate: Vec<&'static str>,
    /// General limits of the method.
    pub limitations: Vec<&'static str>,
}

/// Limits that apply to every scenario.
pub const LIMITATIONS: &[&str] = &[
    "passing cases show that these invariants held for these failures on this machine; they do not prove correctness for all failures",
    "process aborts and kills leave the operating system running, so data in OS caches survives; power loss is not simulated",
    "SQLite's own crash safety (journaling, WAL recovery) is relied on, not independently verified",
];

impl ReliabilityResult {
    /// Assembles a result from cases.
    #[must_use]
    pub fn new(
        info: &ScenarioInfo,
        spec: &ReliabilitySpec,
        history: Value,
        cases: Vec<CaseResult>,
    ) -> Self {
        let count = |v: CaseVerdict| cases.iter().filter(|c| c.verdict == v).count();
        let summary = json!({
            "cases": cases.len(),
            "invariant_preserved": count(CaseVerdict::InvariantPreserved),
            "invariant_violated": count(CaseVerdict::InvariantViolated),
            "inconclusive": count(CaseVerdict::Inconclusive),
            "real_process_cases": cases.iter().filter(|c| c.real_process).count(),
        });
        Self {
            result_version: RESULT_VERSION,
            scenario: json!({ "id": info.id, "version": info.version, "title": info.title }),
            parameters: spec.parameters.clone(),
            history,
            verdict: OverallVerdict::of(&cases),
            cases,
            summary,
            simulates: info.simulates.to_vec(),
            does_not_simulate: info.does_not_simulate.to_vec(),
            limitations: LIMITATIONS.to_vec(),
        }
    }
}

impl Lab {
    /// Validates a reliability spec and records a new experiment (status
    /// `created`). Running it needs the `orbit-reliability` tool.
    ///
    /// # Errors
    /// [`LabError::InvalidSpec`] or registry errors.
    pub fn create_reliability(&self, spec: &ReliabilitySpec) -> Result<ExperimentRecord> {
        let (spec, _) = spec.validated()?;
        let id = self.registry().create(&NewExperiment {
            kind: RELIABILITY,
            spec_version: SPEC_VERSION,
            spec: serde_json::to_value(&spec)?,
            stream: None,
            left: None,
            right: None,
            implementations: json!([]),
            build: BuildInfo::current(),
        })?;
        self.registry().get(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(held: bool) -> InvariantCheck {
        InvariantCheck::new("x", "e", "o", held)
    }

    #[test]
    fn classification_separates_violations_inconclusive_and_preserved() {
        assert_eq!(
            classify(true, &[check(true), check(true)]),
            CaseVerdict::InvariantPreserved
        );
        assert_eq!(
            classify(true, &[check(true), check(false)]),
            CaseVerdict::InvariantViolated
        );
        assert_eq!(classify(false, &[check(true)]), CaseVerdict::Inconclusive);
        // Checks written for the faulted path say nothing if it did not happen.
        assert_eq!(classify(false, &[check(false)]), CaseVerdict::Inconclusive);
    }

    #[test]
    fn overall_verdicts() {
        let case = |observed, held| {
            CaseResult::new(
                "c".into(),
                None,
                FailureMode::SimulatedError,
                observed,
                String::new(),
                vec![check(held)],
                Value::Null,
                Value::Null,
            )
        };
        assert_eq!(
            OverallVerdict::of(&[case(true, true), case(true, true)]),
            OverallVerdict::InvariantsPreserved
        );
        assert_eq!(
            OverallVerdict::of(&[case(true, true), case(false, true)]),
            OverallVerdict::Inconclusive
        );
        assert_eq!(
            OverallVerdict::of(&[case(false, true), case(true, false)]),
            OverallVerdict::InvariantViolated
        );
        assert_eq!(
            OverallVerdict::of(&[case(false, false)]),
            OverallVerdict::Inconclusive
        );
        assert_eq!(
            OverallVerdict::of(&[]),
            OverallVerdict::Inconclusive,
            "no cases prove nothing"
        );
    }

    #[test]
    fn specs_are_validated_against_the_catalog() {
        let ok = ReliabilitySpec::from_json(
            json!({ "scenario": "registry-concurrency", "parameters": { "processes": 3 } }),
        )
        .unwrap();
        let (v, info) = ok.validated().unwrap();
        assert_eq!(info.id, "registry-concurrency");
        assert_eq!(v.scenario_version, Some(1));
        assert_eq!(v.parameter("processes"), 3);
        assert_eq!(
            v.parameter("experiments_per_process"),
            5,
            "default filled in"
        );
        for bad in [
            json!({ "scenario": "rm-rf" }),
            json!({ "scenario": "registry-concurrency", "parameters": { "processes": 9 } }),
            json!({ "scenario": "registry-concurrency", "parameters": { "processes": 1 } }),
            json!({ "scenario": "registry-concurrency", "parameters": { "kill": 1 } }),
            json!({ "scenario": "registry-concurrency", "scenario_version": 7 }),
        ] {
            assert!(
                ReliabilitySpec::from_json(bad.clone())
                    .unwrap()
                    .validated()
                    .is_err(),
                "{bad}"
            );
        }
        assert!(ReliabilitySpec::from_json(json!({ "scenario": "x", "path": "/etc" })).is_err());
    }

    #[test]
    fn modes_say_whether_a_real_process_was_involved() {
        assert!(!FailureMode::SimulatedError.is_real_process());
        assert!(!FailureMode::DeliberateCorruption.is_real_process());
        assert!(FailureMode::ProcessAbort.is_real_process());
        assert!(FailureMode::ProcessKilled.is_real_process());
    }
}
