//! Structured API errors.
//!
//! Every failure is returned as
//! `{"error": {"code": "...", "message": "...", "class": "...", "details": {...}}}`
//! with an HTTP status derived from the engine's error classification.

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use orbit_core::replay::ReplayError;
use orbit_core::{ErrorClass, ReconstructionError, StoreError, ValidationError};
use serde::Serialize;
use serde_json::{Value, json};
use utoipa::ToSchema;

/// Error envelope returned by every endpoint on failure.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorResponse {
    /// The error.
    pub error: ErrorBody,
}

/// Error details.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorBody {
    /// Stable machine-readable code, e.g. `stream_not_found`, `sequence_conflict`.
    pub code: String,
    /// Human-readable message.
    pub message: String,
    /// Engine error class, when the error came from the engine.
    #[schema(value_type = Option<String>)]
    pub class: Option<ErrorClass>,
    /// Structured context (expected/actual sequences, failing event, …).
    #[schema(value_type = Object)]
    pub details: Value,
}

/// An error ready to be turned into an HTTP response.
#[derive(Debug)]
pub struct ApiError {
    /// HTTP status.
    pub status: StatusCode,
    /// Stable code.
    pub code: &'static str,
    /// Message.
    pub message: String,
    /// Engine class.
    pub class: Option<ErrorClass>,
    /// Details.
    pub details: Value,
}

impl ApiError {
    /// A 400 error for a malformed request parameter or body.
    pub fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code,
            message: message.into(),
            class: None,
            details: Value::Null,
        }
    }

    /// A 404 error.
    pub fn not_found(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code,
            message: message.into(),
            class: None,
            details: Value::Null,
        }
    }

    /// A 500 error that should not happen.
    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal_error",
            message: message.into(),
            class: None,
            details: Value::Null,
        }
    }

    /// Attaches structured details.
    #[must_use]
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if self.status.is_server_error() {
            tracing::error!(code = self.code, message = %self.message, "request failed");
        }
        let body = ErrorResponse {
            error: ErrorBody {
                code: self.code.to_owned(),
                message: self.message,
                class: self.class,
                details: self.details,
            },
        };
        (self.status, Json(body)).into_response()
    }
}

impl From<StoreError> for ApiError {
    fn from(error: StoreError) -> Self {
        let class = error.class();
        let (status, code, details) = match &error {
            StoreError::Busy => (StatusCode::SERVICE_UNAVAILABLE, "busy", Value::Null),
            StoreError::InjectedFault(point) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "injected_fault",
                json!({ "point": point }),
            ),
            StoreError::SequenceConflict {
                stream,
                expected,
                actual,
            } => (
                StatusCode::CONFLICT,
                "sequence_conflict",
                json!({ "stream_id": stream, "expected_sequence": expected, "actual_sequence": actual }),
            ),
            StoreError::DuplicateEventId(id) => (
                StatusCode::CONFLICT,
                "duplicate_event_id",
                json!({ "event_id": id }),
            ),
            StoreError::StreamAlreadyExists(id) => (
                StatusCode::CONFLICT,
                "stream_already_exists",
                json!({ "stream_id": id }),
            ),
            StoreError::SnapshotConflict(conflict) => (
                StatusCode::CONFLICT,
                "snapshot_conflict",
                serde_json::to_value(conflict.as_ref()).unwrap_or_default(),
            ),
            StoreError::Validation(v) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                validation_code(v),
                Value::Null,
            ),
            StoreError::Config(_) | StoreError::SequenceExhausted(_) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request",
                Value::Null,
            ),
            StoreError::StreamNotFound(id) => (
                StatusCode::NOT_FOUND,
                "stream_not_found",
                json!({ "stream_id": id }),
            ),
            StoreError::EventNotFound { stream, sequence } => (
                StatusCode::NOT_FOUND,
                "event_not_found",
                json!({ "stream_id": stream, "sequence": sequence }),
            ),
            StoreError::SnapshotNotFound(id) => (
                StatusCode::NOT_FOUND,
                "snapshot_not_found",
                json!({ "snapshot_id": id }),
            ),
            StoreError::ReplayRunNotFound(id) => (
                StatusCode::NOT_FOUND,
                "replay_run_not_found",
                json!({ "run_id": id }),
            ),
            StoreError::SnapshotHistoryMismatch {
                stream,
                sequence,
                reason,
            } => (
                StatusCode::CONFLICT,
                "snapshot_history_mismatch",
                json!({ "stream_id": stream, "sequence": sequence, "reason": reason }),
            ),
            StoreError::CorruptRecord { .. }
            | StoreError::CorruptSnapshot { .. }
            | StoreError::CorruptReplayRecord { .. } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "integrity_violation",
                Value::Null,
            ),
            StoreError::NotAnOrbitDatabase(_) | StoreError::UnsupportedSchemaVersion { .. } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "incompatible_database",
                Value::Null,
            ),
            StoreError::Database(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "database_error",
                Value::Null,
            ),
        };
        Self {
            status,
            code,
            message: error.to_string(),
            class: Some(class),
            details,
        }
    }
}

fn validation_code(error: &ValidationError) -> &'static str {
    match error {
        ValidationError::PayloadTooLarge { .. } | ValidationError::MetadataTooLarge { .. } => {
            "payload_too_large"
        }
        ValidationError::ReplayIntervalTooLarge { .. } => "replay_interval_too_large",
        _ => "validation_failed",
    }
}

impl From<ValidationError> for ApiError {
    fn from(error: ValidationError) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: validation_code(&error),
            message: error.to_string(),
            class: Some(ErrorClass::InvalidRequest),
            details: Value::Null,
        }
    }
}

impl From<ReconstructionError> for ApiError {
    fn from(error: ReconstructionError) -> Self {
        let message = error.to_string();
        match error {
            ReconstructionError::Store(e) => e.into(),
            ReconstructionError::TargetBeyondHead {
                stream,
                target,
                head,
            } => Self {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                code: "target_beyond_head",
                message,
                class: Some(ErrorClass::InvalidRequest),
                details: json!({ "stream_id": stream, "target": target, "head": head }),
            },
            ReconstructionError::InvalidRange { from, to } => {
                Self::bad_request("invalid_range", message)
                    .with_details(json!({ "from": from, "to": to }))
            }
            ReconstructionError::Projection(failure) => Self {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                code: "projection_failed",
                message,
                class: None,
                details: serde_json::to_value(failure.as_ref()).unwrap_or_default(),
            },
            ReconstructionError::HistoryInconsistent {
                stream,
                sequence,
                reason,
            } => Self {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "history_inconsistent",
                message,
                class: Some(ErrorClass::Integrity),
                details: json!({ "stream_id": stream, "sequence": sequence, "reason": reason }),
            },
            ReconstructionError::Snapshot {
                snapshot,
                sequence,
                problem,
            } => Self {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                code: "snapshot_unusable",
                message,
                class: Some(ErrorClass::Integrity),
                details: json!({ "snapshot": snapshot, "sequence": sequence, "problem": problem }),
            },
            ReconstructionError::StateEncoding { .. } => Self::internal(message),
        }
    }
}

impl From<ReplayError> for ApiError {
    fn from(error: ReplayError) -> Self {
        let message = error.to_string();
        match error {
            ReplayError::Store(e) => e.into(),
            ReplayError::Reconstruction(e) => e.into(),
            ReplayError::HistoryIntegrity(report) => Self {
                status: StatusCode::CONFLICT,
                code: "history_integrity",
                message,
                class: Some(ErrorClass::Integrity),
                details: serde_json::to_value(report.as_ref()).unwrap_or_default(),
            },
            ReplayError::InvalidRange { from, to } => Self::bad_request("invalid_range", message)
                .with_details(json!({ "from": from, "to": to })),
        }
    }
}

impl From<JsonRejection> for ApiError {
    fn from(rejection: JsonRejection) -> Self {
        let status = rejection.status();
        let code = if status == StatusCode::PAYLOAD_TOO_LARGE {
            "payload_too_large"
        } else {
            "invalid_body"
        };
        Self {
            status,
            code,
            message: rejection.body_text(),
            class: None,
            details: Value::Null,
        }
    }
}

impl From<QueryRejection> for ApiError {
    fn from(rejection: QueryRejection) -> Self {
        Self::bad_request("invalid_query", rejection.body_text())
    }
}

impl From<PathRejection> for ApiError {
    fn from(rejection: PathRejection) -> Self {
        Self::bad_request("invalid_path", rejection.body_text())
    }
}

impl From<orbit_lab::LabError> for ApiError {
    fn from(error: orbit_lab::LabError) -> Self {
        use orbit_lab::LabError as E;
        let message = error.to_string();
        let (status, code, details) = match error {
            E::Store(e) => return e.into(),
            E::InvalidSpec { field, .. } => (
                StatusCode::BAD_REQUEST,
                "invalid_spec",
                json!({ "field": field }),
            ),
            E::UnknownImplementation(id) => (
                StatusCode::BAD_REQUEST,
                "unknown_implementation",
                json!({ "implementation": id }),
            ),
            E::ExperimentNotFound(id) => (
                StatusCode::NOT_FOUND,
                "experiment_not_found",
                json!({ "experiment_id": id }),
            ),
            E::InvalidTransition { id, from, to } => (
                StatusCode::CONFLICT,
                "invalid_state",
                json!({ "experiment_id": id, "status": from, "requested": to }),
            ),
            E::InvalidState { id, .. } => (
                StatusCode::CONFLICT,
                "invalid_state",
                json!({ "experiment_id": id }),
            ),
            E::ArtifactMissing(_) | E::ArtifactCorrupted(_) | E::CorruptRecord { .. } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "integrity_violation",
                Value::Null,
            ),
            E::DuplicateImplementation(_)
            | E::InvalidDescriptor(_)
            | E::NotALabRegistry(_)
            | E::UnsupportedSchemaVersion { .. }
            | E::Registry(_)
            | E::Io(_)
            | E::Json(_)
            // Cannot happen here: the API never installs a lab fault hook.
            | E::InjectedFault(_) => (StatusCode::INTERNAL_SERVER_ERROR, "lab_error", Value::Null),
        };
        Self {
            status,
            code,
            message,
            class: None,
            details,
        }
    }
}
