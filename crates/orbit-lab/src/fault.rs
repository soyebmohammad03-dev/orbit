//! Lab fault points: where a reliability scenario may interrupt an
//! experiment run or an artifact write.
//!
//! The points always exist so that they can be named and documented. A hook
//! that acts on them can only be installed with the `reliability` cargo
//! feature, which `orbit-api` never enables (a CI step checks its feature
//! graph). Without the feature the points compile to nothing. Nothing reads
//! environment variables: a hook is installed only by code that calls
//! [`Lab::set_fault_hook`](crate::Lab).
//!
//! A hook decides what happens at a point. Returning `true` makes the
//! operation fail there with [`LabError::InjectedFault`](crate::LabError),
//! like an error at that spot would (a *simulated* failure). A hook may also
//! abort the process or block it, which is how the reliability scenarios
//! produce *real* process interruptions. Neither simulates power loss.

use std::fmt;

use serde::Serialize;

/// A named point in an experiment run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LabFaultPoint {
    /// `Lab::run`: status is `running` and the lock is held; nothing executed yet.
    RunAfterStart,
    /// `Lab::run`: the result is computed; no artifact written yet.
    RunAfterExecute,
    /// `ArtifactStore::put`: half of the bytes written to the temporary
    /// file; not flushed, not renamed into place.
    ArtifactAfterPartialWrite,
    /// `Lab::run`: every artifact is written; the registry transaction that
    /// links them and stores the result has not started.
    RunBeforeComplete,
}

impl LabFaultPoint {
    /// Every point, in execution order.
    pub const ALL: [Self; 4] = [
        Self::RunAfterStart,
        Self::RunAfterExecute,
        Self::ArtifactAfterPartialWrite,
        Self::RunBeforeComplete,
    ];

    /// Stable snake-case name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RunAfterStart => "run_after_start",
            Self::RunAfterExecute => "run_after_execute",
            Self::ArtifactAfterPartialWrite => "artifact_after_partial_write",
            Self::RunBeforeComplete => "run_before_complete",
        }
    }

    /// Parses [`as_str`](Self::as_str).
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == text)
    }
}

impl fmt::Display for LabFaultPoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A lab fault hook: returns `true` to inject a failure at the given point.
#[cfg(feature = "reliability")]
pub type LabFaultHook = std::sync::Arc<dyn Fn(LabFaultPoint) -> bool + Send + Sync>;

/// Shared slot for the hook (the lab and its artifact store consult it).
#[cfg(feature = "reliability")]
pub(crate) type HookSlot = std::sync::Arc<std::sync::Mutex<Option<LabFaultHook>>>;

/// Consults `slot` at `point`.
#[cfg(feature = "reliability")]
pub(crate) fn check(slot: &HookSlot, point: LabFaultPoint) -> crate::Result<()> {
    let hook = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if hook.is_some_and(|hook| hook(point)) {
        return Err(crate::LabError::InjectedFault(point));
    }
    Ok(())
}
