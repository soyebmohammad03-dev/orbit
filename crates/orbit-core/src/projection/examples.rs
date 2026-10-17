//! Two demonstration projections, used by the tests, benchmarks and docs.
//!
//! They are intentionally small. In particular [`Inventory`] is a teaching
//! example and not a stock-keeping system.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{Projection, ProjectionError, decode_payload};
use crate::domain::StoredEvent;
use crate::snapshot::ProjectionKey;

/// A signed 64-bit counter.
///
/// | Event type            | Payload        | Effect                  |
/// |-----------------------|----------------|-------------------------|
/// | `counter.incremented` | `{"by": u64}`  | `value += by`           |
/// | `counter.decremented` | `{"by": u64}`  | `value -= by`           |
/// | `counter.reset`       | `{}`           | `value = 0`             |
///
/// Overflow is a rejected transition; any other event type is unsupported.
#[derive(Debug, Clone, Copy, Default)]
pub struct Counter;

/// State of the [`Counter`] projection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CounterState {
    /// Current value.
    pub value: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct By {
    by: u64,
}

impl Projection for Counter {
    type State = CounterState;

    fn key(&self) -> ProjectionKey {
        // Valid by construction; covered by tests.
        ProjectionKey::new("counter", 1).unwrap_or_else(|_| unreachable!())
    }

    fn initial_state(&self) -> CounterState {
        CounterState::default()
    }

    fn apply(
        &self,
        state: CounterState,
        event: &StoredEvent,
    ) -> Result<CounterState, ProjectionError> {
        let step = |event: &StoredEvent| -> Result<i64, ProjectionError> {
            let By { by } = decode_payload(event)?;
            i64::try_from(by)
                .map_err(|_| ProjectionError::rejected(format!("step {by} is too large")))
        };
        let value = match event.event_type.as_str() {
            "counter.incremented" => state.value.checked_add(step(event)?),
            "counter.decremented" => state.value.checked_sub(step(event)?),
            "counter.reset" => Some(0),
            _ => return Err(ProjectionError::unsupported(event)),
        }
        .ok_or_else(|| ProjectionError::rejected(format!("counter overflow at {}", state.value)))?;
        Ok(CounterState { value })
    }
}

/// A single-warehouse inventory with several independently changing fields.
///
/// | Event type                  | Payload                                    | Effect |
/// |-----------------------------|--------------------------------------------|--------|
/// | `inventory.warehouse_named` | `{"name": str}`                            | sets `warehouse` |
/// | `inventory.item_added`      | `{"sku": str, "name": str, "quantity": u64}` | new item; SKU must be new and not discontinued |
/// | `inventory.restocked`       | `{"sku": str, "quantity": u64}`            | `on_hand += quantity` |
/// | `inventory.reserved`        | `{"sku": str, "quantity": u64}`            | moves stock from `on_hand` to `reserved` |
/// | `inventory.shipped`         | `{"sku": str, "quantity": u64}`            | removes stock from `reserved` |
/// | `inventory.discontinued`    | `{"sku": str}`                             | removes the item (must have no reserved stock) and appends the SKU to `discontinued` |
///
/// Restock, reserve and ship quantities must be positive. Operations on unknown SKUs, reserving more
/// than is on hand and shipping more than is reserved are rejected transitions.
#[derive(Debug, Clone, Copy, Default)]
pub struct Inventory;

/// State of the [`Inventory`] projection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryState {
    /// Warehouse display name.
    pub warehouse: Option<String>,
    /// Active items by SKU.
    pub items: BTreeMap<String, Item>,
    /// Discontinued SKUs, in the order they were discontinued.
    pub discontinued: Vec<String>,
    /// Total units shipped across all items.
    pub shipped_total: u64,
}

/// One inventory item.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// Display name.
    pub name: String,
    /// Units available.
    pub on_hand: u64,
    /// Units reserved for orders.
    pub reserved: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Named {
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Added {
    sku: String,
    name: String,
    quantity: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Moved {
    sku: String,
    quantity: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SkuOnly {
    sku: String,
}

fn positive(quantity: u64) -> Result<u64, ProjectionError> {
    if quantity == 0 {
        Err(ProjectionError::new(
            super::ProjectionErrorKind::InvalidPayload,
            "quantity must be positive",
        ))
    } else {
        Ok(quantity)
    }
}

impl Projection for Inventory {
    type State = InventoryState;

    fn key(&self) -> ProjectionKey {
        ProjectionKey::new("inventory", 1).unwrap_or_else(|_| unreachable!())
    }

    fn initial_state(&self) -> InventoryState {
        InventoryState::default()
    }

    fn apply(
        &self,
        mut state: InventoryState,
        event: &StoredEvent,
    ) -> Result<InventoryState, ProjectionError> {
        fn item<'a>(
            state: &'a mut InventoryState,
            sku: &str,
        ) -> Result<&'a mut Item, ProjectionError> {
            state
                .items
                .get_mut(sku)
                .ok_or_else(|| ProjectionError::rejected(format!("unknown SKU {sku}")))
        }
        match event.event_type.as_str() {
            "inventory.warehouse_named" => {
                let Named { name } = decode_payload(event)?;
                state.warehouse = Some(name);
            }
            "inventory.item_added" => {
                let Added {
                    sku,
                    name,
                    quantity,
                } = decode_payload(event)?;
                if state.items.contains_key(&sku) || state.discontinued.contains(&sku) {
                    return Err(ProjectionError::rejected(format!(
                        "SKU {sku} already exists"
                    )));
                }
                state.items.insert(
                    sku,
                    Item {
                        name,
                        on_hand: quantity,
                        reserved: 0,
                    },
                );
            }
            "inventory.restocked" => {
                let Moved { sku, quantity } = decode_payload(event)?;
                let quantity = positive(quantity)?;
                let item = item(&mut state, &sku)?;
                item.on_hand = item
                    .on_hand
                    .checked_add(quantity)
                    .ok_or_else(|| ProjectionError::rejected("stock overflow"))?;
            }
            "inventory.reserved" => {
                let Moved { sku, quantity } = decode_payload(event)?;
                let quantity = positive(quantity)?;
                let item = item(&mut state, &sku)?;
                if quantity > item.on_hand {
                    return Err(ProjectionError::rejected(format!(
                        "cannot reserve {quantity} of {sku}: only {} on hand",
                        item.on_hand
                    )));
                }
                item.on_hand -= quantity;
                item.reserved += quantity;
            }
            "inventory.shipped" => {
                let Moved { sku, quantity } = decode_payload(event)?;
                let quantity = positive(quantity)?;
                let item = item(&mut state, &sku)?;
                if quantity > item.reserved {
                    return Err(ProjectionError::rejected(format!(
                        "cannot ship {quantity} of {sku}: only {} reserved",
                        item.reserved
                    )));
                }
                item.reserved -= quantity;
                state.shipped_total = state.shipped_total.saturating_add(quantity);
            }
            "inventory.discontinued" => {
                let SkuOnly { sku } = decode_payload(event)?;
                if item(&mut state, &sku)?.reserved > 0 {
                    return Err(ProjectionError::rejected(format!(
                        "cannot discontinue {sku} while stock is reserved"
                    )));
                }
                state.items.remove(&sku);
                state.discontinued.push(sku);
            }
            _ => return Err(ProjectionError::unsupported(event)),
        }
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        Digest, EventId, EventType, Metadata, SchemaVersion, Sequence, StreamId, Timestamp,
    };
    use crate::projection::ProjectionErrorKind;
    use serde_json::{Value, json};

    fn event(n: u64, ty: &str, payload: Value) -> StoredEvent {
        StoredEvent {
            event_id: EventId::generate(),
            stream_id: StreamId::new("s").unwrap(),
            sequence: Sequence::new(n).unwrap(),
            event_type: EventType::new(ty).unwrap(),
            schema_version: SchemaVersion::INITIAL,
            recorded_at: Timestamp::from_unix_micros(0).unwrap(),
            payload,
            metadata: Metadata::new(),
            prev_digest: Digest::GENESIS,
            digest: Digest::GENESIS,
        }
    }

    fn fold<P: Projection>(p: &P, events: &[StoredEvent]) -> Result<P::State, ProjectionError> {
        events
            .iter()
            .try_fold(p.initial_state(), |s, e| p.apply(s, e))
    }

    #[test]
    fn keys_are_valid() {
        assert_eq!(Counter.key().to_string(), "counter@v1");
        assert_eq!(Inventory.key().to_string(), "inventory@v1");
    }

    #[test]
    fn counter_applies_valid_events() {
        let events = [
            event(1, "counter.incremented", json!({"by": 10})),
            event(2, "counter.decremented", json!({"by": 3})),
            event(3, "counter.decremented", json!({"by": 9})),
        ];
        assert_eq!(Counter.initial_state().value, 0);
        assert_eq!(fold(&Counter, &events).unwrap().value, -2);
        let reset = event(4, "counter.reset", json!({}));
        assert_eq!(
            Counter
                .apply(CounterState { value: 7 }, &reset)
                .unwrap()
                .value,
            0
        );
    }

    #[test]
    fn counter_rejects_invalid_events() {
        let state = CounterState { value: i64::MAX };
        let err = Counter
            .apply(state, &event(1, "counter.incremented", json!({"by": 1})))
            .unwrap_err();
        assert_eq!(err.kind, ProjectionErrorKind::RejectedTransition);
        for (ty, payload) in [
            ("counter.incremented", json!({"by": -1})),
            ("counter.incremented", json!({"step": 1})),
            ("counter.incremented", json!({"by": 1, "extra": true})),
            ("counter.decremented", json!("1")),
        ] {
            let err = Counter
                .apply(CounterState::default(), &event(1, ty, payload))
                .unwrap_err();
            assert_eq!(err.kind, ProjectionErrorKind::InvalidPayload, "{ty}");
        }
        let err = Counter
            .apply(
                CounterState::default(),
                &event(1, "counter.multiplied", json!({"by": 2})),
            )
            .unwrap_err();
        assert_eq!(err.kind, ProjectionErrorKind::UnsupportedEvent);
        assert!(err.to_string().contains("counter.multiplied"));
        let err = Counter
            .apply(
                CounterState::default(),
                &event(1, "counter.incremented", json!({"by": u64::MAX})),
            )
            .unwrap_err();
        assert_eq!(err.kind, ProjectionErrorKind::RejectedTransition);
    }

    #[test]
    fn inventory_updates_nested_fields() {
        let events = [
            event(1, "inventory.warehouse_named", json!({"name": "North"})),
            event(
                2,
                "inventory.item_added",
                json!({"sku": "A-1", "name": "Bolt", "quantity": 10}),
            ),
            event(
                3,
                "inventory.item_added",
                json!({"sku": "B-2", "name": "Nut", "quantity": 0}),
            ),
            event(
                4,
                "inventory.reserved",
                json!({"sku": "A-1", "quantity": 4}),
            ),
            event(5, "inventory.shipped", json!({"sku": "A-1", "quantity": 3})),
            event(
                6,
                "inventory.restocked",
                json!({"sku": "B-2", "quantity": 5}),
            ),
            event(7, "inventory.discontinued", json!({"sku": "B-2"})),
        ];
        let state = fold(&Inventory, &events).unwrap();
        assert_eq!(state.warehouse.as_deref(), Some("North"));
        assert_eq!(state.items.len(), 1);
        assert_eq!(
            state.items["A-1"],
            Item {
                name: "Bolt".into(),
                on_hand: 6,
                reserved: 1
            }
        );
        assert_eq!(state.discontinued, vec!["B-2".to_owned()]);
        assert_eq!(state.shipped_total, 3);
        // Deterministic: same events, same state.
        assert_eq!(fold(&Inventory, &events).unwrap(), state);
        // JSON round trip.
        let value = serde_json::to_value(&state).unwrap();
        assert_eq!(
            serde_json::from_value::<InventoryState>(value).unwrap(),
            state
        );
    }

    #[test]
    fn inventory_rejects_invalid_transitions() {
        let base = fold(
            &Inventory,
            &[event(
                1,
                "inventory.item_added",
                json!({"sku": "A", "name": "a", "quantity": 2}),
            )],
        )
        .unwrap();
        let cases = [
            (
                "inventory.item_added",
                json!({"sku": "A", "name": "dup", "quantity": 1}),
                ProjectionErrorKind::RejectedTransition,
            ),
            (
                "inventory.reserved",
                json!({"sku": "A", "quantity": 3}),
                ProjectionErrorKind::RejectedTransition,
            ),
            (
                "inventory.shipped",
                json!({"sku": "A", "quantity": 1}),
                ProjectionErrorKind::RejectedTransition,
            ),
            (
                "inventory.restocked",
                json!({"sku": "Z", "quantity": 1}),
                ProjectionErrorKind::RejectedTransition,
            ),
            (
                "inventory.restocked",
                json!({"sku": "A", "quantity": 0}),
                ProjectionErrorKind::InvalidPayload,
            ),
            (
                "inventory.restocked",
                json!({"sku": "A"}),
                ProjectionErrorKind::InvalidPayload,
            ),
            (
                "inventory.teleported",
                json!({}),
                ProjectionErrorKind::UnsupportedEvent,
            ),
        ];
        for (ty, payload, kind) in cases {
            let err = Inventory
                .apply(base.clone(), &event(2, ty, payload))
                .unwrap_err();
            assert_eq!(err.kind, kind, "{ty}: {err}");
        }
        let reserved = Inventory
            .apply(
                base,
                &event(2, "inventory.reserved", json!({"sku": "A", "quantity": 1})),
            )
            .unwrap();
        let err = Inventory
            .apply(
                reserved.clone(),
                &event(3, "inventory.discontinued", json!({"sku": "A"})),
            )
            .unwrap_err();
        assert_eq!(err.kind, ProjectionErrorKind::RejectedTransition);
        let discontinued = Inventory
            .apply(
                Inventory
                    .apply(
                        reserved,
                        &event(3, "inventory.shipped", json!({"sku": "A", "quantity": 1})),
                    )
                    .unwrap(),
                &event(4, "inventory.discontinued", json!({"sku": "A"})),
            )
            .unwrap();
        // A discontinued SKU cannot be re-added.
        let err = Inventory
            .apply(
                discontinued,
                &event(
                    5,
                    "inventory.item_added",
                    json!({"sku": "A", "name": "a", "quantity": 1}),
                ),
            )
            .unwrap_err();
        assert_eq!(err.kind, ProjectionErrorKind::RejectedTransition);
    }
}
