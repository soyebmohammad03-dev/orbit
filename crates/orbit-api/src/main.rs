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
    let app = router(
        AppState::new(store, config.benchmark_dir.clone()),
        &config.cors_origins,
    );

    let listener = tokio::net::TcpListener::bind(config.addr).await?;
    tracing::info!(addr = %listener.local_addr()?, "ORBIT Observatory API listening; OpenAPI at /api/v1/openapi.json");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("shutting down");
        })
        .await?;
    Ok(())
}
