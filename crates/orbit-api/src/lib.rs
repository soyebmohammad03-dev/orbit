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
use std::time::Instant;

use axum::Router;
use axum::extract::{DefaultBodyLimit, Request};
use axum::http::{HeaderValue, Method, header};
use axum::middleware::{self, Next};
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
        }
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
    use routes::{integrity, replay, snapshots, state as st, streams, system};
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
        .fallback(|| async {
            error::ApiError::not_found(
                "route_not_found",
                "no such endpoint; see /api/v1/openapi.json",
            )
        });

    Router::new()
        .nest("/api/v1", api)
        .layer(DefaultBodyLimit::max(body_limit))
        .layer(middleware::from_fn(timing))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
