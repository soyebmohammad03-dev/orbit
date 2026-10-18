//! The experiment lab: registered implementations, differential replay
//! experiments and their verification. The registry is `orbit-lab`'s own
//! database; these handlers never write to the event store.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use orbit_lab::{DifferentialSpec, ExperimentId, Lab};
use serde_json::Value;

use super::page_limit;
use crate::AppState;
use crate::dto::{
    ArtifactVerificationResponse, CreateExperimentRequest, ExperimentDetail, ExperimentPage,
    ExperimentPageQuery, ImplementationList, TraceQuery,
};
use crate::error::{ApiError, ErrorResponse};
use crate::extract::{ApiJson, ApiPath, ApiQuery};

fn lab(state: &AppState) -> Result<Arc<Lab>, ApiError> {
    state.lab.clone().ok_or_else(|| ApiError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        code: "lab_unavailable",
        message: "the experiment lab is disabled on this server (ORBIT_LAB_DIR is empty)"
            .to_owned(),
        class: None,
        details: Value::Null,
    })
}

fn experiment_id(raw: &str) -> Result<ExperimentId, ApiError> {
    raw.parse().map_err(|_| {
        ApiError::bad_request(
            "invalid_experiment_id",
            format!("{raw:?} is not an experiment id (UUID)"),
        )
    })
}

fn json<T: serde::Serialize>(value: &T) -> Result<Value, ApiError> {
    serde_json::to_value(value).map_err(|e| ApiError::internal(e.to_string()))
}

/// Runs a lab call on the blocking pool, with the event store.
async fn with_lab<T, F>(state: &AppState, f: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce(&Lab, &orbit_core::EventStore) -> Result<T, ApiError> + Send + 'static,
{
    let lab = lab(state)?;
    let store = Arc::clone(&state.store);
    tokio::task::spawn_blocking(move || f(&lab, &store))
        .await
        .map_err(|e| ApiError::internal(format!("lab task failed: {e}")))?
}

/// Lists the implementations experiments can use.
#[utoipa::path(get, path = "/api/v1/lab/implementations", tag = "lab",
    responses((status = 200, body = ImplementationList), (status = 503, body = ErrorResponse)))]
pub async fn list_implementations(
    State(state): State<AppState>,
) -> Result<ApiJson<ImplementationList>, ApiError> {
    let lab = lab(&state)?;
    let implementations = lab
        .catalog()
        .all()
        .iter()
        .map(|i| json(i.descriptor()))
        .collect::<Result<_, _>>()?;
    Ok(ApiJson(ImplementationList { implementations }))
}

/// Lists experiments, newest first, with cursor pagination.
#[utoipa::path(get, path = "/api/v1/lab/experiments", tag = "lab", params(ExperimentPageQuery),
    responses((status = 200, body = ExperimentPage), (status = 400, body = ErrorResponse),
              (status = 503, body = ErrorResponse)))]
pub async fn list_experiments(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<ExperimentPageQuery>,
) -> Result<ApiJson<ExperimentPage>, ApiError> {
    let limit = page_limit(query.limit)?;
    let before = query.before.as_deref().map(experiment_id).transpose()?;
    with_lab(&state, move |lab, _| {
        let mut rows = lab.list(limit + 1, before)?;
        let more = rows.len() > limit;
        rows.truncate(limit);
        let next_before = if more {
            rows.last().map(|r| r.id.to_string())
        } else {
            None
        };
        Ok(ApiJson(ExperimentPage {
            experiments: rows.iter().map(json).collect::<Result<_, _>>()?,
            next_before,
        }))
    })
    .await
}

/// Creates a differential replay experiment and runs it to completion.
///
/// Responds 201 with the stored experiment. Its `status` is `completed`
/// (with a result, which may report a divergence or an implementation
/// failure) or `failed` (the input history was unavailable or damaged).
/// The run is synchronous; if it outlasts the request deadline the response
/// is 503 `request_timeout`, but the experiment still finishes in the registry.
#[utoipa::path(post, path = "/api/v1/lab/experiments", tag = "lab",
    request_body = CreateExperimentRequest,
    responses((status = 201, description = "The stored experiment", body = Object),
              (status = 400, body = ErrorResponse), (status = 503, body = ErrorResponse)))]
pub async fn create_experiment(
    State(state): State<AppState>,
    ApiJson(body): ApiJson<CreateExperimentRequest>,
) -> Result<(StatusCode, ApiJson<Value>), ApiError> {
    if body.kind == orbit_lab::reliability::RELIABILITY {
        return Err(ApiError::bad_request(
            "unsupported_experiment_kind",
            "reliability experiments inject faults and kill processes, so this API cannot run them; \
             use the orbit-reliability tool (see docs/RELIABILITY.md). Their results are listed here.",
        ));
    }
    if body.kind != orbit_lab::spec::DIFFERENTIAL_REPLAY {
        return Err(ApiError::bad_request(
            "unsupported_experiment_kind",
            format!(
                "experiment kind {:?} is not supported; use \"differential_replay\"",
                body.kind
            ),
        ));
    }
    let spec = DifferentialSpec::from_json(body.spec)?;
    with_lab(&state, move |lab, store| {
        let record = lab.create_and_run(&spec, store)?;
        Ok((StatusCode::CREATED, ApiJson(json(&record)?)))
    })
    .await
}

/// One experiment with its result, artifacts and verification history.
#[utoipa::path(get, path = "/api/v1/lab/experiments/{experiment_id}", tag = "lab",
    params(("experiment_id" = String, Path, description = "Experiment id (UUID)")),
    responses((status = 200, body = ExperimentDetail), (status = 400, body = ErrorResponse),
              (status = 404, body = ErrorResponse)))]
pub async fn get_experiment(
    State(state): State<AppState>,
    ApiPath(raw): ApiPath<String>,
) -> Result<ApiJson<ExperimentDetail>, ApiError> {
    let id = experiment_id(&raw)?;
    with_lab(&state, move |lab, _| {
        let experiment = json(&lab.experiment(id)?)?;
        let verifications = lab
            .verifications(id)?
            .iter()
            .map(json)
            .collect::<Result<_, _>>()?;
        Ok(ApiJson(ExperimentDetail {
            experiment,
            verifications,
        }))
    })
    .await
}

/// Checks the stored record digests and every artifact of a finished
/// experiment, and records the check.
#[utoipa::path(post, path = "/api/v1/lab/experiments/{experiment_id}/verify", tag = "lab",
    params(("experiment_id" = String, Path, description = "Experiment id (UUID)")),
    responses((status = 200, body = ArtifactVerificationResponse), (status = 404, body = ErrorResponse),
              (status = 409, description = "The experiment is not finished", body = ErrorResponse)))]
pub async fn verify_experiment(
    State(state): State<AppState>,
    ApiPath(raw): ApiPath<String>,
) -> Result<ApiJson<ArtifactVerificationResponse>, ApiError> {
    let id = experiment_id(&raw)?;
    with_lab(&state, move |lab, _| {
        let (report, verification) = lab.verify_artifacts(id)?;
        Ok(ApiJson(ArtifactVerificationResponse {
            report: json(&report)?,
            verification: json(&verification)?,
        }))
    })
    .await
}

/// Executes a completed experiment again from its persisted specification
/// and compares results. The outcome (`reproduced`, `differs`,
/// `history_changed`, `history_unavailable`, `implementation_unavailable`)
/// is recorded and returned.
#[utoipa::path(post, path = "/api/v1/lab/experiments/{experiment_id}/reproduce", tag = "lab",
    params(("experiment_id" = String, Path, description = "Experiment id (UUID)")),
    responses((status = 201, description = "The stored verification record", body = Object),
              (status = 404, body = ErrorResponse),
              (status = 409, description = "The experiment is not completed", body = ErrorResponse)))]
pub async fn reproduce_experiment(
    State(state): State<AppState>,
    ApiPath(raw): ApiPath<String>,
) -> Result<(StatusCode, ApiJson<Value>), ApiError> {
    let id = experiment_id(&raw)?;
    with_lab(&state, move |lab, store| {
        let verification = lab.reproduce(id, store)?;
        Ok((StatusCode::CREATED, ApiJson(json(&verification)?)))
    })
    .await
}

/// A page of a completed experiment's per-boundary trace: both state
/// digests and whether they are equal, read from the verified trace
/// artifacts.
#[utoipa::path(get, path = "/api/v1/lab/experiments/{experiment_id}/trace", tag = "lab",
    params(("experiment_id" = String, Path, description = "Experiment id (UUID)"), TraceQuery),
    responses((status = 200, description = "TracePage", body = Object), (status = 400, body = ErrorResponse),
              (status = 404, body = ErrorResponse),
              (status = 409, description = "The experiment is not completed", body = ErrorResponse),
              (status = 500, description = "A trace artifact is missing or corrupted", body = ErrorResponse)))]
pub async fn get_trace(
    State(state): State<AppState>,
    ApiPath(raw): ApiPath<String>,
    ApiQuery(query): ApiQuery<TraceQuery>,
) -> Result<ApiJson<Value>, ApiError> {
    let id = experiment_id(&raw)?;
    let limit = page_limit(query.limit)?;
    with_lab(&state, move |lab, _| {
        Ok(ApiJson(json(&lab.trace(id, query.from, limit)?)?))
    })
    .await
}

/// The supported reliability scenarios (read-only). They can only be run
/// with the `orbit-reliability` tool; their results appear as experiments.
#[utoipa::path(get, path = "/api/v1/lab/reliability/scenarios", tag = "lab",
    responses((status = 200, description = "Scenario catalog", body = Object), (status = 503, body = ErrorResponse)))]
pub async fn list_reliability_scenarios(
    State(state): State<AppState>,
) -> Result<ApiJson<Value>, ApiError> {
    lab(&state)?;
    Ok(ApiJson(serde_json::json!({
        "scenarios": orbit_lab::reliability::SCENARIOS,
        "execution": "orbit-reliability tool only (cargo run -p orbit-lab --features reliability --bin orbit-reliability -- run <scenario>)",
    })))
}
