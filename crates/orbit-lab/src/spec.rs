//! Experiment specifications.
//!
//! A spec is what a caller asks for. It is validated, then stored in its
//! canonical form with every default filled in, so two requests that mean
//! the same thing (members in another order, defaults omitted or written
//! out) have the same [`spec_digest`](DifferentialSpec::digest).

use orbit_core::replay::MAX_REPLAY_EVENTS;
use orbit_core::{Digest, StreamId};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::catalog::Catalog;
use crate::digest::{SPEC_TAG, value_digest};
use crate::error::{LabError, Result};

/// Version of the specification format.
pub const SPEC_VERSION: u32 = 1;

/// The only experiment kind in this version.
pub const DIFFERENTIAL_REPLAY: &str = "differential_replay";

/// Largest number of events one differential replay may fold (the same cap
/// as `orbit-core` replay runs).
pub const MAX_EVENTS: u64 = MAX_REPLAY_EVENTS;

/// One of the two implementations in a differential replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    /// The `left` implementation.
    Left,
    /// The `right` implementation.
    Right,
}

impl Side {
    /// `"left"` or `"right"`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
        }
    }
}

fn yes() -> bool {
    true
}

/// A differential replay: run `left` and `right` over the same events of
/// `stream` and compare their states.
///
/// Both implementations always start from their own initial state at
/// sequence 0 and fold events `1..=to` in sequence order. States are
/// compared at every boundary `b` with `from <= b <= to`, where boundary
/// `b` is the state after event `b` (boundary 0 is the initial state).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DifferentialSpec {
    /// Stream to replay.
    pub stream: String,
    /// First compared boundary. Default 0.
    #[serde(default)]
    pub from: u64,
    /// Last boundary (inclusive). `None` means the stream's head when the
    /// experiment runs; the resolved value is recorded with the result.
    #[serde(default)]
    pub to: Option<u64>,
    /// Implementation id of the left side.
    pub left: String,
    /// Implementation id of the right side.
    pub right: String,
    /// Which side, if any, is the reference ("expected") behaviour. With
    /// `None`, results describe differences between peers and neither side
    /// is called wrong.
    #[serde(default)]
    pub reference: Option<Side>,
    /// Execute each side a second time on a fresh instance and require
    /// identical traces. Default `true`. Without it, results are never
    /// reported as reproducible.
    #[serde(default = "yes")]
    pub determinism_check: bool,
}

impl DifferentialSpec {
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

    /// Checks the spec against the catalog, without touching any store.
    ///
    /// # Errors
    /// [`LabError::InvalidSpec`] or [`LabError::UnknownImplementation`].
    pub fn validate(&self, catalog: &Catalog) -> Result<StreamId> {
        let stream = StreamId::new(self.stream.as_str()).map_err(|e| LabError::InvalidSpec {
            field: "stream",
            reason: e.to_string(),
        })?;
        for (field, id) in [("left", &self.left), ("right", &self.right)] {
            if catalog.find(id).is_none() {
                return Err(if id.is_empty() {
                    LabError::InvalidSpec {
                        field,
                        reason: "must name an implementation".to_owned(),
                    }
                } else {
                    LabError::UnknownImplementation(id.clone())
                });
            }
        }
        if let Some(to) = self.to {
            if self.from > to {
                return Err(LabError::InvalidSpec {
                    field: "from",
                    reason: format!("from ({}) is after to ({to})", self.from),
                });
            }
            if to > MAX_EVENTS {
                return Err(LabError::InvalidSpec {
                    field: "to",
                    reason: format!("at most {MAX_EVENTS} events can be replayed"),
                });
            }
        }
        if self.from > MAX_EVENTS {
            return Err(LabError::InvalidSpec {
                field: "from",
                reason: format!("at most {MAX_EVENTS} events can be replayed"),
            });
        }
        Ok(stream)
    }

    /// The stored form: `{"kind", "spec_version", "spec"}` with every field
    /// of the spec written out.
    #[must_use]
    pub fn envelope(&self) -> Value {
        json!({
            "kind": DIFFERENTIAL_REPLAY,
            "spec_version": SPEC_VERSION,
            "spec": self,
        })
    }

    /// Digest of the canonical [`envelope`](Self::envelope).
    #[must_use]
    pub fn digest(&self) -> Digest {
        value_digest(SPEC_TAG, &self.envelope())
    }
}
