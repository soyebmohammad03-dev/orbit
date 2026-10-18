//! Registered implementations and how the lab executes them.
//!
//! An [`Implementation`] is an identified, executable piece of projection
//! code. [`ProjectionImplementation`] adapts any `orbit_core::Projection`, so
//! existing projections run unchanged: the lab calls the same
//! `initial_state` and `apply` that reconstruction calls. Each execution
//! gets a fresh [`Stepper`] that owns its own state, so two implementations
//! (or two runs of one implementation) never share state.

use std::any::type_name;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use orbit_core::projection::MergePatch;
use orbit_core::projection::examples::{Counter, Inventory};
use orbit_core::{Projection, ProjectionError, StoredEvent};
use serde::Serialize;
use serde_json::Value;

use crate::error::{LabError, Result};
use crate::identity::{
    Determinism, ImplementationDescriptor, ImplementationId, RUST_PROJECTION_ADAPTER,
};
use crate::variants::{CounterFloorAtZero, InventoryUntrackedShipments};

/// Why one step of an implementation failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StepFailure {
    /// The projection returned an error (it rejected the event).
    Projection {
        /// The error, as returned.
        error: ProjectionError,
    },
    /// The implementation panicked. The panic was caught.
    Panicked {
        /// The panic message, if it was a string.
        message: String,
    },
    /// The state could not be serialized to JSON.
    StateEncoding {
        /// The serializer's message.
        message: String,
    },
    /// A previous step failed, so no state is available.
    Unavailable,
}

/// One execution of an implementation: a state plus the operation that
/// advances it by one event.
pub trait Stepper {
    /// The current state as JSON.
    ///
    /// # Errors
    /// [`StepFailure`] if the state cannot be encoded or is unavailable.
    fn state(&self) -> Result<Value, StepFailure>;

    /// Applies one event.
    ///
    /// # Errors
    /// [`StepFailure`]. After an error the stepper has no state.
    fn apply(&mut self, event: &StoredEvent) -> Result<(), StepFailure>;
}

/// An identified implementation that can be executed.
pub trait Implementation: Send + Sync {
    /// Its identity.
    fn descriptor(&self) -> &ImplementationDescriptor;

    /// Starts a fresh execution at the initial state.
    ///
    /// # Errors
    /// [`StepFailure`] if the initial state cannot be produced.
    fn start(&self) -> Result<Box<dyn Stepper>, StepFailure>;
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic with a non-string payload".to_owned())
}

/// Adapter for an `orbit_core::Projection`.
#[derive(Debug, Clone)]
pub struct ProjectionImplementation<P> {
    projection: P,
    descriptor: ImplementationDescriptor,
}

impl<P> ProjectionImplementation<P>
where
    P: Projection + Clone + Send + Sync + 'static,
{
    /// Wraps `projection` under `id`, with no configuration.
    ///
    /// # Errors
    /// [`LabError::InvalidDescriptor`] for an invalid id or empty name.
    pub fn new(id: &str, name: &str, projection: P, determinism: Determinism) -> Result<Self> {
        Self::with_config(
            id,
            name,
            projection,
            Value::Object(Default::default()),
            determinism,
        )
    }

    /// Wraps `projection` with a recorded configuration value. The
    /// configuration is identity metadata: the lab does not interpret it.
    ///
    /// # Errors
    /// [`LabError::InvalidDescriptor`] for an invalid id or empty name.
    pub fn with_config(
        id: &str,
        name: &str,
        projection: P,
        config: Value,
        determinism: Determinism,
    ) -> Result<Self> {
        let descriptor = ImplementationDescriptor::new(
            ImplementationId::new(id)?,
            name,
            projection.key(),
            RUST_PROJECTION_ADAPTER,
            type_name::<P>(),
            config,
            determinism,
        )?;
        Ok(Self {
            projection,
            descriptor,
        })
    }
}

struct ProjectionStepper<P: Projection> {
    projection: P,
    state: Option<P::State>,
}

impl<P: Projection> Stepper for ProjectionStepper<P> {
    fn state(&self) -> Result<Value, StepFailure> {
        let state = self.state.as_ref().ok_or(StepFailure::Unavailable)?;
        match catch_unwind(AssertUnwindSafe(|| serde_json::to_value(state))) {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(e)) => Err(StepFailure::StateEncoding {
                message: e.to_string(),
            }),
            Err(payload) => Err(StepFailure::Panicked {
                message: panic_message(payload.as_ref()),
            }),
        }
    }

    fn apply(&mut self, event: &StoredEvent) -> Result<(), StepFailure> {
        let state = self.state.take().ok_or(StepFailure::Unavailable)?;
        let projection = &self.projection;
        match catch_unwind(AssertUnwindSafe(|| projection.apply(state, event))) {
            Ok(Ok(next)) => {
                self.state = Some(next);
                Ok(())
            }
            Ok(Err(error)) => Err(StepFailure::Projection { error }),
            Err(payload) => Err(StepFailure::Panicked {
                message: panic_message(payload.as_ref()),
            }),
        }
    }
}

impl<P> Implementation for ProjectionImplementation<P>
where
    P: Projection + Clone + Send + Sync + 'static,
{
    fn descriptor(&self) -> &ImplementationDescriptor {
        &self.descriptor
    }

    fn start(&self) -> Result<Box<dyn Stepper>, StepFailure> {
        let projection = self.projection.clone();
        match catch_unwind(AssertUnwindSafe(|| projection.initial_state())) {
            Ok(state) => Ok(Box::new(ProjectionStepper {
                projection,
                state: Some(state),
            })),
            Err(payload) => Err(StepFailure::Panicked {
                message: panic_message(payload.as_ref()),
            }),
        }
    }
}

/// The set of implementations available to experiments, keyed by id.
#[derive(Clone, Default)]
pub struct Catalog {
    entries: Vec<Arc<dyn Implementation>>,
}

impl std::fmt::Debug for Catalog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.entries.iter().map(|e| e.descriptor().id.as_str()))
            .finish()
    }
}

impl Catalog {
    /// An empty catalog.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The built-in implementations:
    ///
    /// | Id | Projection | Code |
    /// |----|------------|------|
    /// | `orbit.counter` | `counter@1` | `orbit_core::projection::examples::Counter` |
    /// | `orbit.inventory` | `inventory@1` | `orbit_core::projection::examples::Inventory` |
    /// | `orbit.merge-patch` | `merge-patch@1` | `orbit_core::projection::MergePatch` |
    /// | `example.counter-floor-at-zero` | `counter@1` | [`CounterFloorAtZero`] |
    /// | `example.inventory-untracked-shipments` | `inventory@1` | [`InventoryUntrackedShipments`] |
    ///
    /// The two `example.*` entries are deliberately different implementations
    /// that declare the same projection key as a core projection. They exist
    /// to demonstrate and test differential replay.
    ///
    /// # Panics
    /// Never: the built-in descriptors are valid (covered by tests).
    #[must_use]
    pub fn builtin() -> Self {
        let mut catalog = Self::new();
        let entries: [Arc<dyn Implementation>; 5] = [
            Arc::new(
                ProjectionImplementation::new(
                    "orbit.counter",
                    "Counter",
                    Counter,
                    Determinism::Pure,
                )
                .expect("valid built-in descriptor"),
            ),
            Arc::new(
                ProjectionImplementation::new(
                    "orbit.inventory",
                    "Inventory",
                    Inventory,
                    Determinism::Pure,
                )
                .expect("valid built-in descriptor"),
            ),
            Arc::new(
                ProjectionImplementation::new(
                    "orbit.merge-patch",
                    "JSON merge patch",
                    MergePatch,
                    Determinism::Pure,
                )
                .expect("valid built-in descriptor"),
            ),
            Arc::new(
                ProjectionImplementation::new(
                    "example.counter-floor-at-zero",
                    "Counter",
                    CounterFloorAtZero,
                    Determinism::Pure,
                )
                .expect("valid built-in descriptor"),
            ),
            Arc::new(
                ProjectionImplementation::new(
                    "example.inventory-untracked-shipments",
                    "Inventory",
                    InventoryUntrackedShipments,
                    Determinism::Pure,
                )
                .expect("valid built-in descriptor"),
            ),
        ];
        for entry in entries {
            catalog.register(entry).expect("unique built-in ids");
        }
        catalog
    }

    /// Adds an implementation.
    ///
    /// # Errors
    /// [`LabError::DuplicateImplementation`] if its id is already registered.
    pub fn register(&mut self, implementation: Arc<dyn Implementation>) -> Result<()> {
        let id = &implementation.descriptor().id;
        if self.find(id.as_str()).is_some() {
            return Err(LabError::DuplicateImplementation(id.to_string()));
        }
        self.entries.push(implementation);
        Ok(())
    }

    /// The implementation with this id.
    #[must_use]
    pub fn find(&self, id: &str) -> Option<Arc<dyn Implementation>> {
        self.entries
            .iter()
            .find(|e| e.descriptor().id.as_str() == id)
            .cloned()
    }

    /// Every implementation, in registration order.
    #[must_use]
    pub fn all(&self) -> &[Arc<dyn Implementation>] {
        &self.entries
    }
}
