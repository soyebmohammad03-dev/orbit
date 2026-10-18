//! # orbit-lab
//!
//! Experiments over ORBIT histories, kept apart from the event store.
//!
//! * [`registry`]: a SQLite experiment registry in its own file (`lab.db`),
//!   with an enforced lifecycle (`created → running → completed | failed`).
//! * [`artifacts`]: content-addressed artifact files, verified on every read.
//! * [`identity`]: implementation descriptors that tell apart two
//!   implementations declaring the same projection name and version.
//! * [`catalog`]: registered implementations and the adapter that runs any
//!   `orbit_core::Projection`.
//! * [`differential`]: differential replay of two implementations over one
//!   ordered history, with first-divergence diagnosis.
//! * [`lab`]: [`Lab`], which ties them together.
//!
//! `orbit-lab` uses only the public API of `orbit-core`. It reads event
//! stores and never writes to them; it does not change their schema.
//!
//! ```no_run
//! use orbit_core::{EventStore, StoreConfig};
//! use orbit_lab::{Catalog, DifferentialSpec, Lab};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let store = EventStore::open(StoreConfig::new("data/orbit.db"))?;
//! let lab = Lab::open("data/lab".as_ref(), Catalog::builtin())?;
//! let spec = DifferentialSpec::from_json(serde_json::json!({
//!     "stream": "counter-demo",
//!     "left": "orbit.counter",
//!     "right": "example.counter-floor-at-zero",
//! }))?;
//! let record = lab.create_and_run(&spec, &store)?;
//! println!("{} {:?}", record.id, record.outcome);
//! # Ok(())
//! # }
//! ```

pub mod artifacts;
pub mod bundle;
pub mod catalog;
pub mod differential;
pub mod digest;
pub mod error;
pub mod fault;
pub mod identity;
pub mod lab;
pub mod performance;
pub mod registry;
pub mod reliability;
pub mod reproduction;
pub mod signing;
pub mod spec;
pub mod variants;

pub use catalog::{Catalog, Implementation, ProjectionImplementation, StepFailure, Stepper};
pub use differential::{DifferentialResult, Outcome};
pub use error::{LabError, Result};
pub use identity::{
    BuildInfo, Determinism, IdentityAssessment, IdentityCompleteness, ImplementationDescriptor,
    ImplementationId,
};
pub use lab::{ArtifactVerification, Lab, TraceEntry, TracePage};
pub use registry::{ExperimentId, ExperimentRecord, ExperimentSummary, Status};
pub use spec::{DifferentialSpec, Side};
