//! Reconstruction, diffs and per-event explanations.

use axum::extract::State;
use orbit_core::{ReconstructOptions, Target};
use serde_json::Value;

use super::{blocking, engine, sequence, stream_id};
use crate::AppState;
use crate::dto::{DiffQuery, ExplainQuery, MAX_EXPLAIN_EVENTS, StateQuery, StateResponse};
use crate::error::{ApiError, ErrorResponse};
use crate::extract::{ApiJson, ApiPath, ApiQuery};

fn to_value<T: serde::Serialize>(value: &T) -> Result<Value, ApiError> {
    serde_json::to_value(value).map_err(|e| ApiError::internal(e.to_string()))
}

/// Reconstructs the state at the head or at `at`, using the engine.
#[utoipa::path(get, path = "/api/v1/streams/{stream_id}/state", tag = "time-travel",
    params(("stream_id" = String, Path, description = "Stream id"), StateQuery),
    responses((status = 200, body = StateResponse), (status = 400, body = ErrorResponse), (status = 404, body = ErrorResponse),
              (status = 422, description = "target_beyond_head, projection_failed, snapshot_unusable", body = ErrorResponse)))]
pub async fn get_state(
    State(state): State<AppState>,
    ApiPath(raw): ApiPath<String>,
    ApiQuery(query): ApiQuery<StateQuery>,
) -> Result<ApiJson<StateResponse>, ApiError> {
    let id = stream_id(&raw)?;
    let engine = engine(&state, query.projection.as_deref())?;
    let target = match query.at {
        Some(at) => Target::At(sequence("at", at)?),
        None => Target::Latest,
    };
    let options = ReconstructOptions::with_snapshots(query.snapshots.unwrap_or_default().into())
        .recording_events(query.record_events.unwrap_or(false));
    blocking(&state, move |store| {
        let (value, report) = engine.reconstruct(store, &id, target, options)?;
        Ok(ApiJson(StateResponse {
            projection: engine.key().to_string(),
            state: value,
            report: to_value(&report)?,
        }))
    })
    .await
}

/// Structural diff between the states at `from` and `to` (default: head).
/// Returns the engine's `StateDiff`.
#[utoipa::path(get, path = "/api/v1/streams/{stream_id}/diff", tag = "time-travel",
    params(("stream_id" = String, Path, description = "Stream id"), DiffQuery),
    responses((status = 200, description = "StateDiff: changes, events in the interval, base report", body = Object),
              (status = 400, body = ErrorResponse), (status = 404, body = ErrorResponse), (status = 422, body = ErrorResponse)))]
pub async fn get_diff(
    State(state): State<AppState>,
    ApiPath(raw): ApiPath<String>,
    ApiQuery(query): ApiQuery<DiffQuery>,
) -> Result<ApiJson<Value>, ApiError> {
    let id = stream_id(&raw)?;
    let engine = engine(&state, query.projection.as_deref())?;
    let from = sequence("from", query.from)?;
    let to = query.to.map(|t| sequence("to", t)).transpose()?;
    let options = ReconstructOptions::with_snapshots(query.snapshots.unwrap_or_default().into());
    blocking(&state, move |store| {
        let to = match to {
            Some(to) => to,
            None => store.head(&id)?.sequence,
        };
        let diff = engine.diff(store, &id, from, to, options)?;
        Ok(ApiJson(to_value(&diff)?))
    })
    .await
}

/// Per-event changes in `(from, to]` (at most 1000 events). Returns the engine's `Explanation`.
#[utoipa::path(get, path = "/api/v1/streams/{stream_id}/explain", tag = "time-travel",
    params(("stream_id" = String, Path, description = "Stream id"), ExplainQuery),
    responses((status = 200, description = "Explanation: one transition per event", body = Object),
              (status = 400, body = ErrorResponse), (status = 404, body = ErrorResponse), (status = 422, body = ErrorResponse)))]
pub async fn get_explain(
    State(state): State<AppState>,
    ApiPath(raw): ApiPath<String>,
    ApiQuery(query): ApiQuery<ExplainQuery>,
) -> Result<ApiJson<Value>, ApiError> {
    let id = stream_id(&raw)?;
    let engine = engine(&state, query.projection.as_deref())?;
    let from = sequence("from", query.from)?;
    let to = query.to.map(|t| sequence("to", t)).transpose()?;
    blocking(&state, move |store| {
        let to = match to {
            Some(to) => to,
            None => store.head(&id)?.sequence,
        };
        if to.get().saturating_sub(from.get()) > MAX_EXPLAIN_EVENTS {
            return Err(ApiError::bad_request(
                "interval_too_large",
                format!("explain covers at most {MAX_EXPLAIN_EVENTS} events; narrow from/to"),
            ));
        }
        let explanation = engine.explain(store, &id, from, to, ReconstructOptions::default())?;
        Ok(ApiJson(to_value(&explanation)?))
    })
    .await
}
