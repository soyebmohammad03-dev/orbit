//! Launching reliability experiments in a separate, explicitly configured
//! runner process.
//!
//! This module contains **no fault-injection code**. It starts an external
//! executable, the separately built `orbit-reliability` tool, which is the
//! only program that can inject faults. It is always compiled, so that
//! `orbit-api` (built without the `reliability` feature) can offer launching
//! when an operator configures a runner, without containing any fault hooks.
//!
//! The boundary:
//!
//! - The runner executable is configuration (an absolute path to an
//!   existing file), never client input. Without it, nothing can be launched.
//! - A client supplies only a [`ReliabilitySpec`]: a scenario id from the
//!   catalog and bounded integer parameters. It is validated, and recorded as
//!   a `created` experiment by this process (which holds its creation lock)
//!   **before** anything is started.
//! - The runner's arguments are fixed: `--lab <the lab directory> run-id
//!   <experiment id>`. No client-supplied text reaches the command line, no
//!   shell is involved, and the runner reads the specification from the
//!   registry.
//! - One launch at a time per [`Launcher`].
//! - A deadline: on expiry the runner, and on Unix its whole process group
//!   (its scenario child processes), is killed.
//! - Afterwards the lifecycle is finished explicitly. An experiment the
//!   runner never started is failed with kind `infrastructure`; one it left
//!   `running` is failed with kind `infrastructure` (timeout) or
//!   `interrupted` (the runner died). A completed or failed record is never
//!   touched. The scenario's work directory is removed.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::error::{LabError, Result};
use crate::registry::{ExperimentId, Failure, FailureKind, Status};
use crate::reliability::ReliabilitySpec;
use crate::{ExperimentRecord, Lab};

/// Default deadline of one launched experiment.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(900);
/// Largest accepted deadline.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(3_600);
/// Most bytes of runner output kept (for logs only).
pub const MAX_OUTPUT: usize = 64 * 1024;

/// Why a launch was refused.
#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    /// Another launched experiment is still running.
    #[error(
        "a reliability experiment is already running on this server; wait for it to finish (one runs at a time)"
    )]
    Busy,
    /// The spec or the registry refused it.
    #[error(transparent)]
    Lab(#[from] LabError),
}

/// How a launched experiment ended, from the launcher's point of view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchReport {
    /// The experiment.
    pub id: ExperimentId,
    /// Exit status of the runner, if it exited.
    pub exit: Option<String>,
    /// Whether the deadline expired and the runner was killed.
    pub timed_out: bool,
    /// The final status in the registry.
    pub status: Status,
    /// What the launcher itself recorded, if anything.
    pub finalized: Option<&'static str>,
}

/// Starts reliability experiments in the configured runner, one at a time.
#[derive(Debug)]
pub struct Launcher {
    runner: PathBuf,
    timeout: Duration,
    busy: Arc<AtomicBool>,
}

struct Busy(Arc<AtomicBool>);
impl Drop for Busy {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl Launcher {
    /// A launcher for the runner executable at `runner`.
    ///
    /// # Errors
    /// [`LabError::InvalidSpec`] if the path is not absolute or not a
    /// regular file, or the timeout is zero or above [`MAX_TIMEOUT`].
    pub fn new(runner: &Path, timeout: Duration) -> Result<Self> {
        let bad = |reason: &str| LabError::InvalidSpec {
            field: "reliability_runner",
            reason: reason.to_owned(),
        };
        if !runner.is_absolute() {
            return Err(bad("the runner path must be absolute"));
        }
        if !std::fs::metadata(runner).is_ok_and(|m| m.is_file()) {
            return Err(bad("the runner path is not an existing file"));
        }
        if timeout.is_zero() || timeout > MAX_TIMEOUT {
            return Err(bad("the timeout must be between 1 s and 3600 s"));
        }
        Ok(Self {
            runner: runner.to_path_buf(),
            timeout,
            busy: Arc::new(AtomicBool::new(false)),
        })
    }

    /// The deadline of one launch.
    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Whether a launched experiment is running.
    #[must_use]
    pub fn busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    /// The runner's complete argument list for experiment `id` in `lab_dir`.
    /// Nothing else is ever passed.
    #[must_use]
    pub fn arguments(lab_dir: &Path, id: ExperimentId) -> Vec<OsString> {
        vec![
            "--lab".into(),
            lab_dir.as_os_str().to_owned(),
            "run-id".into(),
            id.to_string().into(),
        ]
    }

    /// Validates and records `spec`, starts the runner on it, and returns
    /// the `created` record at once. `on_done` is called on the supervising
    /// thread with the outcome.
    ///
    /// # Errors
    /// [`LaunchError::Busy`] (nothing is recorded), or an invalid spec
    /// (nothing is recorded).
    pub fn launch(
        &self,
        lab: Arc<Lab>,
        spec: &ReliabilitySpec,
        on_done: impl FnOnce(LaunchReport) + Send + 'static,
    ) -> std::result::Result<ExperimentRecord, LaunchError> {
        // Validate before taking the slot, so a bad request cannot block it.
        spec.validated()?;
        if self.busy.swap(true, Ordering::SeqCst) {
            return Err(LaunchError::Busy);
        }
        let guard = Busy(Arc::clone(&self.busy));
        let record = lab.create_reliability(spec)?;
        let id = record.id;
        let mut command = Command::new(&self.runner);
        command
            .args(Self::arguments(lab.dir(), id))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Its own process group, so a timeout can stop the scenario's
            // child processes too.
            command.process_group(0);
        }
        let timeout = self.timeout;
        match command.spawn() {
            Err(e) => {
                let failure = Failure::new(
                    FailureKind::Infrastructure,
                    format!("the reliability runner could not be started: {e}"),
                );
                lab.registry().fail_created(id, &failure)?;
                lab.release(id);
                drop(guard);
                Err(LaunchError::Lab(LabError::InvalidState {
                    id,
                    reason: "the reliability runner could not be started".to_owned(),
                }))
            }
            Ok(child) => {
                std::thread::Builder::new()
                    .name("orbit-reliability-runner".to_owned())
                    .spawn(move || {
                        let _guard = guard;
                        let report = supervise(&lab, id, child, timeout);
                        on_done(report);
                    })
                    .map_err(|e| LaunchError::Lab(e.into()))?;
                Ok(record)
            }
        }
    }
}

fn drain(mut pipe: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        let mut buf = [0u8; 8192];
        while let Ok(n) = pipe.read(&mut buf) {
            if n == 0 {
                break;
            }
            let room = MAX_OUTPUT.saturating_sub(kept.len());
            kept.extend_from_slice(&buf[..n.min(room)]);
        }
        kept
    })
}

fn kill_tree(child: &mut Child) {
    #[cfg(unix)]
    {
        // The runner leads its own process group (see `launch`): signal the
        // group, then the leader.
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", child.id())])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
}

fn supervise(lab: &Lab, id: ExperimentId, mut child: Child, timeout: Duration) -> LaunchReport {
    let out = child.stdout.take().map(drain);
    let err = child.stderr.take().map(drain);
    let child = Arc::new(Mutex::new(child));
    let started = Instant::now();
    let mut timed_out = false;
    let status: Option<ExitStatus> = loop {
        let polled = child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .try_wait();
        match polled {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() >= timeout => {
                timed_out = true;
                let mut c = child
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                kill_tree(&mut c);
                break c.wait().ok();
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => break None,
        }
    };
    let stdout = out.and_then(|h| h.join().ok()).unwrap_or_default();
    let stderr = err.and_then(|h| h.join().ok()).unwrap_or_default();
    tracing::info!(
        %id,
        status = ?status,
        timed_out,
        stdout_bytes = stdout.len(),
        stderr = %String::from_utf8_lossy(&stderr).chars().take(2_000).collect::<String>(),
        "reliability runner finished"
    );
    let finalized = finalize(lab, id, status, timed_out, timeout);
    // A killed runner cannot remove its run lock file (its lock is already
    // released by the OS).
    let lock = lab.dir().join("locks").join(format!("{id}.lock"));
    if lock.is_file() {
        let _ = std::fs::remove_file(&lock);
    }
    // The runner removes its work directory itself unless it was killed.
    let work = lab.dir().join("reliability-work").join(id.to_string());
    if work.is_dir() {
        let _ = std::fs::remove_dir_all(&work);
    }
    let _ = lab.artifacts().remove_stale_temporaries();
    lab.release(id);
    LaunchReport {
        id,
        exit: status.map(|s| s.to_string()),
        timed_out,
        status: lab.experiment(id).map_or(Status::Failed, |r| r.status),
        finalized,
    }
}

/// Records what the runner left undone. Never changes a finished record.
fn finalize(
    lab: &Lab,
    id: ExperimentId,
    status: Option<ExitStatus>,
    timed_out: bool,
    timeout: Duration,
) -> Option<&'static str> {
    let record = lab.experiment(id).ok()?;
    let how = match (timed_out, status) {
        (true, _) => format!(
            "the reliability runner did not finish within {} s and was stopped",
            timeout.as_secs()
        ),
        (false, Some(s)) => format!("the reliability runner exited ({s})"),
        (false, None) => "the reliability runner's exit could not be observed".to_owned(),
    };
    match record.status {
        Status::Created => {
            let failure = Failure::new(
                FailureKind::Infrastructure,
                format!("{how} before starting the experiment"),
            );
            lab.registry()
                .fail_created(id, &failure)
                .ok()
                .filter(|changed| *changed)
                .map(|_| "failed_before_start")
        }
        Status::Running if timed_out => {
            let failure = Failure::new(FailureKind::Infrastructure, how);
            lab.registry()
                .fail(id, &failure, None, None)
                .ok()
                .map(|()| "failed_timeout")
        }
        Status::Running => lab
            .recover_interrupted()
            .ok()
            .filter(|ids| ids.contains(&id))
            .map(|_| "interrupted"),
        Status::Completed | Status::Failed => None,
    }
}
