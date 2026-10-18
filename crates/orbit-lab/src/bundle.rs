//! Portable experiment evidence bundles.
//!
//! A bundle is one JSON document (canonical: sorted keys, no whitespace)
//! that carries a finished experiment and the evidence needed to check it
//! elsewhere:
//!
//! ```text
//! {
//!   "format": "orbit-evidence-bundle",
//!   "format_version": 1,
//!   "manifest": {                       integrity manifest
//!     "experiment_id", "kind", "exporter": {build},
//!     "document_digest": sha256 of the canonical document (tagged),
//!     "entries": [{"name", "media_type", "size", "sha256"}, …]
//!   },
//!   "document": {
//!     "experiment": {the experiment record: spec, digests, identity,
//!                    build, input, result, failure, artifact refs},
//!     "verifications": [its verification history],
//!     "history": {"included", "entry", "events", "reason"},
//!     "provenance": {…}, "limitations": […]
//!   },
//!   "entries": {"artifacts/<role>": base64, "history/events.json": base64}
//! }
//! ```
//!
//! The **bundle digest** is the tagged SHA-256 of the canonical manifest.
//! Because the manifest holds the document digest and every entry digest,
//! it commits to the whole bundle.
//!
//! # Validation
//!
//! [`validate`] trusts nothing until it has checked it, in this order: size
//! ([`MAX_BUNDLE_BYTES`]), nesting depth ([`MAX_DEPTH`]), JSON syntax
//! (truncation is reported as such), the exact set of fields (unknown or
//! missing fields are errors), format and version, entry names (a strict
//! pattern; no `..`, absolute paths, backslashes or control characters),
//! entry count and sizes, strict base64, every entry digest, the document
//! digest, the experiment's own spec and result digests, its artifact
//! references, and, when included, every event digest and the digest
//! chain of the input history.
//!
//! Entries are never written under their names: imported entry bytes are
//! stored content-addressed by digest, like any artifact, so a name cannot
//! address a file. Nothing in a bundle is ever executed. Bundles are not
//! compressed, so there is no decompression step to bound.
//!
//! # What a valid bundle shows
//!
//! *Integrity*: the bundle is internally consistent: every digest matches
//! its content, and any change to the content changes the bundle digest.
//! It is tamper-evident **relative to a bundle digest obtained from a
//! trusted channel**: anyone can build a consistent bundle, under a new
//! digest. *Authenticity* is separate: optional signatures
//! ([`crate::signing`]) count only for keys in a trust store the verifier
//! configured, and the report says `unsigned`, `signed_untrusted` or
//! `trusted`. Neither says anything about scientific correctness.
//! Reproduction ([`Lab::reproduce_bundle`]) is the step that re-executes
//! the experiment, where this platform can.

use std::collections::BTreeMap;

use orbit_core::domain::canonical::to_canonical_string;
use orbit_core::{Digest, EventStore, Sequence, StoredEvent, StreamId, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::artifacts::ArtifactState;
use crate::differential::MemoryHistory;
use crate::digest::{RESULT_TAG, content_digest, value_digest};
use crate::error::{LabError, Result};
use crate::identity::{BuildInfo, IdentityAssessment};
use crate::performance::{self, RegressionPolicy};
use crate::registry::{
    ArtifactRef, BundleVerification, ExperimentId, ExperimentRecord, Failure, ImportedBundle,
    InputRange, Status,
};
use crate::reproduction::ReproductionStatus;
use crate::{Lab, reliability, spec};

/// Value of the `format` field.
pub const FORMAT: &str = "orbit-evidence-bundle";
/// The only supported format version.
pub const FORMAT_VERSION: u32 = 1;
/// Largest accepted bundle, in bytes.
pub const MAX_BUNDLE_BYTES: usize = 32 * 1024 * 1024;
/// Most entries in a bundle.
pub const MAX_ENTRIES: usize = 64;
/// Largest decoded entry, in bytes.
pub const MAX_ENTRY_BYTES: usize = 16 * 1024 * 1024;
/// Deepest accepted JSON nesting.
pub const MAX_DEPTH: usize = 64;
/// Longest entry name.
pub const MAX_NAME_LEN: usize = 128;
/// Tag of the bundle digest (over the canonical manifest).
pub const BUNDLE_TAG: &str = "ORBIT-EVIDENCE-BUNDLE-V1";
/// Tag of the document digest.
pub const DOCUMENT_TAG: &str = "ORBIT-EVIDENCE-DOCUMENT-V1";
/// Name of the input history entry.
pub const HISTORY_ENTRY: &str = "history/events.json";
/// Media types an entry may declare.
pub const MEDIA_TYPES: [&str; 3] = ["application/octet-stream", "application/json", "text/plain"];

/// Limitations recorded in every bundle.
pub const LIMITATIONS: &[&str] = &[
    "a valid bundle is internally consistent (every digest matches) and its bundle digest identifies its content; unless it is signed by a key you trust, compare that digest with one from a trusted source to detect a substituted bundle",
    "integrity is not scientific correctness: reproduction re-executes the experiment where this platform can",
    "a matching executable digest identifies the same binary, not semantically equivalent code",
];

// ---------------------------------------------------------------- base64

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding (RFC 4648 §4).
#[must_use]
pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Strict decoding: only the canonical encoding of some byte string is
/// accepted (no whitespace, no missing or extra padding, no stray bits).
///
/// # Errors
/// If `text` is not canonical base64.
pub fn base64_decode(text: &str) -> std::result::Result<Vec<u8>, String> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return Err("length is not a multiple of 4".to_owned());
    }
    let value = |c: u8| ALPHABET.iter().position(|a| *a == c).map(|p| p as u32);
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for (i, chunk) in bytes.chunks(4).enumerate() {
        let last = i + 1 == bytes.len() / 4;
        let pad = chunk.iter().rev().take_while(|c| **c == b'=').count();
        if pad > 2 || (pad > 0 && !last) {
            return Err("misplaced padding".to_owned());
        }
        let mut n = 0u32;
        for (k, c) in chunk.iter().enumerate() {
            let v = if k >= 4 - pad {
                0
            } else {
                value(*c).ok_or("invalid character")?
            };
            n = (n << 6) | v;
        }
        let decoded = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&decoded[..3 - pad]);
    }
    if base64_encode(&out) != text {
        return Err("not the canonical encoding".to_owned());
    }
    Ok(out)
}

// ------------------------------------------------------------- the format

/// One manifest entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntryInfo {
    /// Entry name (see [`valid_entry_name`]).
    pub name: String,
    /// One of [`MEDIA_TYPES`].
    pub media_type: String,
    /// Decoded size in bytes.
    pub size: u64,
    /// SHA-256 of the decoded bytes.
    pub sha256: Digest,
}

/// The integrity manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Experiment id in the exporting lab.
    pub experiment_id: ExperimentId,
    /// Experiment kind.
    pub kind: String,
    /// Build that exported the bundle.
    pub exporter: BuildInfo,
    /// Tagged digest of the canonical document.
    pub document_digest: Digest,
    /// Every entry, sorted by name.
    pub entries: Vec<EntryInfo>,
}

/// The experiment record as carried in a bundle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundledExperiment {
    /// Id in the exporting lab.
    pub id: ExperimentId,
    /// Kind.
    pub kind: String,
    /// Spec format version.
    pub spec_version: u32,
    /// Spec.
    pub spec: Value,
    /// Envelope digest of the spec.
    pub spec_digest: Digest,
    /// Implementation descriptors.
    pub implementations: Value,
    /// Creating build.
    pub build: BuildInfo,
    /// Status (completed or failed).
    pub status: Status,
    /// Created.
    pub created_at: Timestamp,
    /// Started.
    pub started_at: Option<Timestamp>,
    /// Finished.
    pub finished_at: Option<Timestamp>,
    /// Elapsed.
    pub elapsed_micros: Option<u64>,
    /// Pinned input.
    pub input: Option<InputRange>,
    /// Outcome.
    pub outcome: Option<String>,
    /// Result.
    pub result: Option<Value>,
    /// Result digest.
    pub result_digest: Option<Digest>,
    /// Reproducibility.
    pub reproducibility: Option<String>,
    /// Failure.
    pub failure: Option<Failure>,
    /// Artifact references.
    pub artifacts: Vec<ArtifactRef>,
    /// Identity assessment as exported (recomputed on import).
    pub identity: Value,
}

impl BundledExperiment {
    fn from_record(record: &ExperimentRecord) -> Result<Self> {
        Ok(serde_json::from_value(serde_json::to_value(record)?)?)
    }

    /// The record, with its identity assessment recomputed.
    #[must_use]
    pub fn to_record(&self) -> ExperimentRecord {
        ExperimentRecord {
            id: self.id,
            kind: self.kind.clone(),
            spec_version: self.spec_version,
            spec: self.spec.clone(),
            spec_digest: self.spec_digest,
            identity: IdentityAssessment::assess(&self.implementations, &self.build),
            implementations: self.implementations.clone(),
            build: self.build.clone(),
            status: self.status,
            created_at: self.created_at,
            started_at: self.started_at,
            finished_at: self.finished_at,
            elapsed_micros: self.elapsed_micros,
            input: self.input.clone(),
            outcome: self.outcome.clone(),
            result: self.result.clone(),
            result_digest: self.result_digest,
            reproducibility: self.reproducibility.clone(),
            failure: self.failure.clone(),
            artifacts: self.artifacts.clone(),
        }
    }
}

/// Whether the input history travels with the bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryInfo {
    /// Whether events `1..=to` are included.
    pub included: bool,
    /// Their entry name, when included.
    pub entry: Option<String>,
    /// Number of events, when included.
    pub events: Option<u64>,
    /// Why they are not included, when they are not.
    pub reason: Option<String>,
}

/// The bundle document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    /// The experiment.
    pub experiment: BundledExperiment,
    /// Its verification history in the exporting lab.
    pub verifications: Vec<Value>,
    /// Input history.
    pub history: HistoryInfo,
    /// Exporting environment and what the bundle claims.
    pub provenance: Value,
    /// Limitations.
    pub limitations: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBundle {
    format: String,
    format_version: u32,
    manifest: Value,
    document: Value,
    entries: BTreeMap<String, String>,
    /// Optional signatures over the bundle digest (not covered by it).
    #[serde(default)]
    signatures: Vec<crate::signing::BundleSignature>,
}

/// An exported bundle.
#[derive(Debug, Clone)]
pub struct ExportedBundle {
    /// Canonical JSON bytes.
    pub bytes: Vec<u8>,
    /// Bundle digest.
    pub digest: Digest,
    /// Whether the input history is included.
    pub history_included: bool,
}

/// Whether `name` is an acceptable entry name: 1 to [`MAX_NAME_LEN`]
/// characters of `[a-z0-9_.-]` in `/`-separated segments, each starting
/// with a letter or digit (so no `..`, no hidden files, no empty segments),
/// and no leading `/`.
#[must_use]
pub fn valid_entry_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name.split('/').all(|seg| {
            seg.as_bytes()
                .first()
                .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                && seg.bytes().all(|c| {
                    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'.' | b'-')
                })
        })
}

fn depth_exceeds(bytes: &[u8], max: usize) -> bool {
    let (mut depth, mut in_string, mut escape) = (0usize, false, false);
    for &b in bytes {
        if in_string {
            match (escape, b) {
                (true, _) => escape = false,
                (false, b'\\') => escape = true,
                (false, b'"') => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'[' | b'{' => {
                depth += 1;
                if depth > max {
                    return true;
                }
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    false
}

// ------------------------------------------------------------ validation

/// A problem found by [`validate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Issue {
    /// Stable code: `too_large`, `too_deeply_nested`, `truncated`,
    /// `malformed_json`, `invalid_structure`, `not_a_bundle`,
    /// `unsupported_version`, `unsupported_kind`, `unsafe_entry_name`,
    /// `too_many_entries`, `entry_too_large`, `invalid_encoding`,
    /// `missing_entry`, `unlisted_entry`, `unsupported_media_type`,
    /// `digest_mismatch`, `artifact_missing`, `history_mismatch`,
    /// `inconsistent_record`.
    pub code: &'static str,
    /// What is wrong.
    pub message: String,
}

/// The outcome of [`validate`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ValidationReport {
    /// Whether every check passed. Only valid bundles can be imported.
    pub valid: bool,
    /// The bundle digest, once the manifest could be read.
    pub bundle_digest: Option<Digest>,
    /// Format version, once readable.
    pub format_version: Option<u32>,
    /// Experiment id in the bundle.
    pub experiment_id: Option<ExperimentId>,
    /// Kind.
    pub kind: Option<String>,
    /// Status in the bundle.
    pub status: Option<Status>,
    /// Outcome in the bundle.
    pub outcome: Option<String>,
    /// Problems, in the order found.
    pub errors: Vec<Issue>,
    /// Checks that passed.
    pub passed: Vec<String>,
    /// `verified` (every digest matches) or `failed`.
    pub integrity: &'static str,
    /// Always `not_verified`: validation checks consistency, not correctness.
    pub scientific_correctness: &'static str,
    /// `included_and_verified`, `not_included` or `invalid`.
    pub history: &'static str,
    /// Whether this platform can re-execute the experiment from the bundle
    /// alone, and why not.
    pub independently_replayable: bool,
    /// The reason for `independently_replayable`.
    pub replayability: String,
    /// What the bundle's signatures establish, against the trust store
    /// given to [`validate_with`] (none for [`validate`]).
    pub authenticity: crate::signing::Authenticity,
    /// Limitations of what validation shows.
    pub limitations: Vec<&'static str>,
}

/// A bundle that passed every check.
#[derive(Debug, Clone)]
pub struct ValidBundle {
    /// Bundle digest.
    pub digest: Digest,
    /// Manifest.
    pub manifest: Manifest,
    /// Document.
    pub document: Document,
    /// Decoded entries.
    pub entries: BTreeMap<String, Vec<u8>>,
    /// The verified input history, when included.
    pub history: Option<MemoryHistory>,
    /// Its signatures (all verified).
    pub signatures: Vec<crate::signing::BundleSignature>,
}

struct Checker {
    report: ValidationReport,
}

impl Checker {
    fn fail(&mut self, code: &'static str, message: impl Into<String>) {
        self.report.errors.push(Issue {
            code,
            message: message.into(),
        });
    }
    fn pass(&mut self, check: &str) {
        self.report.passed.push(check.to_owned());
    }
    fn finish(mut self) -> ValidationReport {
        self.report.valid = self.report.errors.is_empty();
        self.report.integrity = if self.report.valid {
            "verified"
        } else {
            "failed"
        };
        if !self.report.valid {
            self.report.independently_replayable = false;
            self.report.replayability = "the bundle is not valid".to_owned();
        }
        self.report
    }
}

fn json_error(e: &serde_json::Error) -> &'static str {
    if e.is_eof() {
        "truncated"
    } else if e.is_syntax() {
        "malformed_json"
    } else {
        "invalid_structure"
    }
}

/// Validates bundle bytes without a trust store: a validly signed bundle is
/// `signed_untrusted` at best. Never panics on any input; never executes or
/// writes anything.
#[must_use]
pub fn validate(bytes: &[u8]) -> (ValidationReport, Option<ValidBundle>) {
    validate_with(bytes, None)
}

/// Validates bundle bytes, classifying signatures against `trust`.
#[must_use]
pub fn validate_with(
    bytes: &[u8],
    trust: Option<&crate::signing::TrustStore>,
) -> (ValidationReport, Option<ValidBundle>) {
    let mut c = Checker {
        report: ValidationReport {
            valid: false,
            bundle_digest: None,
            format_version: None,
            experiment_id: None,
            kind: None,
            status: None,
            outcome: None,
            errors: Vec::new(),
            passed: Vec::new(),
            integrity: "failed",
            scientific_correctness: "not_verified",
            history: "invalid",
            independently_replayable: false,
            replayability: String::new(),
            authenticity: crate::signing::authenticity(&[], trust),
            limitations: LIMITATIONS.to_vec(),
        },
    };
    let parsed = check(&mut c, bytes, trust);
    let report = c.finish();
    let valid = report.valid;
    (report, parsed.filter(|_| valid))
}

#[allow(clippy::too_many_lines)]
fn check(
    c: &mut Checker,
    bytes: &[u8],
    trust: Option<&crate::signing::TrustStore>,
) -> Option<ValidBundle> {
    if bytes.len() > MAX_BUNDLE_BYTES {
        c.fail(
            "too_large",
            format!(
                "{} bytes; at most {MAX_BUNDLE_BYTES} are accepted",
                bytes.len()
            ),
        );
        return None;
    }
    if depth_exceeds(bytes, MAX_DEPTH) {
        c.fail(
            "too_deeply_nested",
            format!("JSON nesting deeper than {MAX_DEPTH}"),
        );
        return None;
    }
    let raw: Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(e) => {
            c.fail(json_error(&e), format!("not a complete JSON document: {e}"));
            return None;
        }
    };
    if raw.get("format").and_then(Value::as_str) != Some(FORMAT) {
        c.fail(
            "not_a_bundle",
            format!("the format field is not {FORMAT:?}"),
        );
        return None;
    }
    match raw.get("format_version").and_then(Value::as_u64) {
        Some(v) if v == u64::from(FORMAT_VERSION) => {}
        other => {
            c.report.format_version = other.and_then(|v| u32::try_from(v).ok());
            c.fail(
                "unsupported_version",
                format!("format version {other:?}; this version reads only {FORMAT_VERSION}"),
            );
            return None;
        }
    }
    c.report.format_version = Some(FORMAT_VERSION);
    let raw: RawBundle = match serde_json::from_value(raw) {
        Ok(r) => r,
        Err(e) => {
            c.fail("invalid_structure", e.to_string());
            return None;
        }
    };
    let _ = (&raw.format, raw.format_version);
    if raw.signatures.len() > crate::signing::MAX_SIGNATURES {
        c.fail(
            "malformed_signature",
            format!(
                "at most {} signatures are accepted",
                crate::signing::MAX_SIGNATURES
            ),
        );
        return None;
    }
    let manifest: Manifest = match serde_json::from_value(raw.manifest.clone()) {
        Ok(m) => m,
        Err(e) => {
            c.fail("invalid_structure", format!("manifest: {e}"));
            return None;
        }
    };
    // One representation per manifest: a digest in upper-case hex, say,
    // would parse to the same value but give another bundle digest.
    if serde_json::to_value(&manifest).ok().as_ref() != Some(&raw.manifest) {
        c.fail("invalid_structure", "the manifest is not in canonical form");
        return None;
    }
    let digest = value_digest(BUNDLE_TAG, &raw.manifest);
    c.report.bundle_digest = Some(digest);
    c.report.experiment_id = Some(manifest.experiment_id);
    c.report.kind = Some(manifest.kind.clone());
    c.pass("structure");

    // Entries.
    if raw.entries.len() > MAX_ENTRIES || manifest.entries.len() > MAX_ENTRIES {
        c.fail(
            "too_many_entries",
            format!("at most {MAX_ENTRIES} entries are accepted"),
        );
        return None;
    }
    let mut entries = BTreeMap::new();
    let mut listed = std::collections::BTreeSet::new();
    for info in &manifest.entries {
        if !valid_entry_name(&info.name) {
            c.fail(
                "unsafe_entry_name",
                format!("entry name {:?} is not allowed", info.name),
            );
            continue;
        }
        if !listed.insert(info.name.clone()) {
            c.fail(
                "invalid_structure",
                format!("entry {} is listed twice", info.name),
            );
            continue;
        }
        if !MEDIA_TYPES.contains(&info.media_type.as_str()) {
            c.fail(
                "unsupported_media_type",
                format!("{}: media type {:?}", info.name, info.media_type),
            );
        }
        let Some(text) = raw.entries.get(&info.name) else {
            c.fail(
                "missing_entry",
                format!("{} is listed but absent", info.name),
            );
            continue;
        };
        if text.len() / 4 * 3 > MAX_ENTRY_BYTES + 3 {
            c.fail(
                "entry_too_large",
                format!("{}: more than {MAX_ENTRY_BYTES} bytes", info.name),
            );
            continue;
        }
        let decoded = match base64_decode(text) {
            Ok(d) => d,
            Err(e) => {
                c.fail("invalid_encoding", format!("{}: {e}", info.name));
                continue;
            }
        };
        if decoded.len() as u64 != info.size || content_digest(&decoded) != info.sha256 {
            c.fail(
                "digest_mismatch",
                format!("{}: content does not match its size and digest", info.name),
            );
            continue;
        }
        entries.insert(info.name.clone(), decoded);
    }
    for name in raw.entries.keys() {
        if !listed.contains(name) {
            let code = if valid_entry_name(name) {
                "unlisted_entry"
            } else {
                "unsafe_entry_name"
            };
            c.fail(code, format!("entry {name:?} is not in the manifest"));
        }
    }
    if c.report.errors.is_empty() {
        c.pass("entries");
    }

    // Document.
    if value_digest(DOCUMENT_TAG, &raw.document) != manifest.document_digest {
        c.fail(
            "digest_mismatch",
            "the document does not match the manifest's document digest",
        );
        return None;
    }
    let document: Document = match serde_json::from_value(raw.document.clone()) {
        Ok(d) => d,
        Err(e) => {
            c.fail("invalid_structure", format!("document: {e}"));
            return None;
        }
    };
    if serde_json::to_value(&document).ok().as_ref() != Some(&raw.document) {
        c.fail("invalid_structure", "the document is not in canonical form");
        return None;
    }
    c.pass("document_digest");
    let e = &document.experiment;
    c.report.status = Some(e.status);
    c.report.outcome.clone_from(&e.outcome);
    if e.id != manifest.experiment_id || e.kind != manifest.kind {
        c.fail(
            "inconsistent_record",
            "the manifest and the experiment disagree on id or kind",
        );
    }
    if ![
        spec::DIFFERENTIAL_REPLAY,
        reliability::RELIABILITY,
        performance::PERFORMANCE,
    ]
    .contains(&e.kind.as_str())
    {
        c.fail("unsupported_kind", format!("experiment kind {:?}", e.kind));
        return None;
    }
    if !matches!(e.status, Status::Completed | Status::Failed) {
        c.fail(
            "inconsistent_record",
            format!("only finished experiments are bundled, not {}", e.status),
        );
    }
    if spec::envelope_digest(&e.kind, e.spec_version, &e.spec) != e.spec_digest {
        c.fail(
            "digest_mismatch",
            "the specification does not match its digest",
        );
    } else {
        c.pass("spec_digest");
    }
    match (&e.result, e.result_digest) {
        (Some(result), Some(d)) if value_digest(RESULT_TAG, result) == d => c.pass("result_digest"),
        (None, None) if e.status == Status::Failed && e.failure.is_some() => {}
        _ => c.fail("digest_mismatch", "the result does not match its digest"),
    }
    for artifact in &e.artifacts {
        let name = format!("artifacts/{}", artifact.role);
        match manifest.entries.iter().find(|i| i.name == name) {
            Some(i) if i.sha256 == artifact.digest && i.size == artifact.size => {}
            Some(_) => c.fail(
                "digest_mismatch",
                format!("entry {name} does not match the experiment's artifact reference"),
            ),
            None => c.fail(
                "artifact_missing",
                format!(
                    "the experiment's artifact {} is not included",
                    artifact.role
                ),
            ),
        }
    }
    if !e.artifacts.is_empty() {
        c.pass("artifact_references");
    }

    // History.
    let mut history = None;
    match (&document.history, e.input.as_ref()) {
        (
            HistoryInfo {
                included: true,
                entry: Some(entry),
                events: Some(n),
                ..
            },
            Some(input),
        ) if entry == HISTORY_ENTRY => {
            let verified = entries
                .get(HISTORY_ENTRY)
                .ok_or_else(|| "the history entry is missing".to_owned())
                .and_then(|bytes| {
                    serde_json::from_slice::<Vec<StoredEvent>>(bytes).map_err(|e| e.to_string())
                })
                .and_then(|events| MemoryHistory::verified(&input.stream, events))
                .and_then(|h| {
                    if h.len() != *n || h.len() != input.to {
                        return Err(format!(
                            "{} events for a range ending at {}",
                            h.len(),
                            input.to
                        ));
                    }
                    let mut range_spec = spec::DifferentialSpec::from_json(e.spec.clone())
                        .map_err(|e| e.to_string())?;
                    range_spec.to = Some(input.to);
                    let range = h.resolve(&range_spec).map_err(|f| f.message)?;
                    if &range == input {
                        Ok(h)
                    } else {
                        Err("the events do not match the experiment's pinned input".to_owned())
                    }
                });
            match verified {
                Ok(h) => {
                    c.report.history = "included_and_verified";
                    c.pass("history_chain");
                    history = Some(h);
                }
                Err(message) => c.fail("history_mismatch", message),
            }
        }
        (
            HistoryInfo {
                included: false,
                entry: None,
                events: None,
                ..
            },
            _,
        ) => {
            c.report.history = "not_included";
            if entries.contains_key(HISTORY_ENTRY) {
                c.fail(
                    "inconsistent_record",
                    "a history entry is present but declared not included",
                );
            }
        }
        _ => c.fail(
            "inconsistent_record",
            "the history declaration is inconsistent",
        ),
    }

    let (replayable, why) = match (e.kind.as_str(), e.status, &history) {
        (_, Status::Failed, _) => (false, "the experiment failed; there is no result to reproduce".to_owned()),
        (spec::DIFFERENTIAL_REPLAY, _, Some(_)) => (
            true,
            "the verified input history is included; reproduction also needs both implementations, with the recorded fingerprints, in this build".to_owned(),
        ),
        (spec::DIFFERENTIAL_REPLAY, _, None) => (
            false,
            format!(
                "the input history is not included ({})",
                document.history.reason.as_deref().unwrap_or("no reason given")
            ),
        ),
        (performance::PERFORMANCE, _, _) => (
            false,
            "timings cannot be replayed; reproduction measures the same workload again here and compares".to_owned(),
        ),
        _ => (
            false,
            "reliability scenarios are re-run only with the orbit-reliability tool".to_owned(),
        ),
    };
    c.report.independently_replayable = replayable;
    c.report.replayability = why;

    // Signatures cover the bundle digest; each must verify.
    let mut verified = Vec::new();
    for (i, sig) in raw.signatures.iter().enumerate() {
        if raw.signatures[..i].iter().any(|o| o.key_id == sig.key_id) {
            c.fail(
                "malformed_signature",
                format!("key id {:?} signs twice", sig.key_id),
            );
            continue;
        }
        match sig.verify(&digest) {
            Ok(key) => verified.push((sig.key_id.clone(), key)),
            Err((code, message)) => c.fail(code, message),
        }
    }
    c.report.authenticity = crate::signing::authenticity(&verified, trust);
    Some(ValidBundle {
        digest,
        manifest,
        document,
        entries,
        history,
        signatures: raw.signatures,
    })
}

// ------------------------------------------------------------ the lab side

/// The outcome of [`Lab::import_bundle`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ImportOutcome {
    /// The stored bundle.
    pub bundle: ImportedBundle,
    /// `false` if a bundle with this digest was already imported (nothing
    /// was changed).
    pub newly_imported: bool,
    /// Whether this lab has an experiment with the same id, and whether its
    /// spec and result digests equal the bundle's. Never changed by an import.
    pub local_experiment: Value,
}

impl Lab {
    /// Exports a finished experiment as a bundle. With `store`, a
    /// differential replay's input events `1..=to` are included if the
    /// stream still holds exactly the pinned history and the bundle stays
    /// within [`MAX_BUNDLE_BYTES`]; otherwise the bundle records why not.
    ///
    /// The bytes are deterministic: exporting the same record (with the
    /// same verification history) from the same build gives the same bytes.
    ///
    /// # Errors
    /// [`LabError::InvalidState`] unless finished; artifact errors (damaged
    /// evidence is never exported).
    pub fn export_bundle(
        &self,
        id: ExperimentId,
        store: Option<&EventStore>,
    ) -> Result<ExportedBundle> {
        let record = self.experiment(id)?;
        if !matches!(record.status, Status::Completed | Status::Failed) {
            return Err(LabError::InvalidState {
                id,
                reason: format!(
                    "only finished experiments can be exported (status is {})",
                    record.status
                ),
            });
        }
        let mut entries: BTreeMap<String, (String, Vec<u8>)> = BTreeMap::new();
        for artifact in &record.artifacts {
            let bytes = self.artifacts().read(&artifact.digest)?;
            entries.insert(
                format!("artifacts/{}", artifact.role),
                (artifact.media_type.clone(), bytes),
            );
        }
        let budget = MAX_BUNDLE_BYTES / 2;
        let used: usize = entries.values().map(|(_, b)| b.len()).sum();
        let history = match (record.kind.as_str(), record.input.as_ref(), store) {
            (spec::DIFFERENTIAL_REPLAY, Some(input), Some(store)) => {
                match history_events(store, input, budget.saturating_sub(used)) {
                    Ok(bytes) => {
                        entries.insert(
                            HISTORY_ENTRY.to_owned(),
                            ("application/json".to_owned(), bytes),
                        );
                        HistoryInfo {
                            included: true,
                            entry: Some(HISTORY_ENTRY.to_owned()),
                            events: Some(input.to),
                            reason: None,
                        }
                    }
                    Err(reason) => not_included(reason),
                }
            }
            (spec::DIFFERENTIAL_REPLAY, Some(_), None) => {
                not_included("the exporting server had no event store available".to_owned())
            }
            (spec::DIFFERENTIAL_REPLAY, None, _) => {
                not_included("the experiment failed before its input was resolved".to_owned())
            }
            (kind, _, _) => not_included(format!("{kind} experiments have no input event history")),
        };
        let document = Document {
            experiment: BundledExperiment::from_record(&record)?,
            verifications: self
                .verifications(id)?
                .iter()
                .map(serde_json::to_value)
                .collect::<std::result::Result<_, _>>()?,
            history,
            provenance: json!({
                "exported_from": "orbit-lab",
                "integrity": "every entry and the document are covered by the manifest digests; signatures, if added later, are reported separately as authenticity",
                "local_paths": "none: entries are addressed by name and digest only",
            }),
            limitations: LIMITATIONS.iter().map(|s| (*s).to_owned()).collect(),
        };
        let document_value = serde_json::to_value(&document)?;
        let manifest = Manifest {
            experiment_id: id,
            kind: record.kind.clone(),
            exporter: BuildInfo::current(),
            document_digest: value_digest(DOCUMENT_TAG, &document_value),
            entries: entries
                .iter()
                .map(|(name, (media_type, bytes))| EntryInfo {
                    name: name.clone(),
                    media_type: media_type.clone(),
                    size: bytes.len() as u64,
                    sha256: content_digest(bytes),
                })
                .collect(),
        };
        let manifest_value = serde_json::to_value(&manifest)?;
        let digest = value_digest(BUNDLE_TAG, &manifest_value);
        let history_included = document.history.included;
        let bundle = json!({
            "format": FORMAT,
            "format_version": FORMAT_VERSION,
            "manifest": manifest_value,
            "document": document_value,
            "entries": entries.iter().map(|(n, (_, b))| (n.clone(), Value::String(base64_encode(b)))).collect::<serde_json::Map<_, _>>(),
        });
        let bytes = to_canonical_string(&bundle).into_bytes();
        if bytes.len() > MAX_BUNDLE_BYTES {
            return Err(LabError::InvalidState {
                id,
                reason: format!(
                    "the bundle would be {} bytes, more than {MAX_BUNDLE_BYTES}",
                    bytes.len()
                ),
            });
        }
        Ok(ExportedBundle {
            bytes,
            digest,
            history_included,
        })
    }

    /// Validates and imports a bundle. Entry bytes are stored as
    /// content-addressed artifacts (existing ones are never replaced by
    /// different content: the address is the content's digest); the
    /// manifest and document are stored in the registry's
    /// `imported_bundles` table. Local experiments are never created or
    /// changed. Importing the same bundle twice changes nothing.
    ///
    /// # Errors
    /// [`LabError::InvalidBundle`] with the report, or storage errors.
    pub fn import_bundle(&self, bytes: &[u8]) -> Result<ImportOutcome> {
        self.import_bundle_with(bytes, None)
    }

    /// [`import_bundle`](Self::import_bundle), classifying signatures
    /// against `trust` (recorded in the stored validation report).
    ///
    /// # Errors
    /// As [`import_bundle`](Self::import_bundle).
    pub fn import_bundle_with(
        &self,
        bytes: &[u8],
        trust: Option<&crate::signing::TrustStore>,
    ) -> Result<ImportOutcome> {
        let (report, valid) = validate_with(bytes, trust);
        let Some(valid) = valid else {
            return Err(LabError::InvalidBundle(Box::new(report)));
        };
        for bytes in valid.entries.values() {
            self.artifacts().put(bytes)?;
        }
        let e = &valid.document.experiment;
        let document = json!({
            "manifest": valid.manifest,
            "document": valid.document,
            "signatures": valid.signatures,
        });
        let newly_imported = self.registry().insert_imported_bundle(
            valid.digest,
            FORMAT_VERSION,
            e.id,
            &e.kind,
            e.status.as_str(),
            e.outcome.as_deref(),
            &document,
            &serde_json::to_value(&report)?,
        )?;
        let local_experiment = match self.experiment(e.id) {
            Ok(local) => json!({
                "exists": true,
                "same_spec_digest": local.spec_digest == e.spec_digest,
                "same_result_digest": local.result_digest == e.result_digest,
            }),
            Err(LabError::ExperimentNotFound(_)) => json!({ "exists": false }),
            Err(other) => return Err(other),
        };
        Ok(ImportOutcome {
            bundle: self.registry().imported_bundle(valid.digest)?,
            newly_imported,
            local_experiment,
        })
    }

    /// Rebuilds an imported bundle from stored data and validates it again
    /// (stored entries are re-read and re-verified).
    fn stored_bundle(&self, digest: Digest) -> Result<(ImportedBundle, ValidBundle)> {
        let imported = self.registry().imported_bundle(digest)?;
        let manifest: Manifest = serde_json::from_value(imported.document["manifest"].clone())
            .map_err(|e| LabError::CorruptRecord {
                record: format!("bundle {digest}"),
                reason: e.to_string(),
            })?;
        let mut entries = serde_json::Map::new();
        for info in &manifest.entries {
            if self.artifacts().check(&info.sha256, info.size)? != ArtifactState::Intact {
                return Err(LabError::ArtifactCorrupted(info.sha256));
            }
            let bytes = self.artifacts().read(&info.sha256)?;
            entries.insert(info.name.clone(), Value::String(base64_encode(&bytes)));
        }
        let bundle = json!({
            "format": FORMAT,
            "format_version": imported.format_version,
            "manifest": imported.document["manifest"],
            "document": imported.document["document"],
            "entries": entries,
            "signatures": imported.document.get("signatures").cloned().unwrap_or_else(|| json!([])),
        });
        let (report, valid) = validate(to_canonical_string(&bundle).as_bytes());
        let valid = valid.ok_or_else(|| LabError::InvalidBundle(Box::new(report)))?;
        Ok((imported, valid))
    }

    /// Attempts to reproduce an imported bundle and appends the attempt to
    /// it (the bundle itself never changes):
    ///
    /// - a differential replay is re-executed over the bundle's verified
    ///   events with this build's implementations, exactly as
    ///   [`Lab::reproduce`] does over a store;
    /// - a performance experiment is measured again here and compared (see
    ///   [`Lab::reproduce_performance`]);
    /// - a reliability experiment is `unsupported` here;
    /// - a failed experiment is `not_reproducible`.
    ///
    /// # Errors
    /// [`LabError::BundleNotFound`], or storage errors.
    pub fn reproduce_bundle(
        &self,
        digest: Digest,
        policy: &RegressionPolicy,
    ) -> Result<BundleVerification> {
        let (_, valid) = self.stored_bundle(digest)?;
        let record = valid.document.experiment.to_record();
        let (status, detail) = match (
            record.kind.as_str(),
            record.status,
            record.input.as_ref(),
            record.result.as_ref(),
        ) {
            (_, Status::Failed, _, _) => (
                ReproductionStatus::NotReproducible,
                json!({ "reason": "the experiment failed; there is no result to reproduce", "failure": record.failure }),
            ),
            (spec::DIFFERENTIAL_REPLAY, _, Some(input), Some(result)) => match &valid.history {
                None => (
                    ReproductionStatus::HistoryUnavailable,
                    json!({ "reason": format!("the bundle does not include the input history ({})", valid.document.history.reason.as_deref().unwrap_or("no reason given")) }),
                ),
                Some(history) => {
                    let (status, mut detail) = self.differential_reproduction(
                        &record,
                        input,
                        result,
                        history,
                        |spec| history.resolve(spec),
                        false,
                    )?;
                    detail["source"] = json!("the bundle's verified input history");
                    (status, detail)
                }
            },
            (performance::PERFORMANCE, _, _, Some(_)) => {
                let recorded = performance::result_of(&record)?;
                let spec = performance::PerformanceSpec::from_json(record.spec.clone())
                    .and_then(|s| s.validated().map(|(v, _)| v));
                match spec {
                    Err(e) => (
                        ReproductionStatus::ConfigurationMismatch,
                        json!({ "reason": e.to_string() }),
                    ),
                    Ok(spec) => {
                        let attempt = self.measure_performance(&spec)?;
                        match performance::result_of(&attempt) {
                            Err(_) => (
                                ReproductionStatus::NotReproducible,
                                json!({ "reason": "the new measurement did not complete", "attempt_experiment_id": attempt.id }),
                            ),
                            Ok(new) => {
                                let comparison =
                                    performance::compare::compare(&recorded, &new, policy);
                                let status = performance::reproduction_status(comparison.verdict);
                                (
                                    status,
                                    json!({ "compared": "a new local measurement of the bundled specification", "attempt_experiment_id": attempt.id, "comparison": comparison }),
                                )
                            }
                        }
                    }
                }
            }
            (reliability::RELIABILITY, _, _, _) => (
                ReproductionStatus::Unsupported,
                json!({ "reason": "reliability scenarios inject faults and kill processes; they are re-run only with the orbit-reliability tool (run the same scenario and version, then compare)" }),
            ),
            _ => (
                ReproductionStatus::Unsupported,
                json!({ "reason": "this bundle has no result this platform can reproduce" }),
            ),
        };
        self.registry()
            .add_bundle_verification(digest, status.as_str(), &detail)
    }
}

/// Adds (or replaces) the signature of `key` on a valid bundle and returns
/// the new bundle bytes (canonical). The bundle digest is unchanged:
/// signatures are not covered by it.
///
/// # Errors
/// [`LabError::InvalidBundle`] if the bundle is not valid, or a malformed key.
pub fn sign(bytes: &[u8], key: &crate::signing::SigningKeyFile) -> Result<Vec<u8>> {
    let (report, valid) = validate(bytes);
    let Some(valid) = valid else {
        return Err(LabError::InvalidBundle(Box::new(report)));
    };
    let mut value: Value = serde_json::from_slice(bytes)?;
    let signature = key.sign(&valid.digest)?;
    let mut signatures: Vec<crate::signing::BundleSignature> = valid
        .signatures
        .into_iter()
        .filter(|s| s.key_id != signature.key_id)
        .collect();
    signatures.push(signature);
    value["signatures"] = serde_json::to_value(&signatures)?;
    Ok(to_canonical_string(&value).into_bytes())
}

fn not_included(reason: String) -> HistoryInfo {
    HistoryInfo {
        included: false,
        entry: None,
        events: None,
        reason: Some(reason),
    }
}

/// Events `1..=to` as canonical JSON, if the store still holds exactly the
/// pinned history and they fit in `budget` bytes.
fn history_events(
    store: &EventStore,
    input: &InputRange,
    budget: usize,
) -> std::result::Result<Vec<u8>, String> {
    let stream = StreamId::new(input.stream.as_str()).map_err(|e| e.to_string())?;
    let mut events = Vec::new();
    let mut size = 0usize;
    let mut after = 0u64;
    while after < input.to {
        let page = store
            .read_after(
                &stream,
                Sequence::new(after).map_err(|e| e.to_string())?,
                1_000,
            )
            .map_err(|e| format!("the stream could not be read: {e}"))?;
        if page.is_empty() {
            return Err(format!(
                "the stream now ends at {after}, before the pinned {}",
                input.to
            ));
        }
        for event in page {
            if after == input.to {
                break;
            }
            size += serde_json::to_vec(&event).map_err(|e| e.to_string())?.len();
            if size > budget {
                return Err(format!(
                    "the {} input events would exceed the bundle size limit",
                    input.to
                ));
            }
            after = event.sequence.get();
            events.push(event);
        }
    }
    // Same pinning rule as reproduction: the chain must end at the recorded digest.
    let history = MemoryHistory::verified(&input.stream, events.clone())?;
    let last = events.last().map_or(Digest::GENESIS, |e| e.digest);
    if last != input.to_event_digest || history.len() != input.to {
        return Err("the stream no longer holds the pinned history".to_owned());
    }
    Ok(
        to_canonical_string(&serde_json::to_value(&events).map_err(|e| e.to_string())?)
            .into_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc_4648_vectors_and_is_strict() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64_encode(plain.as_bytes()), encoded);
            assert_eq!(base64_decode(encoded).unwrap(), plain.as_bytes());
        }
        for bad in [
            "Zg=", "Zg", "Zh==", "Z===", "Zm9v\n", "Zg==Zg==", "Zm9-", " Zm9v",
        ] {
            assert!(base64_decode(bad).is_err(), "{bad:?}");
        }
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(base64_decode(&base64_encode(&all)).unwrap(), all);
    }

    #[test]
    fn entry_names_are_strict() {
        for ok in [
            "artifacts/left_trace",
            "history/events.json",
            "a",
            "x-1/y.z",
        ] {
            assert!(valid_entry_name(ok), "{ok}");
        }
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "a/../b",
            "a//b",
            "a/",
            ".hidden",
            "A",
            "a\\b",
            "a b",
            "a\u{0}b",
            "é",
            &"a".repeat(MAX_NAME_LEN + 1),
        ] {
            assert!(!valid_entry_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn nesting_is_measured_outside_strings() {
        assert!(!depth_exceeds(br#"{"a":"[[[[[[[["}"#, 2));
        assert!(depth_exceeds(b"[[[1]]]", 2));
        assert!(!depth_exceeds(br#"["\"[[[["]"#, 1));
    }

    #[test]
    fn garbage_is_rejected_with_a_code() {
        let code = |bytes: &[u8]| validate(bytes).0.errors[0].code;
        assert_eq!(
            code(b"{\"format\": \"orbit-evidence-bundle\", \"format_ver"),
            "truncated"
        );
        assert_eq!(code(b"not json"), "malformed_json");
        assert_eq!(code(b"{\"format\": \"zip\"}"), "not_a_bundle");
        assert_eq!(
            code(br#"{"format":"orbit-evidence-bundle","format_version":2}"#),
            "unsupported_version"
        );
        assert_eq!(
            code(br#"{"format":"orbit-evidence-bundle","format_version":1}"#),
            "invalid_structure"
        );
        let deep = format!("{}{}", "[".repeat(MAX_DEPTH + 1), "]".repeat(MAX_DEPTH + 1));
        assert_eq!(code(deep.as_bytes()), "too_deeply_nested");
        assert_eq!(code(&vec![b' '; MAX_BUNDLE_BYTES + 1]), "too_large");
    }
}
