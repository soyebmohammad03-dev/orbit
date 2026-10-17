//! Replay Lab: runs, executions and experiments.

use axum::extract::State;
use axum::http::StatusCode;
use orbit_core::replay::{ReplayRun, ReplayRunId, ReplayStart};
use orbit_core::{EventStore, SnapshotPolicy, Target};
use serde_json::Value;

use super::{blocking, engine, sequence, stream_id};
use crate::AppState;
use crate::dto::{CreateReplayRequest, ExecuteReplayRequest, ExperimentRequest, ReplayRunSummary};
use crate::engines::{inspect_replay, list_replays};
use crate::error::{ApiError, ErrorResponse};
use crate::extract::{ApiJson, ApiPath};

fn json<T: serde::Serialize>(value: &T) -> Result<Value, ApiError> {
    serde_json::to_value(value).map_err(|e| ApiError::internal(e.to_string()))
}

fn run_id(raw: &str) -> Result<ReplayRunId, ApiError> {
    serde_json::from_value::<ReplayRunId>(Value::String(raw.to_owned()))
        .ok()
        .filter(|id| !id.as_uuid().is_nil())
        .ok_or_else(|| ApiError::bad_request("invalid_run_id", "run id must be a non-nil UUID"))
}

fn summary(store: &EventStore, run: &ReplayRun) -> Result<ReplayRunSummary, ApiError> {
    let executions = store.replay_executions(run.id)?;
    Ok(ReplayRunSummary {
        run_id: run.id.to_string(),
        stream_id: run.stream_id.to_string(),
        projection: run.projection.to_string(),
        from: run.from.get(),
        to: run.to.get(),
        events: run.to.get() - run.from.get(),
        start_source: json(&run.start_source)?,
        expected_state_digest: run.expected_state_digest.to_hex(),
        created_at: run.created_at.to_rfc3339(),
        execution_count: executions.len() as u64,
        last_outcome: executions.last().map(|e| e.outcome.clone()),
    })
}

/// Replay runs recorded for a stream, oldest first.
#[utoipa::path(get, path = "/api/v1/streams/{stream_id}/replays", tag = "replay",
    params(("stream_id" = String, Path, description = "Stream id")),
    responses((status = 200, body = [ReplayRunSummary]), (status = 404, body = ErrorResponse)))]
pub async fn list_runs(
    State(state): State<AppState>,
    ApiPath(raw): ApiPath<String>,
) -> Result<ApiJson<Vec<ReplayRunSummary>>, ApiError> {
    let id = stream_id(&raw)?;
    blocking(&state, move |store| {
        let runs = list_replays(store, &id)?;
        Ok(ApiJson(
            runs.iter()
                .map(|r| summary(store, r))
                .collect::<Result<_, _>>()?,
        ))
    })
    .await
}

/// Records a replay run over `(start, to]`. Refuses damaged history.
#[utoipa::path(post, path = "/api/v1/streams/{stream_id}/replays", tag = "replay",
    params(("stream_id" = String, Path, description = "Stream id")),
    request_body = CreateReplayRequest,
    responses((status = 201, description = "The recorded ReplayRun, including its trace", body = Object),
              (status = 400, body = ErrorResponse), (status = 404, body = ErrorResponse),
              (status = 409, description = "history_integrity: the interval does not verify", body = ErrorResponse),
              (status = 422, body = ErrorResponse)))]
pub async fn create_run(
    State(state): State<AppState>,
    ApiPath(raw): ApiPath<String>,
    ApiJson(body): ApiJson<CreateReplayRequest>,
) -> Result<(StatusCode, ApiJson<Value>), ApiError> {
    let id = stream_id(&raw)?;
    let engine = engine(&state, body.projection.as_deref())?;
    let to = match body.to {
        Some(to) => Target::At(sequence("to", to)?),
        None => Target::Latest,
    };
    let start = body.start.unwrap_or(ReplayStart::Initial);
    blocking(&state, move |store| {
        let run = engine.record_replay(store, &id, start, to)?;
        Ok((StatusCode::CREATED, ApiJson(json(&run)?)))
    })
    .await
}

/// A run with its full trace and every execution.
#[utoipa::path(get, path = "/api/v1/replays/{run_id}", tag = "replay",
    params(("run_id" = String, Path, description = "Run id (UUID)")),
    responses((status = 200, description = "ReplayInspection: run and executions", body = Object),
              (status = 400, body = ErrorResponse), (status = 404, body = ErrorResponse),
              (status = 500, description = "The stored run is corrupt", body = ErrorResponse)))]
pub async fn get_run(
    State(state): State<AppState>,
    ApiPath(raw): ApiPath<String>,
) -> Result<ApiJson<Value>, ApiError> {
    let id = run_id(&raw)?;
    blocking(&state, move |store| {
        Ok(ApiJson(json(&inspect_replay(store, id)?)?))
    })
    .await
}

/// Executes a run and persists the execution. Divergence, damaged history or
/// a projection mismatch are outcomes (status 200), not errors.
#[utoipa::path(post, path = "/api/v1/replays/{run_id}/executions", tag = "replay",
    params(("run_id" = String, Path, description = "Run id (UUID)")),
    request_body = ExecuteReplayRequest,
    responses((status = 201, description = "ReplayExecution with its classified outcome", body = Object),
              (status = 400, body = ErrorResponse), (status = 404, body = ErrorResponse)))]
pub async fn execute_run(
    State(state): State<AppState>,
    ApiPath(raw): ApiPath<String>,
    ApiJson(body): ApiJson<ExecuteReplayRequest>,
) -> Result<(StatusCode, ApiJson<Value>), ApiError> {
    let id = run_id(&raw)?;
    let explicit = body
        .projection
        .map(|p| engine(&state, Some(&p)))
        .transpose()?;
    let engines = state.engines.clone();
    blocking(&state, move |store| {
        let engine = match explicit {
            Some(engine) => engine,
            None => {
                // Default to the recorded projection; if it is not registered,
                // run with any engine so the outcome reports the mismatch.
                let run = inspect_replay(store, id)?.run;
                engines
                    .for_key(&run.projection)
                    .unwrap_or_else(|| engines.any())
            }
        };
        let execution = engine.execute_replay(store, id)?;
        Ok((StatusCode::CREATED, ApiJson(json(&execution)?)))
    })
    .await
}

/// Runs a structured experiment: verify, compare snapshot-assisted with full
/// replay, record a run from the initial state and execute it.
#[utoipa::path(post, path = "/api/v1/streams/{stream_id}/experiments", tag = "replay",
    params(("stream_id" = String, Path, description = "Stream id")),
    request_body = ExperimentRequest,
    responses((status = 201, description = "ExperimentReport", body = Object),
              (status = 400, body = ErrorResponse), (status = 404, body = ErrorResponse), (status = 422, body = ErrorResponse)))]
pub async fn run_experiment(
    State(state): State<AppState>,
    ApiPath(raw): ApiPath<String>,
    ApiJson(body): ApiJson<ExperimentRequest>,
) -> Result<(StatusCode, ApiJson<Value>), ApiError> {
    let id = stream_id(&raw)?;
    let engine = engine(&state, body.projection.as_deref())?;
    let to = match body.to {
        Some(to) => Target::At(sequence("to", to)?),
        None => Target::Latest,
    };
    let policy: SnapshotPolicy = body.snapshot_policy.unwrap_or_default().into();
    blocking(&state, move |store| {
        let report = engine.experiment(store, &id, to, policy)?;
        Ok((StatusCode::CREATED, ApiJson(json(&report)?)))
    })
    .await
}
