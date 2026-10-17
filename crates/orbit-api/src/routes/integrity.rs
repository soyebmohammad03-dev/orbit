//! Integrity verification and checkpoints.

use axum::extract::State;
use orbit_core::{Digest, StreamCheckpoint};
use serde_json::Value;

use super::{blocking, sequence, stream_id};
use crate::AppState;
use crate::dto::{CheckpointDto, VerifyStreamRequest};
use crate::error::{ApiError, ErrorResponse};
use crate::extract::{ApiJson, ApiPath};

/// The stream's current head as a checkpoint to keep outside the database.
#[utoipa::path(get, path = "/api/v1/streams/{stream_id}/checkpoint", tag = "integrity",
    params(("stream_id" = String, Path, description = "Stream id")),
    responses((status = 200, body = CheckpointDto), (status = 404, description = "Unknown or empty stream", body = ErrorResponse)))]
pub async fn checkpoint(
    State(state): State<AppState>,
    ApiPath(raw): ApiPath<String>,
) -> Result<ApiJson<CheckpointDto>, ApiError> {
    let id = stream_id(&raw)?;
    blocking(&state, move |store| {
        let c = store.checkpoint(&id)?;
        Ok(ApiJson(CheckpointDto {
            sequence: c.sequence.get(),
            digest: c.digest.to_hex(),
        }))
    })
    .await
}

/// Verifies a stream's event chain and head, and any given checkpoints.
/// Integrity defects are reported in the body with status 200; the request
/// fails only if verification could not run.
#[utoipa::path(post, path = "/api/v1/streams/{stream_id}/verify", tag = "integrity",
    params(("stream_id" = String, Path, description = "Stream id")),
    request_body = VerifyStreamRequest,
    responses((status = 200, description = "IntegrityReport (check `total_issues`)", body = Object),
              (status = 400, body = ErrorResponse), (status = 404, body = ErrorResponse)))]
pub async fn verify_stream(
    State(state): State<AppState>,
    ApiPath(raw): ApiPath<String>,
    ApiJson(body): ApiJson<VerifyStreamRequest>,
) -> Result<ApiJson<Value>, ApiError> {
    let id = stream_id(&raw)?;
    let checkpoints = body
        .checkpoints
        .iter()
        .map(|c| {
            Ok(StreamCheckpoint {
                sequence: sequence("checkpoint.sequence", c.sequence)?,
                digest: Digest::from_hex(&c.digest)?,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    blocking(&state, move |store| {
        let report = store.verify_stream_against(&id, &checkpoints)?;
        Ok(ApiJson(
            serde_json::to_value(&report).map_err(|e| ApiError::internal(e.to_string()))?,
        ))
    })
    .await
}

/// Verifies the whole database: SQLite integrity and foreign keys, protective
/// triggers, every stream, snapshot and replay run.
#[utoipa::path(post, path = "/api/v1/verify", tag = "integrity",
    responses((status = 200, description = "StoreIntegrityReport (check each section)", body = Object),
              (status = 500, body = ErrorResponse)))]
pub async fn verify_store(State(state): State<AppState>) -> Result<ApiJson<Value>, ApiError> {
    blocking(&state, |store| {
        let report = store.verify_store()?;
        let mut value =
            serde_json::to_value(&report).map_err(|e| ApiError::internal(e.to_string()))?;
        value["valid"] = Value::Bool(report.is_valid());
        Ok(ApiJson(value))
    })
    .await
}
