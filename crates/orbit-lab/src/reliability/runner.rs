//! Executes reliability scenarios (feature `reliability` only).
//!
//! Each case gets its own directory under the scenario's work directory.
//! Child processes are this tool re-executed as `orbit-reliability __child`
//! ([`super::child`]). Coordination uses lines on the children's stdin and
//! stdout (barriers, pause and resume), never sleeps. The only polling is
//! waiting for a child to exit, and every child has a watchdog that kills it
//! after [`CHILD_DEADLINE`].

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use orbit_core::fault::FaultPoint;
use orbit_core::projection::examples::Counter;
use orbit_core::{
    AppendRequest, Digest, Durability, EventStore, EventType, Metadata, ReconstructOptions,
    ReconstructionError, Reconstructor, SnapshotPolicy, StoreConfig, StoreError, StreamId, Target,
};
use serde_json::{Value, json};

use orbit_core::reconstruct::SnapshotStatus;

use super::child::STREAM;
use super::{
    CaseResult, FailureMode, InvariantCheck, RELIABILITY, ReliabilityResult, ReliabilitySpec,
    ScenarioInfo,
};
use crate::differential::RunFailure;
use crate::digest::{RESULT_TAG, value_digest};
use crate::error::{LabError, Result};
use crate::fault::LabFaultPoint;
use crate::registry::{
    ArtifactRef, Completion, ExperimentId, Failure, FailureKind, Status, VerificationKind,
    VerificationRecord,
};
use crate::{Catalog, DifferentialSpec, ExperimentRecord, Lab};

/// A child process is killed if it has not exited after this long.
pub const CHILD_DEADLINE: Duration = Duration::from_secs(120);
/// Most bytes of child output kept in the `child_output` artifact.
pub const MAX_CHILD_OUTPUT: usize = 256 * 1024;

const APPEND_POINTS: [FaultPoint; 5] = [
    FaultPoint::AppendBeforeBegin,
    FaultPoint::AppendAfterBegin,
    FaultPoint::AppendAfterSequenceAllocated,
    FaultPoint::AppendAfterInsert,
    FaultPoint::AppendBeforeCommit,
];
const SNAPSHOT_POINTS: [FaultPoint; 3] = [
    FaultPoint::SnapshotBeforeBegin,
    FaultPoint::SnapshotAfterValidation,
    FaultPoint::SnapshotBeforeCommit,
];

/// The experiment could not be carried out (never a reliability finding).
#[derive(Debug)]
pub struct Infra(pub String);

impl<E: std::fmt::Display> From<E> for Infra {
    fn from(e: E) -> Self {
        Self(e.to_string())
    }
}

type Infallible<T> = std::result::Result<T, Infra>;

fn snake(p: FaultPoint) -> String {
    let debug = format!("{p:?}");
    let mut out = String::new();
    for (i, c) in debug.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// The deterministic request list of the synthetic history.
fn requests(n: u64) -> Vec<(String, Value)> {
    (0..n)
        .map(|i| {
            if i % 5 == 4 {
                ("counter.decremented".to_owned(), json!({ "by": 9 }))
            } else {
                ("counter.incremented".to_owned(), json!({ "by": i % 3 + 1 }))
            }
        })
        .collect()
}

fn history(n: u64) -> Value {
    let list: Vec<Value> = requests(n)
        .into_iter()
        .map(|(t, p)| json!({ "type": t, "payload": p }))
        .collect();
    json!({
        "events": n,
        "requests_digest": value_digest("ORBIT-LAB-RELIABILITY-HISTORY-V1", &Value::Array(list)),
    })
}

fn stream() -> StreamId {
    StreamId::new(STREAM).unwrap_or_else(|_| unreachable!("valid stream id"))
}

fn open_store(db: &Path) -> std::result::Result<EventStore, StoreError> {
    EventStore::open(StoreConfig::new(db).with_durability(Durability::Full))
}

fn seed(db: &Path, n: u64) -> Infallible<()> {
    let store = open_store(db)?;
    store.create_stream(&stream(), Metadata::new())?;
    for (t, p) in requests(n) {
        store.append(&stream(), AppendRequest::new(EventType::new(t)?, p))?;
    }
    Ok(())
}

fn event_digests(store: &EventStore) -> std::result::Result<Vec<(u64, Digest)>, StoreError> {
    Ok(store
        .read_stream(&stream())?
        .iter()
        .map(|e| (e.sequence.get(), e.digest))
        .collect())
}

/// A running child process with a watchdog.
struct ChildProc {
    child: Arc<Mutex<Child>>,
    out: BufReader<ChildStdout>,
    input: Option<ChildStdin>,
    stdout_lines: Vec<String>,
    timed_out: Arc<AtomicBool>,
    done: Option<mpsc::Sender<()>>,
}

impl ChildProc {
    fn line(&mut self) -> Option<String> {
        let mut line = String::new();
        match self.out.read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => {
                let line = line.trim_end().to_owned();
                self.stdout_lines.push(line.clone());
                Some(line)
            }
        }
    }

    fn send(&mut self, line: &str) -> Infallible<()> {
        let input = self
            .input
            .as_mut()
            .ok_or_else(|| Infra("child stdin closed".into()))?;
        writeln!(input, "{line}")?;
        input.flush()?;
        Ok(())
    }

    fn kill(&self) {
        let _ = self
            .child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .kill();
    }

    /// Reads the remaining output and waits for exit. Returns the exit
    /// status and every stdout line.
    fn finish(mut self, ctx: &mut Ctx, label: &str) -> Infallible<(ExitStatus, Vec<String>)> {
        self.input = None;
        while self.line().is_some() {}
        let status = loop {
            let polled = self
                .child
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .try_wait()?;
            if let Some(status) = polled {
                break status;
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        drop(self.done.take());
        let mut stderr = String::new();
        if let Some(mut e) = self
            .child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stderr
            .take()
        {
            let _ = e.read_to_string(&mut stderr);
        }
        ctx.log(label, &self.stdout_lines, &stderr, status);
        if self.timed_out.load(Ordering::SeqCst) {
            return Err(Infra(format!(
                "child {label} did not finish within {CHILD_DEADLINE:?}"
            )));
        }
        Ok((status, self.stdout_lines))
    }
}

/// How a child ended.
fn ended(status: ExitStatus) -> (bool, bool, String) {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            let name = match signal {
                6 => "SIGABRT",
                9 => "SIGKILL",
                _ => "a signal",
            };
            return (
                signal == 6,
                signal == 9,
                format!("terminated by {name} ({signal})"),
            );
        }
    }
    (false, false, format!("exited with {status}"))
}

struct Ctx<'a> {
    exe: &'a Path,
    work: PathBuf,
    output: String,
}

impl Ctx<'_> {
    fn case_dir(&self, name: &str) -> Infallible<PathBuf> {
        let dir = self.work.join(name.replace('/', "-"));
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    fn spawn(&mut self, role: &Value) -> Infallible<ChildProc> {
        let mut child = Command::new(self.exe)
            .arg("__child")
            .arg(role.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Infra(format!("cannot start child process: {e}")))?;
        let out = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| Infra("no stdout".into()))?,
        );
        let input = child.stdin.take();
        let child = Arc::new(Mutex::new(child));
        let timed_out = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel::<()>();
        {
            let child = Arc::clone(&child);
            let timed_out = Arc::clone(&timed_out);
            std::thread::spawn(move || {
                if let Err(mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(CHILD_DEADLINE) {
                    timed_out.store(true, Ordering::SeqCst);
                    let _ = child
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .kill();
                }
            });
        }
        Ok(ChildProc {
            child,
            out,
            input,
            stdout_lines: Vec::new(),
            timed_out,
            done: Some(tx),
        })
    }

    fn log(&mut self, label: &str, stdout: &[String], stderr: &str, status: ExitStatus) {
        if self.output.len() >= MAX_CHILD_OUTPUT {
            return;
        }
        let work = self.work.display().to_string();
        let entry = format!(
            "== {label}: {}\n{}\n{}",
            ended(status).2,
            stdout.join("\n"),
            stderr.trim_end()
        )
        .replace(&work, "<work>");
        self.output.push_str(&entry);
        self.output.push('\n');
        self.output.truncate(MAX_CHILD_OUTPUT);
    }
}

fn fail_once(point: FaultPoint) -> orbit_core::fault::FaultHook {
    let fired = Arc::new(AtomicBool::new(false));
    Arc::new(move |p| p == point && !fired.swap(true, Ordering::SeqCst))
}

/// Checks after an interrupted append (or snapshot) that committed nothing.
fn store_checks(
    db: &Path,
    before: &[(u64, Digest)],
    checks: &mut Vec<InvariantCheck>,
) -> (Value, Value) {
    let store = match open_store(db) {
        Ok(store) => store,
        Err(e) => {
            checks.push(InvariantCheck::new(
                "store_reopens",
                "the store opens",
                e.to_string(),
                false,
            ));
            return (json!({ "reopened": false }), Value::Null);
        }
    };
    checks.push(InvariantCheck::new(
        "store_reopens",
        "the store opens",
        "opened",
        true,
    ));
    let n = before.len() as u64;
    match event_digests(&store) {
        Ok(after) => {
            checks.push(InvariantCheck::new(
                "nothing_committed",
                format!("head stays at {n}"),
                format!("head is {}", after.len()),
                after.len() as u64 == n,
            ));
            checks.push(InvariantCheck::new(
                "committed_events_unchanged",
                "every earlier event has its earlier digest",
                if after.get(..before.len()) == Some(before) {
                    "unchanged"
                } else {
                    "changed"
                },
                after.get(..before.len()) == Some(before),
            ));
            let dense = after
                .iter()
                .enumerate()
                .all(|(i, (s, _))| *s == i as u64 + 1);
            checks.push(InvariantCheck::new(
                "dense_sequences",
                "sequences 1..=head",
                if dense { "dense" } else { "gap" },
                dense,
            ));
        }
        Err(e) => checks.push(InvariantCheck::new(
            "events_readable",
            "events can be read",
            e.to_string(),
            false,
        )),
    }
    let integrity = verify(&store, checks);
    let next = store.append(
        &stream(),
        AppendRequest::new(
            EventType::new("counter.incremented").unwrap_or_else(|_| unreachable!()),
            json!({ "by": 1 }),
        ),
    );
    let observed = match &next {
        Ok(e) => format!("appended sequence {}", e.sequence),
        Err(e) => e.to_string(),
    };
    checks.push(InvariantCheck::new(
        "next_append_continues",
        format!("the next append gets sequence {}", n + 1),
        observed,
        next.is_ok_and(|e| e.sequence.get() == n + 1),
    ));
    (
        json!({ "reopened": true, "sqlite_recovery": "performed by SQLite when the file was opened" }),
        integrity,
    )
}

fn verify(store: &EventStore, checks: &mut Vec<InvariantCheck>) -> Value {
    match store.verify_store() {
        Ok(report) => {
            let issues: u64 = report.streams.iter().map(|s| s.total_issues).sum::<u64>()
                + report.snapshot_issues.len() as u64;
            checks.push(InvariantCheck::new(
                "store_verifies",
                "verify_store reports no issue",
                if report.is_valid() {
                    "valid".to_owned()
                } else {
                    format!("{issues} issues")
                },
                report.is_valid(),
            ));
            json!({
                "valid": report.is_valid(),
                "streams": report.streams.len(),
                "snapshots_checked": report.snapshots_checked,
                "issues": issues,
                "sqlite_integrity": report.sqlite_integrity,
            })
        }
        Err(e) => {
            checks.push(InvariantCheck::new(
                "store_verifies",
                "verify_store runs",
                e.to_string(),
                false,
            ));
            json!({ "valid": false, "error": e.to_string() })
        }
    }
}

fn append_case(
    ctx: &mut Ctx,
    n: u64,
    point: FaultPoint,
    simulated: bool,
) -> Infallible<CaseResult> {
    let mode = if simulated {
        FailureMode::SimulatedError
    } else {
        FailureMode::ProcessAbort
    };
    let id = format!(
        "{}/{}",
        snake(point),
        if simulated {
            "simulated_error"
        } else {
            "process_abort"
        }
    );
    let dir = ctx.case_dir(&id)?;
    let db = dir.join("orbit.db");
    seed(&db, n)?;
    let before = event_digests(&open_store(&db)?)?;
    let (observed, evidence) = if simulated {
        let store = open_store(&db)?;
        store.set_fault_hook(Some(fail_once(point)));
        let ty = EventType::new("counter.incremented")?;
        match store.append(&stream(), AppendRequest::new(ty, json!({ "by": 1 }))) {
            Err(StoreError::InjectedFault(p)) if p == point => {
                (true, format!("append returned InjectedFault({p})"))
            }
            Ok(e) => (false, format!("append committed sequence {}", e.sequence)),
            Err(e) => (false, format!("append failed differently: {e}")),
        }
    } else {
        let child =
            ctx.spawn(&json!({ "role": "append_abort", "db": db, "point": point.to_string() }))?;
        let (status, lines) = child.finish(ctx, &id)?;
        let (aborted, _, how) = ended(status);
        let acknowledged = lines.iter().any(|l| l.starts_with("not-faulted"));
        (
            aborted && !acknowledged,
            format!("child {how}; it did not report a completed append"),
        )
    };
    let mut checks = Vec::new();
    let (recovery, integrity) = store_checks(&db, &before, &mut checks);
    Ok(CaseResult::new(
        id,
        Some(point.to_string()),
        mode,
        observed,
        evidence,
        checks,
        recovery,
        integrity,
    ))
}

fn kill_case(ctx: &mut Ctx, n: u64) -> Infallible<CaseResult> {
    let id = "continuous_appends/process_killed".to_owned();
    let dir = ctx.case_dir(&id)?;
    let db = dir.join("orbit.db");
    seed(&db, n)?;
    let before = event_digests(&open_store(&db)?)?;
    let mut child = ctx.spawn(&json!({ "role": "append_forever", "db": db }))?;
    let mut acknowledged = 0u64;
    for _ in 0..5 {
        match child
            .line()
            .and_then(|l| l.strip_prefix("committed ").and_then(|s| s.parse().ok()))
        {
            Some(seq) => acknowledged = seq,
            None => break,
        }
    }
    child.kill();
    let (status, _) = child.finish(ctx, &id)?;
    let (_, killed, how) = ended(status);
    let mut checks = Vec::new();
    let store = open_store(&db);
    let integrity = match store {
        Ok(store) => {
            let after = event_digests(&store).unwrap_or_default();
            let head = after.len() as u64;
            checks.push(InvariantCheck::new(
                "acknowledged_events_survive",
                format!("head ≥ {acknowledged}, the last acknowledged sequence"),
                format!("head is {head}"),
                acknowledged > 0 && head >= acknowledged,
            ));
            checks.push(InvariantCheck::new(
                "earlier_events_unchanged",
                "the seeded events keep their digests",
                if after.get(..before.len()) == Some(&before[..]) {
                    "unchanged"
                } else {
                    "changed"
                },
                after.get(..before.len()) == Some(&before[..]),
            ));
            let dense = after
                .iter()
                .enumerate()
                .all(|(i, (s, _))| *s == i as u64 + 1);
            checks.push(InvariantCheck::new(
                "dense_sequences",
                "sequences 1..=head",
                if dense { "dense" } else { "gap" },
                dense,
            ));
            verify(&store, &mut checks)
        }
        Err(e) => {
            checks.push(InvariantCheck::new(
                "store_reopens",
                "the store opens",
                e.to_string(),
                false,
            ));
            Value::Null
        }
    };
    Ok(CaseResult::new(
        id,
        None,
        FailureMode::ProcessKilled,
        killed && acknowledged > 0,
        format!("child acknowledged sequence {acknowledged}, then was {how}"),
        checks,
        json!({ "reopened": true, "acknowledged": acknowledged }),
        integrity,
    ))
}

fn corruption_case(ctx: &mut Ctx, n: u64) -> Infallible<CaseResult> {
    let id = "event_payload_edited/deliberate_corruption".to_owned();
    let dir = ctx.case_dir(&id)?;
    let db = dir.join("orbit.db");
    seed(&db, n)?;
    // Only this scenario's own copy is edited.
    let changed = {
        let raw = rusqlite::Connection::open(&db)?;
        raw.execute_batch("DROP TRIGGER events_no_update")?;
        raw.execute(
            "UPDATE events SET payload = '{\"by\":999}' WHERE stream_id = ?1 AND sequence = 1",
            [STREAM],
        )?
    };
    let store = open_store(&db)?;
    let mut checks = Vec::new();
    let report = store.verify_store()?;
    checks.push(InvariantCheck::new(
        "corruption_detected",
        "verify_store reports the edited event",
        if report.is_valid() {
            "valid (not detected)".to_owned()
        } else {
            format!("{} invalid stream(s)", report.invalid_streams().count())
        },
        !report.is_valid(),
    ));
    let lab = Lab::open(&dir.join("lab"), Catalog::builtin())?;
    let spec = DifferentialSpec::from_json(
        json!({ "stream": STREAM, "left": "orbit.counter", "right": "orbit.counter" }),
    )?;
    let record = lab.create_and_run(&spec, &store)?;
    let kind = record.failure.as_ref().map(|f| f.kind);
    checks.push(InvariantCheck::new(
        "replay_refuses_damaged_history",
        "a differential replay over it fails with integrity_failure",
        format!("{} {:?}", record.status, kind),
        record.status == Status::Failed && kind == Some(FailureKind::IntegrityFailure),
    ));
    Ok(CaseResult::new(
        id,
        None,
        FailureMode::DeliberateCorruption,
        changed == 1,
        format!("{changed} event row edited with the append-only trigger dropped"),
        checks,
        Value::Null,
        json!({ "valid": report.is_valid(), "categories": report.streams.iter().flat_map(|s| s.categories()).collect::<Vec<_>>() }),
    ))
}

fn snapshot_case(
    ctx: &mut Ctx,
    n: u64,
    point: FaultPoint,
    simulated: bool,
) -> Infallible<CaseResult> {
    let mode = if simulated {
        FailureMode::SimulatedError
    } else {
        FailureMode::ProcessAbort
    };
    let id = format!(
        "{}/{}",
        snake(point),
        if simulated {
            "simulated_error"
        } else {
            "process_abort"
        }
    );
    let dir = ctx.case_dir(&id)?;
    let db = dir.join("orbit.db");
    seed(&db, n)?;
    let (observed, evidence) = if simulated {
        let store = open_store(&db)?;
        store.set_fault_hook(Some(fail_once(point)));
        match Reconstructor::new(&store, Counter).create_snapshot(
            &stream(),
            Target::Latest,
            ReconstructOptions::default(),
        ) {
            Err(ReconstructionError::Store(StoreError::InjectedFault(p))) if p == point => {
                (true, format!("create_snapshot returned InjectedFault({p})"))
            }
            Ok(_) => (false, "the snapshot was saved".to_owned()),
            Err(e) => (false, format!("create_snapshot failed differently: {e}")),
        }
    } else {
        let child =
            ctx.spawn(&json!({ "role": "snapshot_abort", "db": db, "point": point.to_string() }))?;
        let (status, lines) = child.finish(ctx, &id)?;
        let (aborted, _, how) = ended(status);
        let completed = lines.iter().any(|l| l.starts_with("not-faulted"));
        (aborted && !completed, format!("child {how}"))
    };
    let mut checks = Vec::new();
    let store = open_store(&db)?;
    let snapshots = store.list_snapshots(&stream())?;
    checks.push(InvariantCheck::new(
        "no_snapshot_stored",
        "no snapshot",
        format!("{} snapshot(s)", snapshots.len()),
        snapshots.is_empty(),
    ));
    let integrity = verify(&store, &mut checks);
    let r = Reconstructor::new(&store, Counter);
    let equivalent = r
        .compare_with_full_replay(&stream(), Target::Latest, SnapshotPolicy::Prefer)
        .map(|e| e.equivalent);
    checks.push(InvariantCheck::new(
        "reconstruction_matches_full_replay",
        "snapshot-assisted reconstruction equals full replay",
        format!("{equivalent:?}"),
        matches!(equivalent, Ok(true)),
    ));
    let next = r
        .create_snapshot(&stream(), Target::Latest, ReconstructOptions::default())
        .and_then(|c| r.check_snapshot(c.saved.snapshot.id));
    checks.push(InvariantCheck::new(
        "next_snapshot_valid",
        "a later snapshot saves and validates",
        match &next {
            Ok(SnapshotStatus::Usable { .. }) => "usable".to_owned(),
            Ok(other) => format!("{other:?}"),
            Err(e) => e.to_string(),
        },
        matches!(next, Ok(SnapshotStatus::Usable { .. })),
    ));
    Ok(CaseResult::new(
        id,
        Some(point.to_string()),
        mode,
        observed,
        evidence,
        checks,
        json!({ "reopened": true }),
        integrity,
    ))
}

/// Baseline evidence that must survive later interruptions.
struct Baseline {
    record: ExperimentRecord,
}

fn differential_spec() -> Infallible<DifferentialSpec> {
    Ok(DifferentialSpec::from_json(json!({
        "stream": STREAM,
        "left": "orbit.counter",
        "right": "example.counter-floor-at-zero",
    }))?)
}

fn lab_checks(
    lab: &Lab,
    store: &EventStore,
    id: ExperimentId,
    baseline: &Baseline,
    checks: &mut Vec<InvariantCheck>,
    expect: Status,
) -> Infallible<Value> {
    let record = lab.experiment(id)?;
    let interrupted = record.status == Status::Failed
        && record
            .failure
            .as_ref()
            .is_some_and(|f| f.kind == FailureKind::Interrupted)
        && record.result.is_none();
    match expect {
        Status::Failed => {
            checks.push(InvariantCheck::new(
                "interrupted_not_completed",
                "failed/interrupted, without a result",
                format!(
                    "{} {:?}",
                    record.status,
                    record.failure.as_ref().map(|f| f.kind)
                ),
                interrupted,
            ));
            checks.push(InvariantCheck::new(
                "no_artifact_links",
                "no artifact attached",
                format!("{} attached", record.artifacts.len()),
                record.artifacts.is_empty(),
            ));
        }
        _ => checks.push(InvariantCheck::new(
            "completed_after_resume",
            "completed, with a result",
            format!("{} result: {}", record.status, record.result.is_some()),
            record.status == Status::Completed && record.result.is_some(),
        )),
    }
    let lock = lab.dir().join("locks").join(format!("{id}.lock"));
    checks.push(InvariantCheck::new(
        "lock_released",
        "no lock file remains",
        if lock.exists() { "present" } else { "absent" },
        !lock.exists(),
    ));
    let base = lab.experiment(baseline.record.id)?;
    checks.push(InvariantCheck::new(
        "earlier_result_preserved",
        "the earlier completed experiment is unchanged",
        format!("{} {:?}", base.status, base.result_digest),
        base.status == Status::Completed && base.result_digest == baseline.record.result_digest,
    ));
    let (report, _) = lab.verify_artifacts(baseline.record.id)?;
    checks.push(InvariantCheck::new(
        "earlier_artifacts_intact",
        "verify reports intact",
        report.status.clone(),
        report.status == "intact",
    ));
    let reproduced = lab.reproduce(baseline.record.id, store)?;
    checks.push(InvariantCheck::new(
        "earlier_result_reproduces",
        "reproduce reports reproduced",
        reproduced.status.clone(),
        reproduced.status == "reproduced",
    ));
    let temporaries = lab.artifacts().temporary_files()?;
    checks.push(InvariantCheck::new(
        "no_temporary_files_left",
        "no temporary file after recovery",
        format!("{temporaries}"),
        temporaries == 0,
    ));
    let stored = lab.artifacts().stored_digests()?;
    let intact = stored
        .iter()
        .filter(|d| lab.artifacts().read(d).is_ok())
        .count();
    checks.push(InvariantCheck::new(
        "stored_files_intact",
        "every file under an artifact name has that content",
        format!("{intact} of {} intact", stored.len()),
        intact == stored.len(),
    ));
    let linked = lab.registry().linked_artifact_digests()?;
    let unlinked = stored.iter().filter(|d| !linked.contains(d)).count();
    Ok(json!({ "status": record.status, "unlinked_artifact_files": unlinked }))
}

fn experiment_interruption(ctx: &mut Ctx, n: u64) -> Infallible<Vec<CaseResult>> {
    let dir = ctx.case_dir("lab")?;
    let db = dir.join("orbit.db");
    let lab_dir = dir.join("lab");
    seed(&db, n)?;
    let store = open_store(&db)?;
    let lab = Lab::open(&lab_dir, Catalog::builtin())?;
    let spec = differential_spec()?;
    let baseline = Baseline {
        record: lab.create_and_run(&spec, &store)?,
    };
    if baseline.record.status != Status::Completed {
        return Err(Infra("the baseline experiment did not complete".into()));
    }
    let mut cases = Vec::new();
    for (i, point) in LabFaultPoint::ALL.into_iter().enumerate() {
        let id = format!("{}/process_abort", point.as_str());
        // A different range per case gives new artifact content, so an
        // artifact write really happens (identical content is deduplicated
        // and skipped).
        let mut interrupted = spec.clone();
        interrupted.to = Some(n - 1 - i as u64);
        let created = lab.create(&interrupted)?;
        let child = ctx.spawn(&json!({
            "role": "lab_run", "db": db, "lab": lab_dir, "experiment": created.id.to_string(),
            "point": point.as_str(), "action": "abort",
        }))?;
        let (status, lines) = child.finish(ctx, &id)?;
        let (aborted, _, how) = ended(status);
        let aborted = aborted
            && !lines
                .iter()
                .any(|l| l.starts_with("done") || l.starts_with("error"));
        let temporaries_before = lab.artifacts().temporary_files()?;
        // A new process opening the lab performs recovery.
        let reopened = Lab::open(&lab_dir, Catalog::builtin())?;
        let mut checks = Vec::new();
        let mut recovery = lab_checks(
            &reopened,
            &store,
            created.id,
            &baseline,
            &mut checks,
            Status::Failed,
        )?;
        recovery["temporary_files_before_recovery"] = json!(temporaries_before);
        if point == LabFaultPoint::ArtifactAfterPartialWrite {
            checks.push(InvariantCheck::new(
                "partial_write_left_only_a_temporary_file",
                "the half-written file exists only as a temporary file before recovery",
                format!("{temporaries_before} temporary file(s)"),
                temporaries_before >= 1,
            ));
        }
        let (report, _) = reopened.verify_artifacts(created.id)?;
        cases.push(CaseResult::new(
            id,
            Some(point.as_str().to_owned()),
            FailureMode::ProcessAbort,
            aborted,
            format!("child {how} at {point}"),
            checks,
            recovery,
            json!({ "experiment_record": report.status, "spec_digest_ok": report.spec_digest_ok }),
        ));
    }

    // Simulated error before the result is recorded, in this process.
    {
        let id = "run_before_complete/simulated_error".to_owned();
        let created = lab.create(&spec)?;
        lab.set_fault_hook(Some(Arc::new(|p| p == LabFaultPoint::RunBeforeComplete)));
        let record = lab.run(created.id, &store);
        lab.set_fault_hook(None);
        let record = record?;
        let injected = record
            .failure
            .as_ref()
            .is_some_and(|f| f.message.contains("injected fault"));
        let mut checks = vec![InvariantCheck::new(
            "failed_not_completed",
            "failed/internal, without a result or artifact links",
            format!(
                "{} {:?} links: {}",
                record.status,
                record.failure.as_ref().map(|f| f.kind),
                record.artifacts.len()
            ),
            record.status == Status::Failed
                && record.result.is_none()
                && record.artifacts.is_empty(),
        )];
        let mut scratch = Vec::new();
        let recovery = lab_checks(
            &lab,
            &store,
            created.id,
            &baseline,
            &mut scratch,
            Status::Completed,
        )?;
        // Only the checks about other evidence apply here.
        checks.extend(
            scratch
                .into_iter()
                .filter(|c| c.name != "completed_after_resume"),
        );
        cases.push(CaseResult::new(
            id,
            Some(LabFaultPoint::RunBeforeComplete.as_str().to_owned()),
            FailureMode::SimulatedError,
            injected,
            record
                .failure
                .as_ref()
                .map_or_else(String::new, |f| f.message.clone()),
            checks,
            recovery,
            Value::Null,
        ));
    }

    // A live experiment must not be recovered by another process.
    {
        let id = "run_after_start/process_paused".to_owned();
        let created = lab.create(&spec)?;
        let mut child = ctx.spawn(&json!({
            "role": "lab_run", "db": db, "lab": lab_dir, "experiment": created.id.to_string(),
            "point": LabFaultPoint::RunAfterStart.as_str(), "action": "pause",
        }))?;
        let paused = child.line().as_deref() == Some("paused");
        let mut checks = Vec::new();
        let observer = Lab::open(&lab_dir, Catalog::builtin())?;
        let while_paused = observer.experiment(created.id)?.status;
        checks.push(InvariantCheck::new(
            "live_experiment_not_interrupted",
            "still running while its process is alive",
            while_paused.to_string(),
            while_paused == Status::Running,
        ));
        child.send("continue")?;
        let done = child.line();
        let (status, _) = child.finish(ctx, &id)?;
        let recovery = lab_checks(
            &observer,
            &store,
            created.id,
            &baseline,
            &mut checks,
            Status::Completed,
        )?;
        cases.push(CaseResult::new(
            id,
            Some(LabFaultPoint::RunAfterStart.as_str().to_owned()),
            FailureMode::ProcessPaused,
            paused,
            format!("child paused, another process opened the lab, the child resumed and reported {done:?}; {}", ended(status).2),
            checks,
            recovery,
            Value::Null,
        ));
    }

    // A completed artifact corrupted on disk.
    {
        let id = "completed_artifact_edited/deliberate_corruption".to_owned();
        let trace = baseline
            .record
            .artifacts
            .iter()
            .find(|a| a.role == "left_trace")
            .ok_or_else(|| Infra("baseline has no left_trace".into()))?;
        let path = lab.artifacts().local_path(&trace.digest);
        let mut bytes = std::fs::read(&path)?;
        bytes[0] ^= 0xff;
        std::fs::write(&path, &bytes)?;
        let mut checks = Vec::new();
        let (report, _) = lab.verify_artifacts(baseline.record.id)?;
        checks.push(InvariantCheck::new(
            "corruption_reported",
            "verify reports damaged",
            report.status.clone(),
            report.status == "damaged",
        ));
        let read = lab.artifact(baseline.record.id, "left_trace");
        checks.push(InvariantCheck::new(
            "corrupted_artifact_never_read",
            "reading it fails with ArtifactCorrupted",
            match &read {
                Ok(_) => "read succeeded".to_owned(),
                Err(e) => e.to_string(),
            },
            matches!(read, Err(LabError::ArtifactCorrupted(_))),
        ));
        let trace_page = lab.trace(baseline.record.id, None, 10);
        checks.push(InvariantCheck::new(
            "trace_refused",
            "the trace endpoint refuses it",
            if trace_page.is_ok() {
                "served"
            } else {
                "refused"
            },
            trace_page.is_err(),
        ));
        cases.push(CaseResult::new(
            id,
            None,
            FailureMode::DeliberateCorruption,
            true,
            "first byte of the earlier experiment's left_trace file flipped".to_owned(),
            checks,
            Value::Null,
            json!({ "artifacts": report.artifacts.iter().map(|a| json!([a.artifact.role, a.state])).collect::<Vec<_>>() }),
        ));
    }
    Ok(cases)
}

fn barrier(ctx: &mut Ctx, children: &mut [ChildProc]) -> Infallible<bool> {
    let ready = children
        .iter_mut()
        .all(|c| c.line().as_deref() == Some("ready"));
    for c in children.iter_mut() {
        c.send("go")?;
    }
    let _ = ctx;
    Ok(ready)
}

fn registry_concurrency(ctx: &mut Ctx, processes: u64, per: u64) -> Infallible<Vec<CaseResult>> {
    let dir = ctx.case_dir("registry")?;
    let db = dir.join("orbit.db");
    let lab_dir = dir.join("lab");
    seed(&db, 6)?;
    let store = open_store(&db)?;
    let lab = Lab::open(&lab_dir, Catalog::builtin())?;
    let mut cases = Vec::new();

    // A: concurrent create-and-run.
    {
        let id = "create_and_run/concurrent_processes".to_owned();
        let mut children = (0..processes)
            .map(|_| ctx.spawn(&json!({ "role": "registry_worker", "db": db, "lab": lab_dir, "stream": STREAM, "count": per })))
            .collect::<Infallible<Vec<_>>>()?;
        let ready = barrier(ctx, &mut children)?;
        let mut reported = Vec::new();
        let mut errors = Vec::new();
        let mut clean = true;
        for (i, mut child) in children.into_iter().enumerate() {
            let line = child.line().unwrap_or_default();
            let (status, _) = child.finish(ctx, &format!("{id}#{i}"))?;
            clean &= status.success();
            let v: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
            reported.extend(
                v["ids"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|x| x.as_str().map(str::to_owned)),
            );
            errors.extend(v["errors"].as_array().cloned().unwrap_or_default());
        }
        let all = lab.list(10_000, None)?;
        let ids: BTreeSet<String> = all.iter().map(|s| s.id.to_string()).collect();
        let reported_set: BTreeSet<String> = reported.iter().cloned().collect();
        let expected = processes * per;
        let mut checks = vec![
            InvariantCheck::new(
                "no_errors",
                "no process reported an error",
                format!("{} error(s)", errors.len()),
                errors.is_empty(),
            ),
            InvariantCheck::new(
                "none_lost_or_duplicated",
                format!("{expected} experiments, all distinct"),
                format!(
                    "{} reported, {} distinct, {} in the registry",
                    reported.len(),
                    reported_set.len(),
                    ids.len()
                ),
                reported.len() as u64 == expected
                    && reported_set.len() as u64 == expected
                    && ids == reported_set,
            ),
        ];
        let completed = all.iter().filter(|s| s.status == Status::Completed).count();
        checks.push(InvariantCheck::new(
            "all_completed",
            "every experiment completed",
            format!("{completed} of {}", all.len()),
            completed == all.len(),
        ));
        let mut digests = BTreeSet::new();
        let mut intact = 0;
        for s in &all {
            let r = lab.experiment(s.id)?;
            digests.insert(r.result_digest);
            if lab.verify_artifacts(s.id)?.0.status == "intact" {
                intact += 1;
            }
        }
        checks.push(InvariantCheck::new(
            "artifacts_intact",
            "every experiment's artifacts verify",
            format!("{intact} of {}", all.len()),
            intact == all.len(),
        ));
        checks.push(InvariantCheck::new(
            "identical_results",
            "identical inputs give one result digest",
            format!("{} distinct", digests.len()),
            digests.len() == 1,
        ));
        let linked = lab.registry().linked_artifact_digests()?;
        let rows = lab.registry().artifact_count()?;
        checks.push(InvariantCheck::new(
            "artifact_rows_deduplicated",
            "one artifact row per distinct content",
            format!("{rows} rows, {} distinct linked", linked.len()),
            rows as usize == linked.len(),
        ));
        cases.push(CaseResult::new(
            id,
            None,
            FailureMode::ConcurrentProcesses,
            ready && clean,
            format!("{processes} processes released together by a barrier; all exited successfully: {clean}"),
            checks,
            Value::Null,
            json!({ "experiments": all.len() }),
        ));
    }

    // B and C: race to start one experiment, then race to recover it.
    let contested = lab.create(&differential_spec()?)?;
    {
        let id = "start/concurrent_processes".to_owned();
        let mut children = (0..processes)
            .map(|_| ctx.spawn(&json!({ "role": "start_race", "lab": lab_dir, "experiment": contested.id.to_string() })))
            .collect::<Infallible<Vec<_>>>()?;
        let ready = barrier(ctx, &mut children)?;
        let mut answers = Vec::new();
        let mut clean = true;
        for (i, mut child) in children.into_iter().enumerate() {
            answers.push(child.line().unwrap_or_default());
            clean &= child.finish(ctx, &format!("{id}#{i}"))?.0.success();
        }
        let won = answers.iter().filter(|a| *a == "won").count();
        let lost = answers
            .iter()
            .filter(|a| *a == "lost invalid_transition")
            .count();
        let status = lab.experiment(contested.id)?.status;
        cases.push(CaseResult::new(
            id,
            None,
            FailureMode::ConcurrentProcesses,
            ready && clean,
            format!("answers: {answers:?}"),
            vec![
                InvariantCheck::new(
                    "exactly_one_start",
                    "one process starts it; the others get InvalidTransition",
                    format!("{won} won, {lost} lost"),
                    won == 1 && lost as u64 == processes - 1,
                ),
                InvariantCheck::new(
                    "status_running",
                    "running",
                    status.to_string(),
                    status == Status::Running,
                ),
            ],
            Value::Null,
            Value::Null,
        ));
    }
    {
        // The winner exited without running it: interrupted work with no owner.
        let id = "recover/concurrent_processes".to_owned();
        let mut children = (0..processes)
            .map(|_| ctx.spawn(&json!({ "role": "recover_race", "lab": lab_dir })))
            .collect::<Infallible<Vec<_>>>()?;
        let ready = barrier(ctx, &mut children)?;
        let mut recoveries = 0usize;
        let mut errors = 0usize;
        let mut clean = true;
        for (i, mut child) in children.into_iter().enumerate() {
            let v: Value =
                serde_json::from_str(&child.line().unwrap_or_default()).unwrap_or(Value::Null);
            recoveries += v["recovered"].as_array().map_or(0, |a| {
                a.iter()
                    .filter(|x| x.as_str() == Some(&contested.id.to_string()))
                    .count()
            });
            errors += usize::from(v.get("error").is_some());
            clean &= child.finish(ctx, &format!("{id}#{i}"))?.0.success();
        }
        let record = lab.experiment(contested.id)?;
        cases.push(CaseResult::new(
            id,
            None,
            FailureMode::ConcurrentProcesses,
            ready && clean,
            format!("{processes} processes recovered concurrently"),
            vec![
                InvariantCheck::new(
                    "exactly_one_recovery",
                    "one process marks it interrupted",
                    format!("{recoveries} recoveries, {errors} errors"),
                    recoveries == 1 && errors == 0,
                ),
                InvariantCheck::new(
                    "classified_interrupted",
                    "failed/interrupted",
                    format!(
                        "{} {:?}",
                        record.status,
                        record.failure.as_ref().map(|f| f.kind)
                    ),
                    record.status == Status::Failed
                        && record
                            .failure
                            .as_ref()
                            .is_some_and(|f| f.kind == FailureKind::Interrupted),
                ),
            ],
            Value::Null,
            Value::Null,
        ));
    }
    drop(store);
    Ok(cases)
}

/// Runs the scenario of `spec` in `work`; returns the history and cases.
fn scenario_cases(
    ctx: &mut Ctx,
    spec: &ReliabilitySpec,
    info: &ScenarioInfo,
) -> Infallible<(Value, Vec<CaseResult>)> {
    match info.id {
        "event-append-interruption" => {
            let n = spec.parameter("events");
            let mut cases = Vec::new();
            for point in APPEND_POINTS {
                cases.push(append_case(ctx, n, point, true)?);
                cases.push(append_case(ctx, n, point, false)?);
            }
            cases.push(kill_case(ctx, n)?);
            cases.push(corruption_case(ctx, n)?);
            Ok((history(n), cases))
        }
        "snapshot-write-interruption" => {
            let n = spec.parameter("events");
            let mut cases = Vec::new();
            for point in SNAPSHOT_POINTS {
                cases.push(snapshot_case(ctx, n, point, true)?);
                cases.push(snapshot_case(ctx, n, point, false)?);
            }
            Ok((history(n), cases))
        }
        "experiment-interruption" => {
            let n = spec.parameter("events");
            Ok((history(n), experiment_interruption(ctx, n)?))
        }
        "registry-concurrency" => Ok((
            history(6),
            registry_concurrency(
                ctx,
                spec.parameter("processes"),
                spec.parameter("experiments_per_process"),
            )?,
        )),
        other => Err(Infra(format!("scenario {other} has no implementation"))),
    }
}

/// Executes a scenario in a fresh work directory and removes it afterwards.
fn execute_scenario(
    lab: &Lab,
    work: PathBuf,
    exe: &Path,
    spec: &ReliabilitySpec,
    info: &ScenarioInfo,
) -> Infallible<(ReliabilityResult, String)> {
    if work.exists() {
        std::fs::remove_dir_all(&work)?;
    }
    std::fs::create_dir_all(&work)
        .map_err(|e| Infra(format!("cannot create the work directory: {e}")))?;
    let _ = lab;
    let mut ctx = Ctx {
        exe,
        work: work.clone(),
        output: String::new(),
    };
    let outcome = scenario_cases(&mut ctx, spec, info);
    let _ = std::fs::remove_dir_all(&work);
    let (history, cases) = outcome?;
    Ok((
        ReliabilityResult::new(info, spec, history, cases),
        ctx.output,
    ))
}

fn work_dir(lab: &Lab, name: &str) -> PathBuf {
    lab.dir().join("reliability-work").join(name)
}

fn record_spec(record: &ExperimentRecord) -> Infallible<(ReliabilitySpec, &'static ScenarioInfo)> {
    let spec = ReliabilitySpec::from_json(record.spec.clone())?;
    let (validated, info) = spec.validated()?;
    if validated != spec {
        return Err(Infra(
            "the stored specification is not in validated form".into(),
        ));
    }
    Ok((spec, info))
}

/// Runs a `created` reliability experiment. `exe` is the `orbit-reliability`
/// executable used for child processes.
///
/// # Errors
/// [`LabError::InvalidState`] for another kind, or registry errors.
/// Scenario setup problems fail the experiment with kind `infrastructure`.
pub fn run(lab: &Lab, id: ExperimentId, exe: &Path) -> Result<ExperimentRecord> {
    let record = lab.experiment(id)?;
    if record.kind != RELIABILITY {
        return Err(LabError::InvalidState {
            id,
            reason: "not a reliability experiment".to_owned(),
        });
    }
    lab.run_locked(id, |record| {
        let infra = |e: Infra| RunFailure::from(Failure::new(FailureKind::Infrastructure, e.0));
        let (spec, info) = record_spec(record).map_err(infra)?;
        let (result, output) =
            execute_scenario(lab, work_dir(lab, &id.to_string()), exe, &spec, info)
                .map_err(infra)?;
        let mut artifacts = Vec::new();
        if !output.is_empty() {
            let digest = lab
                .artifacts()
                .put(output.as_bytes())
                .map_err(|e| infra(Infra(e.to_string())))?;
            artifacts.push(ArtifactRef {
                role: "child_output".to_owned(),
                digest,
                media_type: "text/plain".to_owned(),
                size: output.len() as u64,
            });
        }
        let value = serde_json::to_value(&result).map_err(|e| infra(Infra(e.to_string())))?;
        Ok(Completion {
            input: None,
            outcome: result.verdict.label().to_owned(),
            result_digest: value_digest(RESULT_TAG, &value),
            result: value,
            reproducibility: "unverified".to_owned(),
            artifacts,
            elapsed_micros: 0,
        })
    })
}

/// The comparable part of a result: per case, its verdict, whether the fault
/// was observed, and which invariants held.
fn signature(result: &Value) -> Vec<Value> {
    result["cases"]
        .as_array()
        .map(|cases| {
            cases
                .iter()
                .map(|c| {
                    json!({
                        "id": c["id"],
                        "verdict": c["verdict"],
                        "fault_observed": c["fault_observed"],
                        "invariants": c["invariants"].as_array().map(|i| i.iter().map(|x| json!([x["name"], x["held"]])).collect::<Vec<_>>()),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Runs a completed reliability experiment again, in a new work directory,
/// and compares case verdicts, observed faults and held invariants. The
/// original record is never modified; the attempt is appended as a
/// verification (`reproduced`, `differs`, `configuration_mismatch` or
/// `not_reproducible` when the rerun could not be carried out).
///
/// # Errors
/// [`LabError::InvalidState`] unless a completed reliability experiment.
pub fn reproduce(lab: &Lab, id: ExperimentId, exe: &Path) -> Result<VerificationRecord> {
    let record = lab.experiment(id)?;
    let (Status::Completed, true, Some(recorded)) = (
        record.status,
        record.kind == RELIABILITY,
        record.result.as_ref(),
    ) else {
        return Err(LabError::InvalidState {
            id,
            reason: "only completed reliability experiments can be reproduced here".to_owned(),
        });
    };
    let (status, detail) = match record_spec(&record) {
        Err(e) => ("configuration_mismatch", json!({ "reason": e.0 })),
        Ok((spec, info)) => {
            let name = format!("{id}-reproduction-{}", uuid::Uuid::now_v7());
            match execute_scenario(lab, work_dir(lab, &name), exe, &spec, info) {
                Err(e) => (
                    "not_reproducible",
                    json!({ "reason": format!("the rerun could not be carried out: {}", e.0) }),
                ),
                Ok((result, _)) => {
                    let new = serde_json::to_value(&result)?;
                    let (old_sig, new_sig) = (signature(recorded), signature(&new));
                    let differences: Vec<Value> = old_sig
                        .iter()
                        .zip(&new_sig)
                        .filter(|(a, b)| a != b)
                        .map(|(a, _)| a["id"].clone())
                        .collect();
                    let same = old_sig.len() == new_sig.len() && differences.is_empty();
                    (
                        if same { "reproduced" } else { "differs" },
                        json!({
                            "compared": "case verdicts, observed faults and held invariants",
                            "recorded_verdict": recorded["verdict"],
                            "reproduced_verdict": result.verdict,
                            "differences": differences,
                            "cases": [old_sig.len(), new_sig.len()],
                        }),
                    )
                }
            }
        }
    };
    lab.registry()
        .add_verification(id, VerificationKind::Reproduction, status, &detail)
}
