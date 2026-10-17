//! Snapshots: listing with validation, creation, inspection.

use axum::extract::State;
use axum::http::StatusCode;
use orbit_core::reconstruct::SnapshotStatus;
use orbit_core::snapshot::SnapshotCheck;
use orbit_core::{EventStore, ReconstructOptions, Snapshot, SnapshotId, Target};

use super::{blocking, engine, sequence, stream_id};
use crate::AppState;
use crate::dto::{CreateSnapshotRequest, CreateSnapshotResponse, SnapshotDto, SnapshotValidation};
use crate::engines::Engines;
use crate::error::{ApiError, ErrorResponse};
use crate::extract::{ApiJson, ApiPath};

/// Validates a snapshot with its own projection when it is registered,
/// otherwise with storage-level checks only, and builds the DTO.
fn validated(
    store: &EventStore,
    engines: &Engines,
    snapshot: Snapshot,
) -> Result<SnapshotDto, ApiError> {
    let (status, checked_by, problem) = match engines.for_key(&snapshot.projection) {
        Some(engine) => match engine.check_snapshot(store, snapshot.id)? {
            SnapshotStatus::Usable { .. } => ("usable", "projection", None),
            SnapshotStatus::Unusable { problem, .. } => ("unusable", "projection", Some(problem)),
        },
        None => match store.check_snapshot(snapshot.id)? {
            SnapshotCheck::Intact { .. } => ("usable", "storage", None),
            SnapshotCheck::Unusable { problem, .. } => ("unusable", "storage", Some(problem)),
        },
    };
    Ok(SnapshotDto {
        snapshot_id: snapshot.id.to_string(),
        stream_id: snapshot.stream_id.to_string(),
        sequence: snapshot.sequence.get(),
        projection: snapshot.projection.to_string(),
        event_digest: snapshot.event_digest.to_hex(),
        state_digest: snapshot.state_digest.to_hex(),
        created_at: snapshot.created_at.to_rfc3339(),
        state: snapshot.state,
        validation: SnapshotValidation {
            status: status.into(),
            checked_by: checked_by.into(),
            problem: problem.map(|p| serde_json::to_value(p).unwrap_or_default()),
        },
    })
}

/// Every snapshot of a stream, each validated for this response.
#[utoipa::path(get, path = "/api/v1/streams/{stream_id}/snapshots", tag = "snapshots",
    params(("stream_id" = String, Path, description = "Stream id")),
    responses((status = 200, body = [SnapshotDto]), (status = 404, body = ErrorResponse),
              (status = 500, description = "A snapshot row is malformed (integrity_violation)", body = ErrorResponse)))]
pub async fn list_snapshots(
    State(state): State<AppState>,
    ApiPath(raw): ApiPath<String>,
) -> Result<ApiJson<Vec<SnapshotDto>>, ApiError> {
    let id = stream_id(&raw)?;
    let engines = state.engines.clone();
    blocking(&state, move |store| {
        let snapshots = store.list_snapshots(&id)?;
        let dtos = snapshots
            .into_iter()
            .map(|s| validated(store, &engines, s))
            .collect::<Result<_, _>>()?;
        Ok(ApiJson(dtos))
    })
    .await
}

/// Reconstructs the state at `at` (default: head) and stores it as a snapshot.
#[utoipa::path(post, path = "/api/v1/streams/{stream_id}/snapshots", tag = "snapshots",
    params(("stream_id" = String, Path, description = "Stream id")),
    request_body = CreateSnapshotRequest,
    responses((status = 201, description = "Created", body = CreateSnapshotResponse),
              (status = 200, description = "An identical snapshot already existed", body = CreateSnapshotResponse),
              (status = 404, body = ErrorResponse), (status = 409, body = ErrorResponse), (status = 422, body = ErrorResponse)))]
pub async fn create_snapshot(
    State(state): State<AppState>,
    ApiPath(raw): ApiPath<String>,
    ApiJson(body): ApiJson<CreateSnapshotRequest>,
) -> Result<(StatusCode, ApiJson<CreateSnapshotResponse>), ApiError> {
    let id = stream_id(&raw)?;
    let engine = engine(&state, body.projection.as_deref())?;
    let target = match body.at {
        Some(at) => Target::At(sequence("at", at)?),
        None => Target::Latest,
    };
    let engines = state.engines.clone();
    blocking(&state, move |store| {
        let created = engine.create_snapshot(store, &id, target, ReconstructOptions::default())?;
        let status = if created.saved.created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        };
        Ok((
            status,
            ApiJson(CreateSnapshotResponse {
                created: created.saved.created,
                snapshot: validated(store, &engines, created.saved.snapshot)?,
                report: serde_json::to_value(&created.report)
                    .map_err(|e| ApiError::internal(e.to_string()))?,
            }),
        ))
    })
    .await
}

/// One snapshot with its validation result.
#[utoipa::path(get, path = "/api/v1/snapshots/{snapshot_id}", tag = "snapshots",
    params(("snapshot_id" = String, Path, description = "Snapshot id (UUID)")),
    responses((status = 200, body = SnapshotDto), (status = 400, body = ErrorResponse), (status = 404, body = ErrorResponse)))]
pub async fn get_snapshot(
    State(state): State<AppState>,
    ApiPath(raw): ApiPath<String>,
) -> Result<ApiJson<SnapshotDto>, ApiError> {
    let id = uuid::Uuid::parse_str(&raw)
        .ok()
        .and_then(|u| SnapshotId::from_uuid(u).ok())
        .ok_or_else(|| {
            ApiError::bad_request("invalid_snapshot_id", "snapshot id must be a non-nil UUID")
        })?;
    let engines = state.engines.clone();
    blocking(&state, move |store| {
        let snapshot = store.snapshot(id)?;
        Ok(ApiJson(validated(store, &engines, snapshot)?))
    })
    .await
}
