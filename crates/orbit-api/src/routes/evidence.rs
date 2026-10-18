//! Portable evidence bundles: export, validation, import and reproduction.
//!
//! Bundle bodies are read as raw bytes (at most
//! [`orbit_lab::bundle::MAX_BUNDLE_BYTES`], a limit set on these routes
//! only) and handed to `orbit_lab::bundle::validate`, which checks them
//! before anything is trusted. Nothing in a bundle is executed, and entry
//! names are never used as file paths.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use orbit_core::Digest;
use orbit_lab::Lab;
use orbit_lab::performance::RegressionPolicy;
use serde_json::{Value, json};

use super::lab::{in_background, lab_handle};
use super::page_limit;
use crate::AppState;
use crate::dto::{BundlePageQuery, ExportQuery};
use crate::error::{ApiError, ErrorResponse};
use crate::extract::{ApiJson, ApiPath, ApiQuery};

fn digest(raw: &str) -> Result<Digest, ApiError> {
    Digest::from_hex(raw).map_err(|_| {
        ApiError::bad_request(
            "invalid_bundle_digest",
            "a bundle digest is 64 lowercase hexadecimal characters",
        )
    })
}

fn json<T: serde::Serialize>(value: &T) -> Result<Value, ApiError> {
    serde_json::to_value(value).map_err(|e| ApiError::internal(e.to_string()))
}

async fn with_lab<T, F>(state: &AppState, f: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce(&Lab, &orbit_core::EventStore) -> Result<T, ApiError> + Send + 'static,
{
    let lab = lab_handle(state)?;
    let store = Arc::clone(&state.store);
    tokio::task::spawn_blocking(move || f(&lab, &store))
        .await
        .map_err(|e| ApiError::internal(format!("lab task failed: {e}")))?
}

/// Downloads a finished experiment as an evidence bundle (canonical JSON).
/// A differential replay's input events are included when the server's
/// event store still holds exactly the pinned history (and `history` is not
/// `false`); otherwise the bundle records why not. The bundle digest is in
/// the `X-Orbit-Bundle-Digest` header.
#[utoipa::path(get, path = "/api/v1/lab/experiments/{experiment_id}/bundle", tag = "lab",
    params(("experiment_id" = String, Path, description = "Experiment id (UUID)"), ExportQuery),
    responses((status = 200, description = "The bundle (application/json attachment)", body = Object),
              (status = 404, body = ErrorResponse),
              (status = 409, description = "The experiment is not finished, or the bundle would be too large", body = ErrorResponse),
              (status = 500, description = "An artifact is missing or damaged; damaged evidence is never exported", body = ErrorResponse)))]
pub async fn export_bundle(
    State(state): State<AppState>,
    ApiPath(raw): ApiPath<String>,
    ApiQuery(query): ApiQuery<ExportQuery>,
) -> Result<Response, ApiError> {
    let id: orbit_lab::ExperimentId = raw.parse().map_err(|_| {
        ApiError::bad_request(
            "invalid_experiment_id",
            format!("{raw:?} is not an experiment id (UUID)"),
        )
    })?;
    let with_history = query.history.unwrap_or(true);
    let bundle = with_lab(&state, move |lab, store| {
        Ok(lab.export_bundle(id, with_history.then_some(store))?)
    })
    .await?;
    let mut response = (StatusCode::OK, bundle.bytes).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    if let Ok(v) = HeaderValue::from_str(&format!(
        "attachment; filename=\"orbit-evidence-{id}.json\""
    )) {
        headers.insert(header::CONTENT_DISPOSITION, v);
    }
    if let Ok(v) = HeaderValue::from_str(&bundle.digest.to_hex()) {
        headers.insert("x-orbit-bundle-digest", v);
    }
    Ok(response)
}

/// Validates a bundle (the raw request body) without storing anything.
/// Always 200: the report's `valid` says whether it passed, and `errors`
/// lists each problem with a stable code.
#[utoipa::path(post, path = "/api/v1/lab/bundles/validate", tag = "lab",
    request_body(content = Object, description = "The bundle file", content_type = "application/json"),
    responses((status = 200, description = "Validation report", body = Object),
              (status = 413, description = "Larger than the bundle size limit", body = ErrorResponse)))]
pub async fn validate_bundle(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<ApiJson<Value>, ApiError> {
    lab_handle(&state)?;
    let trust = state.trusted_keys.clone();
    let report = tokio::task::spawn_blocking(move || {
        orbit_lab::bundle::validate_with(&body, trust.as_deref()).0
    })
    .await
    .map_err(|e| ApiError::internal(format!("validation task failed: {e}")))?;
    Ok(ApiJson(json(&report)?))
}

/// Validates and imports a bundle. 201 when newly imported, 200 when the
/// same bundle was already imported (nothing changes), 422
/// `invalid_bundle` with the validation report in `details` otherwise.
/// An import never creates or changes a local experiment.
#[utoipa::path(post, path = "/api/v1/lab/bundles", tag = "lab",
    request_body(content = Object, description = "The bundle file", content_type = "application/json"),
    responses((status = 201, description = "Imported", body = Object),
              (status = 200, description = "Already imported", body = Object),
              (status = 413, body = ErrorResponse),
              (status = 422, description = "Invalid bundle; nothing was imported", body = ErrorResponse)))]
pub async fn import_bundle(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<(StatusCode, ApiJson<Value>), ApiError> {
    let trust = state.trusted_keys.clone();
    with_lab(&state, move |lab, _| {
        let outcome = lab.import_bundle_with(&body, trust.as_deref())?;
        let status = if outcome.newly_imported {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        };
        Ok((status, ApiJson(json(&outcome)?)))
    })
    .await
}

/// Imported bundles, newest import first, with cursor pagination.
#[utoipa::path(get, path = "/api/v1/lab/bundles", tag = "lab", params(BundlePageQuery),
    responses((status = 200, description = "A page of imported bundles", body = Object),
              (status = 400, body = ErrorResponse)))]
pub async fn list_bundles(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<BundlePageQuery>,
) -> Result<ApiJson<Value>, ApiError> {
    let limit = page_limit(query.limit)?;
    with_lab(&state, move |lab, _| {
        let mut rows = lab.registry().imported_bundles(limit + 1, query.before)?;
        let more = rows.len() > limit;
        rows.truncate(limit);
        let next = if more {
            rows.last().map(|r| r.sequence)
        } else {
            None
        };
        Ok(ApiJson(json!({ "bundles": rows, "next_before": next })))
    })
    .await
}

/// One imported bundle: its manifest and document (without entry
/// contents), the validation report from its import, and its reproduction
/// attempts.
#[utoipa::path(get, path = "/api/v1/lab/bundles/{bundle_digest}", tag = "lab",
    params(("bundle_digest" = String, Path, description = "Bundle digest (hex)")),
    responses((status = 200, description = "The bundle", body = Object),
              (status = 400, body = ErrorResponse), (status = 404, body = ErrorResponse)))]
pub async fn get_bundle(
    State(state): State<AppState>,
    ApiPath(raw): ApiPath<String>,
) -> Result<ApiJson<Value>, ApiError> {
    let digest = digest(&raw)?;
    with_lab(&state, move |lab, _| {
        let bundle = lab.registry().imported_bundle(digest)?;
        let verifications = lab.registry().bundle_verifications(digest)?;
        Ok(ApiJson(
            json!({ "bundle": bundle, "verifications": verifications }),
        ))
    })
    .await
}

/// Reproduces an imported bundle and appends the attempt to it.
///
/// - Differential replay with its history: re-executed over the bundle's
///   verified events with this server's implementations; 201 with the
///   attempt (`reproduced`, `differs`, `implementation_unavailable`, …).
/// - Performance: measured again here in the background; 202, and the
///   attempt appears in the bundle's verifications when it finishes.
/// - Reliability: 201 with status `unsupported`; failed experiments:
///   `not_reproducible`; without history: `history_unavailable`.
#[utoipa::path(post, path = "/api/v1/lab/bundles/{bundle_digest}/reproduce", tag = "lab",
    params(("bundle_digest" = String, Path, description = "Bundle digest (hex)")),
    responses((status = 201, description = "The stored attempt", body = Object),
              (status = 202, description = "A performance measurement was started", body = Object),
              (status = 404, body = ErrorResponse),
              (status = 409, description = "A measurement is already running", body = ErrorResponse)))]
pub async fn reproduce_bundle(
    State(state): State<AppState>,
    ApiPath(raw): ApiPath<String>,
) -> Result<(StatusCode, ApiJson<Value>), ApiError> {
    let digest = digest(&raw)?;
    let kind = with_lab(&state, move |lab, _| {
        Ok(lab.registry().imported_bundle(digest)?.kind)
    })
    .await?;
    if kind == orbit_lab::performance::PERFORMANCE {
        let value = in_background(
            &state,
            move |_| Ok((json!({ "status": "measuring", "bundle_digest": digest }), None)),
            move |lab, _| {
                if let Err(e) = lab.reproduce_bundle(digest, &RegressionPolicy::default()) {
                    tracing::error!(%digest, error = %e, "bundle reproduction could not be recorded");
                }
            },
        )
        .await?;
        return Ok((StatusCode::ACCEPTED, ApiJson(value)));
    }
    with_lab(&state, move |lab, _| {
        let attempt = lab.reproduce_bundle(digest, &RegressionPolicy::default())?;
        Ok((StatusCode::CREATED, ApiJson(json(&attempt)?)))
    })
    .await
}
