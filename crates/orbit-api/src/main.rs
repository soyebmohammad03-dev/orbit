//! `orbit-api`: serves the ORBIT Observatory API.
//!
//! Configuration comes from `ORBIT_*` environment variables; see
//! [`orbit_api::ApiConfig::from_lookup`] and docs/API.md.

use orbit_api::{ApiConfig, AppState, router};
use orbit_core::EventStore;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,tower_http=info")),
        )
        .init();

    let config = ApiConfig::from_env()?;
    if let Some(parent) = config
        .store
        .path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let store = tokio::task::spawn_blocking({
        let store_config = config.store.clone();
        move || EventStore::open(store_config)
    })
    .await??;
    let mut state = AppState::new(store, config.benchmark_dir.clone())
        .with_allowed_hosts(config.allowed_hosts.clone())
        .with_request_timeout(config.request_timeout);
    if let Some(dir) = config.lab_dir.clone() {
        let lab = tokio::task::spawn_blocking(move || {
            orbit_lab::Lab::open(&dir, orbit_lab::Catalog::builtin())
        })
        .await??;
        state = state.with_lab(lab);
        if let Some((runner, timeout)) = config.reliability_runner.clone() {
            let launcher = orbit_lab::reliability::launcher::Launcher::new(&runner, timeout)?;
            tracing::warn!(
                timeout_secs = timeout.as_secs(),
                "reliability runs can be launched through the API (ORBIT_RELIABILITY_RUNNER is set); \
                 they run in the separate orbit-reliability process"
            );
            state = state.with_reliability_launcher(launcher);
        }
    } else if config.reliability_runner.is_some() {
        tracing::warn!("ORBIT_RELIABILITY_RUNNER is ignored because the lab is disabled");
    }
    let app = router(state, &config.cors_origins);

    let listener = tokio::net::TcpListener::bind(config.addr).await?;
    let addr = listener.local_addr()?;
    if !addr.ip().is_loopback() {
        tracing::warn!(%addr, "listening on a non-loopback address: this API has NO authentication; use trusted networks only");
    }
    tracing::info!(%addr, "ORBIT Observatory API listening (OpenAPI at /api/v1/openapi.json)");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("shutting down");
        })
        .await?;
    Ok(())
}
