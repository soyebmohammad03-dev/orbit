//! The OpenAPI document, generated from the handler annotations.

use utoipa::OpenApi;

use crate::routes::{integrity, replay, snapshots, state, streams, system};

/// OpenAPI description of `/api/v1`.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "ORBIT Observatory API",
        description = "Local HTTP API over the ORBIT event store: streams, events, time travel, snapshots, replay and integrity. \
                       Engine reports are returned as the engine serializes them; see docs/API.md.",
        license(name = "MIT")
    ),
    paths(
        system::health, system::capabilities, system::overview, system::benchmarks, system::openapi,
        streams::list_streams, streams::create_stream, streams::get_stream,
        streams::list_events, streams::append_event, streams::get_event,
        state::get_state, state::get_diff, state::get_explain,
        snapshots::list_snapshots, snapshots::create_snapshot, snapshots::get_snapshot,
        integrity::checkpoint, integrity::verify_stream, integrity::verify_store,
        replay::list_runs, replay::create_run, replay::get_run, replay::execute_run, replay::run_experiment,
    ),
    tags(
        (name = "system", description = "Health, capabilities, overview, benchmarks"),
        (name = "streams", description = "Stream lifecycle"),
        (name = "events", description = "Appending and reading events"),
        (name = "time-travel", description = "Reconstruction, diffs, explanations"),
        (name = "snapshots", description = "Snapshot creation and validation"),
        (name = "integrity", description = "Verification and checkpoints"),
        (name = "replay", description = "Replay runs, executions, experiments"),
    )
)]
pub struct ApiDoc;
