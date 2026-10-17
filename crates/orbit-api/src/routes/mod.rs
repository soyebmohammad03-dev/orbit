//! HTTP handlers. Each handler validates its input, runs the engine call on
//! the blocking thread pool, and maps the result to a DTO.

pub mod integrity;
pub mod replay;
pub mod snapshots;
pub mod state;
pub mod streams;
pub mod system;

use std::sync::Arc;

use orbit_core::{EventStore, Sequence, StreamId};

use crate::AppState;
use crate::dto::{DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE};
use crate::engines::{DEFAULT_PROJECTION, DynEngine};
use crate::error::ApiError;

/// Runs a synchronous engine call on Tokio's blocking pool, so SQLite I/O
/// never blocks the async executor.
pub(crate) async fn blocking<T, F>(state: &AppState, f: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce(&EventStore) -> Result<T, ApiError> + Send + 'static,
{
    let store = Arc::clone(&state.store);
    tokio::task::spawn_blocking(move || f(&store))
        .await
        .map_err(|e| ApiError::internal(format!("engine task failed: {e}")))?
}

pub(crate) fn stream_id(raw: &str) -> Result<StreamId, ApiError> {
    StreamId::new(raw).map_err(|e| ApiError::bad_request("invalid_stream_id", e.to_string()))
}

pub(crate) fn sequence(name: &str, value: u64) -> Result<Sequence, ApiError> {
    Sequence::new(value)
        .map_err(|e| ApiError::bad_request("invalid_parameter", format!("{name}: {e}")))
}

pub(crate) fn page_limit(limit: Option<usize>) -> Result<usize, ApiError> {
    match limit.unwrap_or(DEFAULT_PAGE_SIZE) {
        0 => Err(ApiError::bad_request(
            "invalid_parameter",
            "limit must be at least 1",
        )),
        n if n > MAX_PAGE_SIZE => Err(ApiError::bad_request(
            "invalid_parameter",
            format!("limit must be at most {MAX_PAGE_SIZE}"),
        )),
        n => Ok(n),
    }
}

pub(crate) fn engine(state: &AppState, name: Option<&str>) -> Result<Arc<dyn DynEngine>, ApiError> {
    let name = name.unwrap_or(DEFAULT_PROJECTION);
    state.engines.find(name).ok_or_else(|| {
        ApiError::bad_request(
            "unknown_projection",
            format!("projection {name:?} is not registered"),
        )
        .with_details(serde_json::json!({
            "available": state.engines.all().iter().map(|e| e.key().to_string()).collect::<Vec<_>>()
        }))
    })
}
