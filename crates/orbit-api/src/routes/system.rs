//! Health, capabilities, overview, benchmarks and the OpenAPI document.

use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use orbit_core::replay::MAX_REPLAY_EVENTS;
use utoipa::OpenApi;

use super::blocking;
use crate::AppState;
use crate::benchmarks;
use crate::dto::{
    BenchmarksResponse, CapabilitiesResponse, DEFAULT_PAGE_SIZE, DatabaseHealth, EventDto,
    HealthResponse, LimitsInfo, MAX_EXPLAIN_EVENTS, MAX_PAGE_SIZE, OverviewResponse,
    ProjectionInfo, StatisticsDto, StreamSummaryDto,
};
use crate::error::{ApiError, ErrorResponse};
use crate::extract::ApiJson;

/// Engine (and API) version.
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Service health. Runs a real query against the database.
#[utoipa::path(get, path = "/api/v1/health", tag = "system",
    responses((status = 200, body = HealthResponse), (status = 500, body = ErrorResponse)))]
pub async fn health(State(state): State<AppState>) -> Result<ApiJson<HealthResponse>, ApiError> {
    let statistics = blocking(&state, |store| Ok(store.statistics()?)).await?;
    Ok(ApiJson(HealthResponse {
        status: "ok".into(),
        api_version: "v1".into(),
        engine_version: ENGINE_VERSION.into(),
        database: DatabaseHealth {
            reachable: true,
            schema_version: statistics.schema_version,
        },
    }))
}

/// Projections, limits and optional features.
#[utoipa::path(get, path = "/api/v1/capabilities", tag = "system",
    responses((status = 200, body = CapabilitiesResponse)))]
pub async fn capabilities(State(state): State<AppState>) -> ApiJson<CapabilitiesResponse> {
    let limits = state.store.config().limits;
    ApiJson(CapabilitiesResponse {
        api_version: "v1".into(),
        engine_version: ENGINE_VERSION.into(),
        default_projection: crate::engines::DEFAULT_PROJECTION.into(),
        projections: state
            .engines
            .all()
            .iter()
            .map(|e| {
                let key = e.key();
                ProjectionInfo {
                    id: key.id.to_string(),
                    version: key.version,
                    key: key.to_string(),
                    description: e.description().into(),
                }
            })
            .collect(),
        snapshot_policies: vec!["ignore".into(), "prefer".into(), "strict".into()],
        limits: LimitsInfo {
            max_payload_bytes: limits.max_payload_bytes,
            max_metadata_bytes: limits.max_metadata_bytes,
            max_body_bytes: state.max_body_bytes,
            default_page_size: DEFAULT_PAGE_SIZE,
            max_page_size: MAX_PAGE_SIZE,
            max_explain_events: MAX_EXPLAIN_EVENTS,
            max_replay_events: MAX_REPLAY_EVENTS,
        },
        benchmarks_available: state.benchmark_dir.as_ref().is_some_and(|d| d.is_dir()),
    })
}

/// Store-wide counts, recent events and the most active streams.
#[utoipa::path(get, path = "/api/v1/overview", tag = "system",
    responses((status = 200, body = OverviewResponse), (status = 500, body = ErrorResponse)))]
pub async fn overview(
    State(state): State<AppState>,
) -> Result<ApiJson<OverviewResponse>, ApiError> {
    blocking(&state, |store| {
        let s = store.statistics()?;
        let recent = store.recent_events(10)?;
        // Bounded scan for the activity list: the first 500 streams by id.
        let mut streams = store.stream_summaries(None, MAX_PAGE_SIZE)?;
        streams.sort_by(|a, b| {
            b.last_recorded_at
                .cmp(&a.last_recorded_at)
                .then_with(|| a.stream_id.cmp(&b.stream_id))
        });
        streams.truncate(10);
        Ok(ApiJson(OverviewResponse {
            statistics: StatisticsDto {
                streams: s.streams,
                events: s.events,
                snapshots: s.snapshots,
                replay_runs: s.replay_runs,
                replay_executions: s.replay_executions,
                schema_version: s.schema_version,
            },
            recent_events: recent.into_iter().map(EventDto::from).collect(),
            active_streams: streams.into_iter().map(StreamSummaryDto::from).collect(),
        }))
    })
    .await
}

/// Criterion benchmark results found in the configured results directory
/// (read-only). Values are measurements from `cargo bench`, not live timings.
#[utoipa::path(get, path = "/api/v1/benchmarks", tag = "system",
    responses((status = 200, body = BenchmarksResponse)))]
pub async fn benchmarks(
    State(state): State<AppState>,
) -> Result<ApiJson<BenchmarksResponse>, ApiError> {
    let dir = state.benchmark_dir.clone();
    let response = tokio::task::spawn_blocking(move || benchmarks::load(dir.as_deref()))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(ApiJson(response))
}

/// The OpenAPI 3.1 document of this API.
#[utoipa::path(get, path = "/api/v1/openapi.json", tag = "system",
    responses((status = 200, description = "OpenAPI document", content_type = "application/json")))]
pub async fn openapi() -> impl IntoResponse {
    let body = crate::openapi::ApiDoc::openapi()
        .to_pretty_json()
        .unwrap_or_else(|_| "{}".into());
    ([(header::CONTENT_TYPE, "application/json")], body)
}
