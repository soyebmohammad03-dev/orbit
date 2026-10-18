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

/// The build that created an experiment. Recorded with every experiment,
/// but not part of any fingerprint, so fingerprints stay comparable across
/// builds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildInfo {
    /// `orbit-lab` crate version (the workspace version).
    pub orbit_version: String,
    /// Git commit the lab was built from, when the build could determine it.
    /// Uncommitted changes are not detected.
    pub git_commit: Option<String>,
    /// SHA-256 of the executable file that was running, when it could be
    /// read. Absent in experiments recorded before registry schema v2.
    #[serde(default)]
    pub executable_digest: Option<Digest>,
}

impl BuildInfo {
    /// The running build. The executable digest is computed once per process.
    #[must_use]
    pub fn current() -> Self {
        Self {
            orbit_version: env!("CARGO_PKG_VERSION").to_owned(),
            git_commit: option_env!("ORBIT_LAB_GIT_COMMIT").map(str::to_owned),
            executable_digest: executable_digest(),
        }
    }
}

/// SHA-256 of the running executable, read from `std::env::current_exe()`
/// once per process. `None` if the file cannot be read.
///
/// It identifies the exact compiled code of *every* implementation in the
/// binary at once, so it is coarse: any change anywhere in the program
/// changes it. A file replaced on disk after the process started is not
/// detected.
#[must_use]
pub fn executable_digest() -> Option<Digest> {
    static DIGEST: std::sync::OnceLock<Option<Digest>> = std::sync::OnceLock::new();
    *DIGEST.get_or_init(|| {
        let bytes = std::fs::read(std::env::current_exe().ok()?).ok()?;
        Some(crate::digest::content_digest(&bytes))
    })
}

/// How completely an experiment's implementations are identified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityCompleteness {
    /// Only the declared descriptor (id, name, projection key, type name,
    /// configuration, determinism). A code change that keeps these is invisible.
    Declared,
    /// Plus the Git commit the binary was built from. Uncommitted changes and
    /// the build environment are not captured.
    SourceRevision,
    /// Plus the SHA-256 of the executable that ran: the exact compiled code,
    /// for all implementations together.
    Executable,
}

/// One side's declared and configuration identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SideIdentityReport {
    /// `"left"` or `"right"`.
    pub side: String,
    /// Implementation id.
    pub implementation: String,
    /// Declared projection key.
    pub projection: Value,
    /// Descriptor fingerprint (declared identity).
    pub fingerprint: Option<Digest>,
    /// Configuration digest (configuration identity).
    pub config_digest: Option<Digest>,
}

/// Declared, configuration and build identity of an experiment, with an
/// honest completeness level. Derived from stored data, never stored itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IdentityAssessment {
    /// Declared and configuration identity per side.
    pub sides: Vec<SideIdentityReport>,
    /// The build that created the experiment.
    pub build: BuildInfo,
    /// Completeness level.
    pub completeness: IdentityCompleteness,
    /// What the identity does not capture, in plain words.
    pub gaps: Vec<String>,
}

impl IdentityAssessment {
    /// Assesses stored descriptors (`[{"side", "descriptor"}]`) and a build.
    #[must_use]
    pub fn assess(implementations: &Value, build: &BuildInfo) -> Self {
        let digest = |v: &Value, pointer: &str| {
            v.pointer(pointer)
                .and_then(Value::as_str)
                .and_then(|h| Digest::from_hex(h).ok())
        };
        let sides = implementations
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .map(|e| SideIdentityReport {
                        side: e["side"].as_str().unwrap_or_default().to_owned(),
                        implementation: e
                            .pointer("/descriptor/id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        projection: e
                            .pointer("/descriptor/projection")
                            .cloned()
                            .unwrap_or(Value::Null),
                        fingerprint: digest(e, "/descriptor/fingerprint"),
                        config_digest: digest(e, "/descriptor/config_digest"),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let completeness = if build.executable_digest.is_some() {
            IdentityCompleteness::Executable
        } else if build.git_commit.is_some() {
            IdentityCompleteness::SourceRevision
        } else {
            IdentityCompleteness::Declared
        };
        let mut gaps = vec![
            "fingerprints cover registration metadata and configuration, not code; they do not establish semantic equivalence".to_owned(),
        ];
        match completeness {
            IdentityCompleteness::Executable => gaps.push(
                "the executable digest covers the whole binary, so it changes with any code change and cannot single out one implementation".to_owned(),
            ),
            IdentityCompleteness::SourceRevision => {
                gaps.push("no executable digest was recorded".to_owned());
                gaps.push("uncommitted changes at build time are not detected".to_owned());
            }
            IdentityCompleteness::Declared => {
                gaps.push("no source revision or executable digest was recorded".to_owned());
            }
        }
        Self {
            sides,
            build: build.clone(),
            completeness,
            gaps,
        }
    }
}
