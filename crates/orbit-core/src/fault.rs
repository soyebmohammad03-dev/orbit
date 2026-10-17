//! Deterministic fault injection at transaction boundaries.
//!
//! The write paths of [`EventStore`](crate::EventStore) pass through named
//! [`FaultPoint`]s. With the `fault-injection` cargo feature enabled, a
//! per-store hook installed with `EventStore::set_fault_hook` is
//! consulted at each point. If the hook returns `true`, the operation fails
//! there with [`StoreError::InjectedFault`](crate::StoreError::InjectedFault),
//! exactly as a real error at that spot would: an open transaction is dropped
//! and rolled back. A hook may also terminate the process (for example with
//! `std::process::abort`) to simulate a crash at that point.
//!
//! Without the feature, which is the default, no hook exists. The points
//! compile to nothing, and there is no global state either way.
//!
//! These faults simulate *errors and process crashes*. They do not simulate
//! power loss or storage-device failure.

use std::fmt;

use serde::Serialize;

/// A named point in a write path where a fault can be injected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FaultPoint {
    /// `create_stream`: after the insert, immediately before `COMMIT`.
    CreateStreamBeforeCommit,
    /// `append`: after validation, before `BEGIN IMMEDIATE`.
    AppendBeforeBegin,
    /// `append`: write lock held, nothing read yet.
    AppendAfterBegin,
    /// `append`: head read, preconditions checked, sequence and digest computed; nothing written.
    AppendAfterSequenceAllocated,
    /// `append`: event row inserted, stream head not yet advanced.
    AppendAfterInsert,
    /// `append`: event inserted and head advanced, immediately before `COMMIT`.
    AppendBeforeCommit,
    /// `save_snapshot`: before `BEGIN IMMEDIATE`.
    SnapshotBeforeBegin,
    /// `save_snapshot`: history binding validated, row not yet inserted.
    SnapshotAfterValidation,
    /// `save_snapshot`: row inserted, immediately before `COMMIT`.
    SnapshotBeforeCommit,
    /// Replay persistence: run or execution row inserted, immediately before `COMMIT`.
    ReplayBeforeCommit,
}

impl FaultPoint {
    /// Every fault point, in write-path order.
    pub const ALL: [FaultPoint; 10] = [
        Self::CreateStreamBeforeCommit,
        Self::AppendBeforeBegin,
        Self::AppendAfterBegin,
        Self::AppendAfterSequenceAllocated,
        Self::AppendAfterInsert,
        Self::AppendBeforeCommit,
        Self::SnapshotBeforeBegin,
        Self::SnapshotAfterValidation,
        Self::SnapshotBeforeCommit,
        Self::ReplayBeforeCommit,
    ];
}

impl fmt::Display for FaultPoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

/// A fault hook: returns `true` to inject a failure at the given point.
#[cfg(feature = "fault-injection")]
pub type FaultHook = std::sync::Arc<dyn Fn(FaultPoint) -> bool + Send + Sync>;
