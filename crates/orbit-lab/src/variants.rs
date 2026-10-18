//! Example implementations that deliberately differ from the core examples
//! while declaring the *same* projection key.
//!
//! They model a common real situation: code changed but nobody bumped the
//! projection version. `orbit-core` alone cannot tell them apart from the
//! originals, because they share a `ProjectionKey`. The lab can, because
//! each is registered under its own implementation id, and differential
//! replay shows exactly where their behaviour differs.

use orbit_core::projection::examples::{Counter, CounterState, Inventory, InventoryState};
use orbit_core::{Projection, ProjectionError, ProjectionKey, StoredEvent};

/// `counter@1`, except that `counter.decremented` never takes the value
/// below zero (it stops at 0). Identical to [`Counter`] for every history in
/// which the value never goes negative.
#[derive(Debug, Clone, Copy, Default)]
pub struct CounterFloorAtZero;

impl Projection for CounterFloorAtZero {
    type State = CounterState;

    fn key(&self) -> ProjectionKey {
        Counter.key()
    }

    fn initial_state(&self) -> CounterState {
        Counter.initial_state()
    }

    fn apply(
        &self,
        state: CounterState,
        event: &StoredEvent,
    ) -> Result<CounterState, ProjectionError> {
        let next = Counter.apply(state, event)?;
        if event.event_type.as_str() == "counter.decremented" && next.value < 0 {
            return Ok(CounterState { value: 0 });
        }
        Ok(next)
    }
}

/// `inventory@1`, except that `inventory.shipped` does not add to
/// `shipped_total`. Stock levels are identical to [`Inventory`].
#[derive(Debug, Clone, Copy, Default)]
pub struct InventoryUntrackedShipments;

impl Projection for InventoryUntrackedShipments {
    type State = InventoryState;

    fn key(&self) -> ProjectionKey {
        Inventory.key()
    }

    fn initial_state(&self) -> InventoryState {
        Inventory.initial_state()
    }

    fn apply(
        &self,
        state: InventoryState,
        event: &StoredEvent,
    ) -> Result<InventoryState, ProjectionError> {
        let shipped_total = state.shipped_total;
        let mut next = Inventory.apply(state, event)?;
        next.shipped_total = shipped_total;
        Ok(next)
    }
}
