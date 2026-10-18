//! # orbit-api
//!
//! The ORBIT Observatory HTTP API: an Axum server over [`orbit_core`].
//!
//! The engine is synchronous (SQLite). Every engine call runs on Tokio's
//! blocking pool (`spawn_blocking`), so request handling never blocks the
//! async executor. Handlers only validate input, call the engine and map the
//! result; all domain logic lives in `orbit-core`.

pub mod benchmarks;
pub mod config;
pub mod dto;
pub mod engines;
pub mod error;
pub mod extract;
pub mod openapi;
pub mod routes;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::{get, post};
use orbit_core::EventStore;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

pub use config::ApiConfig;
pub use engines::Engines;

/// Shared state of every handler.
#[derive(Clone, Debug)]
pub struct AppState {
    /// The event store (one SQLite connection behind a mutex).
    pub store: Arc<EventStore>,
    /// Registered projections.
    pub engines: Engines,
    /// Criterion results directory, if configured.
    pub benchmark_dir: Option<PathBuf>,
    /// Maximum request body size in bytes.
    pub max_body_bytes: usize,
    /// Accepted `Host` header names (lowercase, without port).
    pub allowed_hosts: Arc<Vec<String>>,
    /// Deadline for one request.
    pub request_timeout: Duration,
    /// The experiment lab, if enabled. Its registry is a separate database.
    pub lab: Option<Arc<orbit_lab::Lab>>,
    /// Set while a performance measurement runs in the background; at most
    /// one runs at a time (see `routes::lab`).
    pub measuring: Arc<std::sync::atomic::AtomicBool>,
    /// Launches reliability runs in the separately configured runner
    /// process, if enabled (see [`ApiConfig::reliability_runner`]).
    pub reliability: Option<Arc<orbit_lab::reliability::launcher::Launcher>>,
}

impl AppState {
    /// State for `store` with the built-in projections. The body limit is the
    /// store's payload plus metadata limits plus 64 KiB of envelope.
    #[must_use]
    pub fn new(store: EventStore, benchmark_dir: Option<PathBuf>) -> Self {
        let limits = store.config().limits;
        Self {
            max_body_bytes: limits.max_payload_bytes + limits.max_metadata_bytes + 64 * 1024,
            store: Arc::new(store),
            engines: Engines::builtin(),
            benchmark_dir,
            allowed_hosts: Arc::new(
                config::LOOPBACK_HOSTS
                    .iter()
                    .map(|h| (*h).to_owned())
                    .collect(),
            ),
            request_timeout: config::DEFAULT_REQUEST_TIMEOUT,
            lab: None,
            measuring: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            reliability: None,
        }
    }

    /// Enables launching reliability runs with `launcher`.
    #[must_use]
    pub fn with_reliability_launcher(
        mut self,
        launcher: orbit_lab::reliability::launcher::Launcher,
    ) -> Self {
        self.reliability = Some(Arc::new(launcher));
        self
    }

    /// Enables the lab endpoints with `lab`.
    #[must_use]
    pub fn with_lab(mut self, lab: orbit_lab::Lab) -> Self {
        self.lab = Some(Arc::new(lab));
        self
    }

    /// Replaces the accepted `Host` names (see [`ApiConfig::allowed_hosts`]).
    #[must_use]
    pub fn with_allowed_hosts(mut self, hosts: Vec<String>) -> Self {
        self.allowed_hosts = Arc::new(hosts);
        self
    }

    /// Replaces the per-request deadline.
    #[must_use]
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }
}

/// The host name part of a `Host` header value: `[::1]:7878` → `[::1]`,
/// `localhost:3000` → `localhost`. Lowercased.
fn host_name(value: &str) -> String {
    let value = value.trim();
    let name = if value.starts_with('[') {
        value.split_inclusive(']').next().unwrap_or(value)
    } else {
        value.rsplit_once(':').map_or(value, |(name, _port)| name)
    };
    name.to_ascii_lowercase()
}

/// Rejects requests whose `Host` is not in the allowlist (DNS-rebinding
/// defence: CORS alone does not stop a rebound page, which the browser treats
/// as same-origin), then applies the per-request deadline.
async fn guard(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(host_name);
    let allowed = host
        .as_ref()
        .is_some_and(|h| state.allowed_hosts.iter().any(|a| a == h));
    if !allowed {
        tracing::warn!(host = ?host, "rejected request with a non-allowlisted Host header");
        return error::ApiError {
            status: StatusCode::FORBIDDEN,
            code: "host_not_allowed",
            message: "the Host header is not allowed; this API only serves local clients (see ORBIT_API_ALLOWED_HOSTS)"
                .to_owned(),
            class: None,
            details: serde_json::Value::Null,
        }
        .into_response();
    }
    match tokio::time::timeout(state.request_timeout, next.run(request)).await {
        Ok(response) => response,
        Err(_) => error::ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "request_timeout",
            message: format!(
                "the request did not complete within {} s; the engine operation may still finish, \
                 so retry writes idempotently (expected_sequence or event_id)",
                state.request_timeout.as_secs_f64()
            ),
            class: Some(orbit_core::ErrorClass::Retryable),
            details: serde_json::Value::Null,
        }
        .into_response(),
    }
}

/// Adds `Server-Timing` and `X-Orbit-Elapsed-Micros` headers with the
/// measured handling time of each request.
async fn timing(request: Request, next: Next) -> Response {
    let started = Instant::now();
    let mut response = next.run(request).await;
    let micros = started.elapsed().as_micros();
    if let Ok(value) = HeaderValue::from_str(&micros.to_string()) {
        response
            .headers_mut()
            .insert("x-orbit-elapsed-micros", value);
    }
    #[allow(clippy::cast_precision_loss)]
    let millis = micros as f64 / 1000.0;
    if let Ok(value) = HeaderValue::from_str(&format!("app;dur={millis:.3}")) {
        response.headers_mut().insert("server-timing", value);
    }
    response
}

/// Builds the router. `cors_origins` are the browser origins allowed to call
/// the API (exact matches only).
pub fn router(state: AppState, cors_origins: &[String]) -> Router {
    use routes::{evidence, integrity, lab, replay, snapshots, state as st, streams, system};
    let origins: Vec<HeaderValue> = cors_origins
        .iter()
        .filter_map(|o| HeaderValue::from_str(o).ok())
        .collect();
    let cors = CorsLayer::new()
        .allow_origin(origins)
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([header::CONTENT_TYPE])
        .expose_headers([
            header::HeaderName::from_static("x-orbit-elapsed-micros"),
            header::HeaderName::from_static("server-timing"),
            header::HeaderName::from_static("x-orbit-bundle-digest"),
            header::CONTENT_DISPOSITION,
        ]);
    let body_limit = state.max_body_bytes;

    let api = Router::new()
        .route("/health", get(system::health))
        .route("/capabilities", get(system::capabilities))
        .route("/overview", get(system::overview))
        .route("/benchmarks", get(system::benchmarks))
        .route("/openapi.json", get(system::openapi))
        .route(
            "/streams",
            get(streams::list_streams).post(streams::create_stream),
        )
        .route("/streams/{stream_id}", get(streams::get_stream))
        .route(
            "/streams/{stream_id}/events",
            get(streams::list_events).post(streams::append_event),
        )
        .route(
            "/streams/{stream_id}/events/{sequence}",
            get(streams::get_event),
        )
        .route("/streams/{stream_id}/state", get(st::get_state))
        .route("/streams/{stream_id}/diff", get(st::get_diff))
        .route("/streams/{stream_id}/explain", get(st::get_explain))
        .route(
            "/streams/{stream_id}/snapshots",
            get(snapshots::list_snapshots).post(snapshots::create_snapshot),
        )
        .route("/snapshots/{snapshot_id}", get(snapshots::get_snapshot))
        .route(
            "/streams/{stream_id}/checkpoint",
            get(integrity::checkpoint),
        )
        .route(
            "/streams/{stream_id}/verify",
            post(integrity::verify_stream),
        )
        .route("/verify", post(integrity::verify_store))
        .route(
            "/streams/{stream_id}/replays",
            get(replay::list_runs).post(replay::create_run),
        )
        .route("/replays/{run_id}", get(replay::get_run))
        .route("/replays/{run_id}/executions", post(replay::execute_run))
        .route(
            "/streams/{stream_id}/experiments",
            post(replay::run_experiment),
        )
        .route("/lab/implementations", get(lab::list_implementations))
        .route(
            "/lab/experiments",
            get(lab::list_experiments).post(lab::create_experiment),
        )
        .route("/lab/experiments/{experiment_id}", get(lab::get_experiment))
        .route(
            "/lab/experiments/{experiment_id}/verify",
            post(lab::verify_experiment),
        )
        .route(
            "/lab/experiments/{experiment_id}/reproduce",
            post(lab::reproduce_experiment),
        )
        .route(
            "/lab/experiments/{experiment_id}/trace",
            get(lab::get_trace),
        )
        .route(
            "/lab/experiments/{experiment_id}/bundle",
            get(evidence::export_bundle),
        )
        .route(
            "/lab/reliability/scenarios",
            get(lab::list_reliability_scenarios),
        )
        .route("/lab/reliability/runs", post(lab::launch_reliability))
        .route(
            "/lab/performance/workloads",
            get(lab::list_performance_workloads),
        )
        .route("/lab/performance/compare", post(lab::compare_performance))
        .route(
            "/lab/performance/compare-runs",
            post(lab::compare_performance_runs),
        )
        .route(
            "/lab/bundles",
            get(evidence::list_bundles)
                .post(evidence::import_bundle)
                .layer(DefaultBodyLimit::max(orbit_lab::bundle::MAX_BUNDLE_BYTES)),
        )
        .route(
            "/lab/bundles/validate",
            post(evidence::validate_bundle)
                .layer(DefaultBodyLimit::max(orbit_lab::bundle::MAX_BUNDLE_BYTES)),
        )
        .route("/lab/bundles/{bundle_digest}", get(evidence::get_bundle))
        .route(
            "/lab/bundles/{bundle_digest}/reproduce",
            post(evidence::reproduce_bundle),
        )
        .fallback(|| async {
            error::ApiError::not_found(
                "route_not_found",
                "no such endpoint; see /api/v1/openapi.json",
            )
        });

    Router::new()
        .nest("/api/v1", api)
        .layer(DefaultBodyLimit::max(body_limit))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .layer(middleware::from_fn(timing))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::host_name;

    #[test]
    fn host_names_are_extracted_without_port() {
        assert_eq!(host_name("127.0.0.1:7878"), "127.0.0.1");
        assert_eq!(host_name("LOCALHOST"), "localhost");
        assert_eq!(host_name("[::1]:7878"), "[::1]");
        assert_eq!(host_name("[::1]"), "[::1]");
        assert_eq!(host_name("evil.example:80"), "evil.example");
    }
}
