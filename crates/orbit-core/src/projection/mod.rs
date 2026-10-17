//! Projections: deterministic folds of events into application state.
//!
//! A [`Projection`] defines an initial state and how one event transforms a
//! state. Reconstructing state is then a left fold over a stream's events in
//! sequence order, starting either from [`Projection::initial_state`] or from
//! a validated snapshot. See [`crate::reconstruct`].
//!
//! # Contract
//!
//! * **Deterministic.** `apply` must depend only on the state and event it
//!   is given. No clocks, randomness, I/O or global state. In particular
//!   use [`StoredEvent::sequence`], not `recorded_at`, for anything order-related.
//! * **Total or explicit.** Every event either produces a new state or an
//!   error. Unknown event types should normally be reported as
//!   [`ProjectionErrorKind::UnsupportedEvent`]. A projection that
//!   deliberately ignores some event types must say so in its documentation
//!   and return the state unchanged.
//! * **Versioned.** [`Projection::key`] names the projection and its version.
//!   Bump the version whenever the same events could yield a different state,
//!   or the state's serialized shape changes. Snapshots are only reused by
//!   projections with an identical key.
//! * **Serializable state.** States are compared, diffed and snapshotted as
//!   JSON through `serde`. The JSON form must round-trip: deserializing a
//!   serialized state must give back an equivalent state.

pub mod examples;

use std::fmt;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::domain::StoredEvent;
use crate::snapshot::ProjectionKey;

/// A deterministic transformation of state in response to events.
pub trait Projection {
    /// The state being reconstructed.
    type State: Clone + Serialize + DeserializeOwned;

    /// Name and version of this projection.
    fn key(&self) -> ProjectionKey;

    /// State before any event has been applied (the state at sequence 0).
    fn initial_state(&self) -> Self::State;

    /// Applies one event, returning the next state or an error that explains
    /// why the event cannot be applied to this state.
    ///
    /// # Errors
    /// A [`ProjectionError`]. Reconstruction stops at the first error. A
    /// partial state is never returned.
    fn apply(
        &self,
        state: Self::State,
        event: &StoredEvent,
    ) -> Result<Self::State, ProjectionError>;
}

/// Category of a [`ProjectionError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionErrorKind {
    /// The projection does not handle this event type (or schema version).
    UnsupportedEvent,
    /// The payload is malformed for this event type.
    InvalidPayload,
    /// The event is well-formed but not valid in the current state
    /// (e.g. removing more stock than is available).
    RejectedTransition,
}

/// Why a projection could not apply an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, thiserror::Error)]
#[error("{kind}: {message}")]
pub struct ProjectionError {
    /// Category.
    pub kind: ProjectionErrorKind,
    /// Human-readable explanation.
    pub message: String,
}

impl fmt::Display for ProjectionErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::UnsupportedEvent => "unsupported event",
            Self::InvalidPayload => "invalid payload",
            Self::RejectedTransition => "rejected transition",
        })
    }
}

impl ProjectionError {
    /// Builds an error of the given kind.
    pub fn new(kind: ProjectionErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// The event's type (or schema version) is not handled by this projection.
    #[must_use]
    pub fn unsupported(event: &StoredEvent) -> Self {
        Self::new(
            ProjectionErrorKind::UnsupportedEvent,
            format!(
                "event type {} (schema v{}) is not handled",
                event.event_type, event.schema_version
            ),
        )
    }

    /// The event is not valid in the current state.
    pub fn rejected(message: impl Into<String>) -> Self {
        Self::new(ProjectionErrorKind::RejectedTransition, message)
    }
}

/// Deserializes an event payload into `T`, mapping failures to
/// [`ProjectionErrorKind::InvalidPayload`].
///
/// # Errors
/// If the payload does not match `T`.
pub fn decode_payload<T: DeserializeOwned>(event: &StoredEvent) -> Result<T, ProjectionError> {
    T::deserialize(&event.payload).map_err(|e| {
        ProjectionError::new(
            ProjectionErrorKind::InvalidPayload,
            format!("{} payload: {e}", event.event_type),
        )
    })
}
