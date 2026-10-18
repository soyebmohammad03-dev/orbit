//! The outcome of a reproduction attempt, shared by every experiment kind
//! and by imported evidence bundles.
//!
//! A reproduction attempt never changes the original record: it is appended
//! as a verification (`kind = reproduction`) whose `status` is one of these
//! labels and whose `detail` names exactly what matched, what differed, or
//! which dependency was missing.
//!
//! A status describes the *evidence*, not the correctness of the code:
//! `reproduced` means the same result was obtained again from the same
//! inputs, configuration and implementation identities. It does not mean
//! the result is right, and an identical executable digest does not mean
//! two implementations are semantically equivalent.

use serde::{Deserialize, Serialize};

/// Outcome of a reproduction attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReproductionStatus {
    /// The same result was obtained again. For performance experiments: a
    /// new measurement whose difference from the original is below the
    /// threshold (see the performance module).
    Reproduced,
    /// A different result was obtained from the same inputs; the detail
    /// lists what differs.
    Differs,
    /// The input events differ from the ones recorded (the history changed).
    HistoryChanged,
    /// The configuration cannot be the one recorded: the stored spec no
    /// longer matches its digest, cannot be read by this version, or was
    /// recorded under other rules, scenario versions or an incompatible
    /// environment.
    ConfigurationMismatch,
    /// A required implementation is not registered, or its fingerprint
    /// changed.
    ImplementationUnavailable,
    /// A recorded artifact is missing or damaged, so the original evidence
    /// cannot be compared completely.
    ArtifactMissing,
    /// The input history is missing, too short or fails verification (or,
    /// for a bundle, was not included).
    HistoryUnavailable,
    /// The original result was itself classified not reproducible, or the
    /// re-execution was nondeterministic or could not be carried out.
    NotReproducible,
    /// The attempt ran but its evidence does not support either conclusion
    /// (for example a performance change above the threshold that is not
    /// statistically supported, or too few samples).
    Inconclusive,
    /// This platform cannot reproduce this kind of evidence here (for
    /// example a reliability bundle, which needs the `orbit-reliability`
    /// tool).
    Unsupported,
}

impl ReproductionStatus {
    /// Every status.
    pub const ALL: [Self; 10] = [
        Self::Reproduced,
        Self::Differs,
        Self::HistoryChanged,
        Self::ConfigurationMismatch,
        Self::ImplementationUnavailable,
        Self::ArtifactMissing,
        Self::HistoryUnavailable,
        Self::NotReproducible,
        Self::Inconclusive,
        Self::Unsupported,
    ];

    /// Stored label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reproduced => "reproduced",
            Self::Differs => "differs",
            Self::HistoryChanged => "history_changed",
            Self::ConfigurationMismatch => "configuration_mismatch",
            Self::ImplementationUnavailable => "implementation_unavailable",
            Self::ArtifactMissing => "artifact_missing",
            Self::HistoryUnavailable => "history_unavailable",
            Self::NotReproducible => "not_reproducible",
            Self::Inconclusive => "inconclusive",
            Self::Unsupported => "unsupported",
        }
    }

    /// Parses a stored label.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == text)
    }
}

impl std::fmt::Display for ReproductionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_round_trip_and_match_serde() {
        for status in ReproductionStatus::ALL {
            assert_eq!(ReproductionStatus::parse(status.as_str()), Some(status));
            assert_eq!(
                serde_json::to_value(status).unwrap(),
                serde_json::json!(status.as_str())
            );
        }
        assert_eq!(ReproductionStatus::parse("ok"), None);
    }
}
