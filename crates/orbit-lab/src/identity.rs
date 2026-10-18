//! Implementation identity.
//!
//! `orbit-core` identifies a projection by its [`ProjectionKey`] (name and
//! declared version). That key says which *contract* a state follows, but
//! two different pieces of code can declare the same key: a refactor, a bug
//! fix that forgot to bump the version, or a candidate rewrite. The lab
//! therefore identifies an *implementation* by an
//! [`ImplementationDescriptor`]:
//!
//! | Field | Meaning |
//! |-------|---------|
//! | `id` | Stable, unique registry identifier, e.g. `orbit.counter`. The only unique key. |
//! | `name` | Human-readable label. Never used as an identity. |
//! | `projection` | The declared [`ProjectionKey`]. Several implementations may share one. |
//! | `adapter` | How the lab drives the code, e.g. [`RUST_PROJECTION_ADAPTER`]. |
//! | `implementation_type` | The Rust type name (`std::any::type_name`). |
//! | `config`, `config_digest` | Implementation configuration (canonical JSON) and its digest. |
//! | `determinism` | What the implementation declares about its determinism. |
//! | `fingerprint` | Digest over all of the fields above. |
//!
//! # What the fingerprint guarantees
//!
//! The fingerprint changes whenever any recorded identity field changes, and
//! it does not depend on JSON member order. Two descriptors with the same
//! `name` and `projection` but a different `id`, type or configuration have
//! different fingerprints.
//!
//! It does **not** hash source code or machine code, so it cannot detect a
//! change to an implementation's body that keeps its id, type name and
//! configuration. It says nothing about semantic equivalence: two
//! implementations with different fingerprints may behave identically, and
//! behaviour is only established by running them (differential replay). The
//! Rust type name is not guaranteed stable across compiler versions. The
//! build ([`BuildInfo`]) is recorded next to the descriptor, not inside the
//! fingerprint, so a fingerprint stays comparable across commits.

use std::fmt;

use orbit_core::{Digest, ProjectionKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::digest::{CONFIG_TAG, IMPLEMENTATION_TAG, value_digest};
use crate::error::{LabError, Result};

/// Adapter name of implementations that are `orbit_core::Projection` values.
pub const RUST_PROJECTION_ADAPTER: &str = "orbit-core/projection";

/// Version of the descriptor layout covered by the fingerprint.
pub const DESCRIPTOR_VERSION: u32 = 1;

/// Longest accepted implementation identifier.
pub const MAX_IMPLEMENTATION_ID_LEN: usize = 128;

/// A stable, unique implementation identifier: lowercase ASCII letters,
/// digits, `.`, `_` and `-`, starting with a letter or digit, at most
/// [`MAX_IMPLEMENTATION_ID_LEN`] bytes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ImplementationId(String);

impl ImplementationId {
    /// Validates and wraps `value`.
    ///
    /// # Errors
    /// [`LabError::InvalidDescriptor`] if `value` is not of the documented form.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= MAX_IMPLEMENTATION_ID_LEN
            && value
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            && value.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
            });
        if valid {
            Ok(Self(value))
        } else {
            Err(LabError::InvalidDescriptor(format!(
                "implementation id {value:?} must be 1-{MAX_IMPLEMENTATION_ID_LEN} bytes of [a-z0-9._-], starting with [a-z0-9]"
            )))
        }
    }

    /// The identifier as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ImplementationId {
    type Error = LabError;
    fn try_from(value: String) -> Result<Self> {
        Self::new(value)
    }
}

impl From<ImplementationId> for String {
    fn from(value: ImplementationId) -> Self {
        value.0
    }
}

impl fmt::Display for ImplementationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What an implementation declares about its determinism.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Determinism {
    /// Follows the `Projection` contract: the result depends only on the
    /// state and the event (no clocks, randomness, I/O or global state).
    /// This is a declaration; the lab checks it by executing twice, which can
    /// disprove it but never prove it.
    Pure,
    /// Depends on state the lab does not control (for example a clock or a
    /// global). Results are never reported as reproducible.
    Uncontrolled,
}

/// Identity of one implementation. Build with [`ImplementationDescriptor::new`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImplementationDescriptor {
    /// Layout version ([`DESCRIPTOR_VERSION`]).
    pub descriptor_version: u32,
    /// Unique identifier.
    pub id: ImplementationId,
    /// Display name. Not unique.
    pub name: String,
    /// Declared projection name and version. Not unique.
    pub projection: ProjectionKey,
    /// Adapter kind.
    pub adapter: String,
    /// Rust type name of the implementation.
    pub implementation_type: String,
    /// Configuration, as canonical JSON.
    pub config: Value,
    /// Digest of `config`.
    pub config_digest: Digest,
    /// Determinism declaration.
    pub determinism: Determinism,
    /// Digest over every field above.
    pub fingerprint: Digest,
}

impl ImplementationDescriptor {
    /// Builds a descriptor and computes its digests.
    ///
    /// # Errors
    /// [`LabError::InvalidDescriptor`] for an empty name, adapter or type.
    pub fn new(
        id: ImplementationId,
        name: impl Into<String>,
        projection: ProjectionKey,
        adapter: impl Into<String>,
        implementation_type: impl Into<String>,
        config: Value,
        determinism: Determinism,
    ) -> Result<Self> {
        let name = name.into();
        let adapter = adapter.into();
        let implementation_type = implementation_type.into();
        for (field, value) in [
            ("name", &name),
            ("adapter", &adapter),
            ("implementation_type", &implementation_type),
        ] {
            if value.trim().is_empty() {
                return Err(LabError::InvalidDescriptor(format!("{field} is empty")));
            }
        }
        let config_digest = value_digest(CONFIG_TAG, &config);
        let mut descriptor = Self {
            descriptor_version: DESCRIPTOR_VERSION,
            id,
            name,
            projection,
            adapter,
            implementation_type,
            config,
            config_digest,
            determinism,
            fingerprint: Digest::GENESIS,
        };
        descriptor.fingerprint = descriptor.computed_fingerprint();
        Ok(descriptor)
    }

    /// Recomputes the fingerprint from the other fields.
    #[must_use]
    pub fn computed_fingerprint(&self) -> Digest {
        let body = serde_json::json!({
            "descriptor_version": self.descriptor_version,
            "id": self.id,
            "name": self.name,
            "projection": self.projection,
            "adapter": self.adapter,
            "implementation_type": self.implementation_type,
            "config_digest": value_digest(CONFIG_TAG, &self.config),
            "determinism": self.determinism,
        });
        value_digest(IMPLEMENTATION_TAG, &body)
    }
}

/// The build that ran an experiment. Recorded with every experiment, but not
/// part of any fingerprint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildInfo {
    /// `orbit-lab` crate version (the workspace version).
    pub orbit_version: String,
    /// Git commit the lab was built from, when the build could determine it.
    /// Uncommitted changes are not detected.
    pub git_commit: Option<String>,
}

impl BuildInfo {
    /// The running build.
    #[must_use]
    pub fn current() -> Self {
        Self {
            orbit_version: env!("CARGO_PKG_VERSION").to_owned(),
            git_commit: option_env!("ORBIT_LAB_GIT_COMMIT").map(str::to_owned),
        }
    }
}
