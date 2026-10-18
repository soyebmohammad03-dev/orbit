//! The experiment lab: registered implementations, differential replay
//! experiments and their verification. The registry is `orbit-lab`'s own
//! database; these handlers never write to the event store.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::extract::State;
use axum::http::StatusCode;
use orbit_lab::performance::{self, PerformanceSpec, RegressionPolicy};
use orbit_lab::{DifferentialSpec, ExperimentId, Lab};
use serde_json::Value;

use super::page_limit;
use crate::AppState;
use crate::dto::{
    ArtifactVerificationResponse, CompareRequest, CompareRunsRequest, CreateExperimentRequest,
    ExperimentDetail, ExperimentPage, ExperimentPageQuery, ImplementationList, TraceQuery,
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

pub(crate) fn lab_handle(state: &AppState) -> Result<Arc<Lab>, ApiError> {
    lab(state)
}

/// Clears the `measuring` flag when the background work ends, however it ends.
struct Measuring(Arc<AtomicBool>);

impl Drop for Measuring {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Runs `prepare` on the blocking pool and then `work` on its own thread,
/// with at most one such job at a time per server: a performance
/// measurement must not compete with another one, and the number of
/// threads stays bounded. A second request gets 409 `measurement_in_progress`.
///
/// The job is not cancellable. If the server stops while it runs, the
/// experiment is left `running` and marked `failed`/`interrupted` when the
/// lab is next opened.
pub(crate) async fn in_background<T, P, W>(
    state: &AppState,
    prepare: P,
    work: W,
) -> Result<T, ApiError>
where
    T: Send + 'static,
    P: FnOnce(&Lab) -> Result<(T, Option<orbit_lab::ExperimentId>), ApiError> + Send + 'static,
    W: FnOnce(&Lab, Option<orbit_lab::ExperimentId>) + Send + 'static,
{
    let lab = lab(state)?;
    if state.measuring.swap(true, Ordering::SeqCst) {
        return Err(ApiError {
            status: StatusCode::CONFLICT,
            code: "measurement_in_progress",
            message: "a performance measurement is already running on this server; wait for it to finish (one runs at a time)".to_owned(),
            class: Some(orbit_core::ErrorClass::Retryable),
            details: Value::Null,
        });
    }
    let guard = Measuring(Arc::clone(&state.measuring));
    let prepared = {
        let lab = Arc::clone(&lab);
        tokio::task::spawn_blocking(move || prepare(&lab))
            .await
            .map_err(|e| ApiError::internal(format!("lab task failed: {e}")))?
    };
    let (value, id) = prepared?;
    std::thread::Builder::new()
        .name("orbit-measurement".to_owned())
        .spawn(move || {
            let _guard = guard;
            work(&lab, id);
        })
        .map_err(|e| ApiError::internal(format!("cannot start the measurement: {e}")))?;
    Ok(value)
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
    let kinds = [
        orbit_lab::spec::DIFFERENTIAL_REPLAY,
        orbit_lab::reliability::RELIABILITY,
        performance::PERFORMANCE,
    ];
    let kind = match query.kind.as_deref() {
        None => None,
        Some(k) => Some(*kinds.iter().find(|x| **x == k).ok_or_else(|| {
            ApiError::bad_request(
                "invalid_parameter",
                format!("kind must be one of {}", kinds.join(", ")),
            )
        })?),
    };
    with_lab(&state, move |lab, _| {
        let mut rows = lab.registry().list_kind(limit + 1, before, kind)?;
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

/// Creates an experiment and runs it.
///
/// - `differential_replay`: runs to completion within the request and
///   responds 201 with the stored experiment, `completed` (with a result,
///   which may report a divergence or an implementation failure) or
///   `failed` (the input history was unavailable or damaged). If it
///   outlasts the request deadline the response is 503 `request_timeout`,
///   but the experiment still finishes in the registry.
/// - `performance`: responds 202 with the experiment in status `created`
///   and measures it in the background; poll `GET /lab/experiments/{id}`
///   until it is `completed` or `failed`. One measurement runs at a time
///   (409 `measurement_in_progress` otherwise). It cannot be cancelled.
/// - `reliability`: refused with 400 (fault injection is not available here).
#[utoipa::path(post, path = "/api/v1/lab/experiments", tag = "lab",
    request_body = CreateExperimentRequest,
    responses((status = 201, description = "The stored differential replay", body = Object),
              (status = 202, description = "The accepted performance experiment (status created)", body = Object),
              (status = 400, body = ErrorResponse), (status = 409, body = ErrorResponse),
              (status = 503, body = ErrorResponse)))]
pub async fn create_experiment(
    State(state): State<AppState>,
    ApiJson(body): ApiJson<CreateExperimentRequest>,
) -> Result<(StatusCode, ApiJson<Value>), ApiError> {
    if body.kind == performance::PERFORMANCE {
        let spec = PerformanceSpec::from_json(body.spec)?;
        spec.validated()?;
        let record = in_background(
            &state,
            move |lab| {
                let record = lab.create_performance(&spec)?;
                let id = record.id;
                Ok((json(&record)?, Some(id)))
            },
            |lab, id| {
                if let Some(id) = id
                    && let Err(e) = lab.run_performance(id)
                {
                    tracing::error!(%id, error = %e, "performance measurement could not be recorded");
                }
            },
        )
        .await?;
        return Ok((StatusCode::ACCEPTED, ApiJson(record)));
    }
    if body.kind == orbit_lab::reliability::RELIABILITY {
        return Err(ApiError::bad_request(
            "unsupported_experiment_kind",
            "reliability experiments inject faults and kill processes, so this API never runs them itself; \
             launch one with POST /api/v1/lab/reliability/runs (needs a configured runner) or the \
             orbit-reliability tool (see docs/RELIABILITY.md)",
        ));
    }
    if body.kind != orbit_lab::spec::DIFFERENTIAL_REPLAY {
        return Err(ApiError::bad_request(
            "unsupported_experiment_kind",
            format!(
                "experiment kind {:?} is not supported; use \"differential_replay\" or \"performance\"",
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

/// Reproduces a completed experiment; the original is never modified.
///
/// - Differential replay: executed again from its persisted specification
///   and compared; responds 201 with the stored verification record
///   (`status` is a reproduction status such as `reproduced`, `differs`,
///   `history_changed`, `history_unavailable`, `implementation_unavailable`,
///   `artifact_missing` or `configuration_mismatch`).
/// - Performance: responds 202 with the linked new experiment (status
///   `created`), measured in the background; when it finishes, a
///   verification naming it is appended to the original. Same one-at-a-time
///   rule as creating a performance experiment.
/// - Reliability: 409 (reproduced only with the orbit-reliability tool).
#[utoipa::path(post, path = "/api/v1/lab/experiments/{experiment_id}/reproduce", tag = "lab",
    params(("experiment_id" = String, Path, description = "Experiment id (UUID)")),
    responses((status = 201, description = "The stored verification record", body = Object),
              (status = 202, description = "The linked performance attempt (status created)", body = Object),
              (status = 404, body = ErrorResponse),
              (status = 409, description = "The experiment is not completed, or a measurement is running", body = ErrorResponse)))]
pub async fn reproduce_experiment(
    State(state): State<AppState>,
    ApiPath(raw): ApiPath<String>,
) -> Result<(StatusCode, ApiJson<Value>), ApiError> {
    let id = experiment_id(&raw)?;
    let kind = with_lab(&state, move |lab, _| Ok(lab.experiment(id)?.kind)).await?;
    if kind == performance::PERFORMANCE {
        let value = in_background(
            &state,
            move |lab| match lab.prepare_performance_reproduction(id)? {
                Ok(attempt) => Ok((json(&attempt)?, Some(attempt.id))),
                Err(verification) => Ok((json(&verification)?, None)),
            },
            move |lab, attempt| {
                if let Some(attempt) = attempt
                    && let Err(e) = lab.finish_performance_reproduction(
                        id,
                        attempt,
                        &RegressionPolicy::default(),
                    )
                {
                    tracing::error!(%id, error = %e, "performance reproduction could not be recorded");
                }
            },
        )
        .await?;
        let status = if value.get("detail").is_some() {
            StatusCode::CREATED
        } else {
            StatusCode::ACCEPTED
        };
        return Ok((status, ApiJson(value)));
    }
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

/// The supported reliability scenarios and whether this server can launch
/// them. Launching needs the separately built `orbit-reliability` runner,
/// configured with `ORBIT_RELIABILITY_RUNNER`; it is disabled by default.
#[utoipa::path(get, path = "/api/v1/lab/reliability/scenarios", tag = "lab",
    responses((status = 200, description = "Scenario catalog and runner status", body = Object), (status = 503, body = ErrorResponse)))]
pub async fn list_reliability_scenarios(
    State(state): State<AppState>,
) -> Result<ApiJson<Value>, ApiError> {
    lab(&state)?;
    let runner = state.reliability.as_ref().map_or_else(
        || {
            serde_json::json!({
                "enabled": false,
                "reason": "launching is disabled on this server; set ORBIT_RELIABILITY_RUNNER to the absolute path of a separately built orbit-reliability executable to enable it (see docs/RELIABILITY.md)",
            })
        },
        |l| {
            serde_json::json!({
                "enabled": true,
                "timeout_secs": l.timeout().as_secs(),
                "busy": l.busy(),
            })
        },
    );
    Ok(ApiJson(serde_json::json!({
        "scenarios": orbit_lab::reliability::SCENARIOS,
        "runner": runner,
        "execution": "cargo run -p orbit-lab --features reliability --bin orbit-reliability -- run <scenario>",
    })))
}

/// Body of `POST /api/v1/lab/reliability/runs`: a reliability spec.
pub type LaunchRequest = Value;

/// Launches a catalog scenario in the separately configured runner process.
///
/// The body is a reliability spec (`{scenario, scenario_version?,
/// parameters?}`; unknown fields are rejected, parameters are bounded). It
/// is recorded as a `created` experiment and returned (202); the runner
/// (`orbit-reliability --lab <lab> run-id <id>`, no client text on its
/// command line) executes it. Poll the experiment. One run at a time;
/// runs past the deadline are stopped and recorded as infrastructure
/// failures. Nothing here injects faults: only the runner can.
#[utoipa::path(post, path = "/api/v1/lab/reliability/runs", tag = "lab",
    request_body(content = Object, description = "A reliability spec"),
    responses((status = 202, description = "The recorded experiment (status created)", body = Object),
              (status = 400, body = ErrorResponse),
              (status = 409, description = "A reliability run is already in progress", body = ErrorResponse),
              (status = 503, description = "Launching is disabled on this server", body = ErrorResponse)))]
pub async fn launch_reliability(
    State(state): State<AppState>,
    ApiJson(body): ApiJson<LaunchRequest>,
) -> Result<(StatusCode, ApiJson<Value>), ApiError> {
    let lab = lab(&state)?;
    let launcher = state.reliability.clone().ok_or_else(|| ApiError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        code: "reliability_runner_disabled",
        message: "reliability runs cannot be launched on this server: ORBIT_RELIABILITY_RUNNER is not set (see docs/RELIABILITY.md)".to_owned(),
        class: None,
        details: Value::Null,
    })?;
    let spec = orbit_lab::reliability::ReliabilitySpec::from_json(body)?;
    let record = tokio::task::spawn_blocking(move || {
        launcher.launch(lab, &spec, |report| {
            tracing::info!(id = %report.id, status = %report.status, timed_out = report.timed_out, finalized = ?report.finalized, "reliability run ended");
        })
    })
    .await
    .map_err(|e| ApiError::internal(format!("lab task failed: {e}")))?
    .map_err(|e| match e {
        orbit_lab::reliability::launcher::LaunchError::Busy => ApiError {
            status: StatusCode::CONFLICT,
            code: "reliability_run_in_progress",
            message: e.to_string(),
            class: Some(orbit_core::ErrorClass::Retryable),
            details: Value::Null,
        },
        orbit_lab::reliability::launcher::LaunchError::Lab(e) => e.into(),
    })?;
    Ok((StatusCode::ACCEPTED, ApiJson(json(&record)?)))
}

/// The performance workloads, their bounds and the default regression policy.
#[utoipa::path(get, path = "/api/v1/lab/performance/workloads", tag = "lab",
    responses((status = 200, description = "Workload catalog", body = Object), (status = 503, body = ErrorResponse)))]
pub async fn list_performance_workloads(
    State(state): State<AppState>,
) -> Result<ApiJson<Value>, ApiError> {
    lab(&state)?;
    Ok(ApiJson(serde_json::json!({
        "workloads": performance::WORKLOADS,
        "warmup_iterations": performance::WARMUP,
        "measured_iterations": performance::MEASURED,
        "min_measurement_samples": performance::MIN_MEASUREMENT_SAMPLES,
        "default_policy": RegressionPolicy::default(),
        "build_profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "measuring": state.measuring.load(Ordering::SeqCst),
    })))
}

/// Most candidates in one comparison.
pub const MAX_CANDIDATES: usize = 10;

/// Compares completed performance experiments against a baseline. Nothing
/// is stored. Each comparison reports a verdict (`incompatible`,
/// `insufficient_samples`, `within_threshold`, `suspected_regression`,
/// `suspected_improvement`, `regression`, `improvement`), the absolute and
/// relative change of the median, both median intervals and the test.
#[utoipa::path(post, path = "/api/v1/lab/performance/compare", tag = "lab",
    request_body = CompareRequest,
    responses((status = 200, description = "Comparisons, or status no_baseline", body = Object),
              (status = 400, body = ErrorResponse), (status = 404, body = ErrorResponse),
              (status = 409, description = "Not a completed performance experiment", body = ErrorResponse)))]
pub async fn compare_performance(
    State(state): State<AppState>,
    ApiJson(body): ApiJson<CompareRequest>,
) -> Result<ApiJson<Value>, ApiError> {
    if body.candidates.is_empty() || body.candidates.len() > MAX_CANDIDATES {
        return Err(ApiError::bad_request(
            "invalid_parameter",
            format!("candidates must hold 1 to {MAX_CANDIDATES} experiment ids"),
        ));
    }
    let candidates = body
        .candidates
        .iter()
        .map(|c| experiment_id(c))
        .collect::<Result<Vec<_>, _>>()?;
    let baseline = body.baseline.as_deref().map(experiment_id).transpose()?;
    let defaults = RegressionPolicy::default();
    let policy = RegressionPolicy {
        threshold_percent: body.threshold_percent.unwrap_or(defaults.threshold_percent),
        alpha: body.alpha.unwrap_or(defaults.alpha),
    }
    .validated()
    .map_err(|e| ApiError::bad_request("invalid_parameter", e))?;
    with_lab(&state, move |lab, _| {
        Ok(ApiJson(lab.compare_performance(
            baseline,
            &candidates,
            &policy,
        )?))
    })
    .await
}

/// Compares two sets of independent runs (completed performance
/// experiments of one configuration) on their per-run medians; the test is
/// exact for small run counts and reports the smallest p-value the counts
/// allow. Nothing is stored.
#[utoipa::path(post, path = "/api/v1/lab/performance/compare-runs", tag = "lab",
    request_body = CompareRunsRequest,
    responses((status = 200, description = "The run comparison", body = Object),
              (status = 400, body = ErrorResponse), (status = 404, body = ErrorResponse),
              (status = 409, description = "Not a completed performance experiment", body = ErrorResponse)))]
pub async fn compare_performance_runs(
    State(state): State<AppState>,
    ApiJson(body): ApiJson<CompareRunsRequest>,
) -> Result<ApiJson<Value>, ApiError> {
    let max = orbit_lab::performance::compare::MAX_RUNS;
    if [&body.baseline, &body.candidates]
        .iter()
        .any(|v| v.is_empty() || v.len() > max)
    {
        return Err(ApiError::bad_request(
            "invalid_parameter",
            format!("baseline and candidates must each hold 1 to {max} experiment ids"),
        ));
    }
    let parse = |v: &[String]| {
        v.iter()
            .map(|c| experiment_id(c))
            .collect::<Result<Vec<_>, _>>()
    };
    let (baseline, candidates) = (parse(&body.baseline)?, parse(&body.candidates)?);
    let defaults = RegressionPolicy::default();
    let policy = RegressionPolicy {
        threshold_percent: body.threshold_percent.unwrap_or(defaults.threshold_percent),
        alpha: body.alpha.unwrap_or(defaults.alpha),
    }
    .validated()
    .map_err(|e| ApiError::bad_request("invalid_parameter", e))?;
    with_lab(&state, move |lab, _| {
        Ok(ApiJson(lab.compare_performance_runs(
            &baseline,
            &candidates,
            &policy,
        )?))
    })
    .await
}
