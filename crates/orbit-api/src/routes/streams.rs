//! Streams and events.

use axum::extract::State;
use axum::http::StatusCode;
use orbit_core::{AppendRequest, EventId, EventType, Metadata, SchemaVersion, Sequence, StreamId};
use serde_json::Value;

use super::{blocking, page_limit, sequence, stream_id};
use crate::AppState;
use crate::dto::{
    AppendEventRequest, CreateStreamRequest, EventDto, EventPage, EventPageQuery, StreamDetail,
    StreamPage, StreamPageQuery, StreamSummaryDto,
};
use crate::error::{ApiError, ErrorResponse};
use crate::extract::{ApiJson, ApiPath, ApiQuery};

/// Lists streams ordered by id, with cursor pagination.
#[utoipa::path(get, path = "/api/v1/streams", tag = "streams", params(StreamPageQuery),
    responses((status = 200, body = StreamPage), (status = 400, body = ErrorResponse)))]
pub async fn list_streams(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<StreamPageQuery>,
) -> Result<ApiJson<StreamPage>, ApiError> {
    let limit = page_limit(query.limit)?;
    let after = query.after.as_deref().map(stream_id).transpose()?;
    blocking(&state, move |store| {
        // Fetch one extra row to know whether another page exists.
        let mut rows = store.stream_summaries(after.as_ref(), limit + 1)?;
        let more = rows.len() > limit;
        rows.truncate(limit);
        let next_after = if more {
            rows.last().map(|s| s.stream_id.to_string())
        } else {
            None
        };
        Ok(ApiJson(StreamPage {
            streams: rows.into_iter().map(StreamSummaryDto::from).collect(),
            next_after,
        }))
    })
    .await
}

/// Creates an empty stream.
#[utoipa::path(post, path = "/api/v1/streams", tag = "streams", request_body = CreateStreamRequest,
    responses((status = 201, body = StreamDetail), (status = 400, body = ErrorResponse),
              (status = 409, description = "Stream already exists", body = ErrorResponse),
              (status = 422, body = ErrorResponse)))]
pub async fn create_stream(
    State(state): State<AppState>,
    ApiJson(body): ApiJson<CreateStreamRequest>,
) -> Result<(StatusCode, ApiJson<StreamDetail>), ApiError> {
    let id = stream_id(&body.stream_id)?;
    let metadata: Metadata = body.metadata.unwrap_or_default();
    blocking(&state, move |store| {
        store.create_stream(&id, metadata)?;
        Ok((StatusCode::CREATED, ApiJson(detail(store, &id)?)))
    })
    .await
}

fn detail(store: &orbit_core::EventStore, id: &StreamId) -> Result<StreamDetail, ApiError> {
    let stream = store.stream(id)?;
    let summary = store.stream_summary(id)?;
    Ok(StreamDetail {
        summary: summary.into(),
        metadata: Value::Object(stream.metadata),
    })
}

/// Stream metadata, head and counts.
#[utoipa::path(get, path = "/api/v1/streams/{stream_id}", tag = "streams",
    params(("stream_id" = String, Path, description = "Stream id")),
    responses((status = 200, body = StreamDetail), (status = 404, body = ErrorResponse)))]
pub async fn get_stream(
    State(state): State<AppState>,
    ApiPath(raw): ApiPath<String>,
) -> Result<ApiJson<StreamDetail>, ApiError> {
    let id = stream_id(&raw)?;
    blocking(&state, move |store| Ok(ApiJson(detail(store, &id)?))).await
}

/// A page of events in ascending sequence order.
#[utoipa::path(get, path = "/api/v1/streams/{stream_id}/events", tag = "events",
    params(("stream_id" = String, Path, description = "Stream id"), EventPageQuery),
    responses((status = 200, body = EventPage), (status = 400, body = ErrorResponse), (status = 404, body = ErrorResponse)))]
pub async fn list_events(
    State(state): State<AppState>,
    ApiPath(raw): ApiPath<String>,
    ApiQuery(query): ApiQuery<EventPageQuery>,
) -> Result<ApiJson<EventPage>, ApiError> {
    let id = stream_id(&raw)?;
    let limit = page_limit(query.limit)?;
    let after = sequence("after", query.after.unwrap_or(0))?;
    blocking(&state, move |store| {
        let head = store.head(&id)?.sequence;
        let events = store.read_after(&id, after, limit)?;
        let next_after = events
            .last()
            .map(|e| e.sequence)
            .filter(|s| *s < head)
            .map(Sequence::get);
        Ok(ApiJson(EventPage {
            events: events.into_iter().map(EventDto::from).collect(),
            next_after,
            head_sequence: head.get(),
        }))
    })
    .await
}

/// Appends one event. `expected_sequence` enables optimistic concurrency.
#[utoipa::path(post, path = "/api/v1/streams/{stream_id}/events", tag = "events",
    params(("stream_id" = String, Path, description = "Stream id")),
    request_body = AppendEventRequest,
    responses((status = 201, body = EventDto), (status = 400, body = ErrorResponse), (status = 404, body = ErrorResponse),
              (status = 409, description = "sequence_conflict or duplicate_event_id", body = ErrorResponse),
              (status = 413, body = ErrorResponse), (status = 422, body = ErrorResponse)))]
pub async fn append_event(
    State(state): State<AppState>,
    ApiPath(raw): ApiPath<String>,
    ApiJson(body): ApiJson<AppendEventRequest>,
) -> Result<(StatusCode, ApiJson<EventDto>), ApiError> {
    let id = stream_id(&raw)?;
    let mut request = AppendRequest::new(EventType::new(body.event_type)?, body.payload);
    if let Some(version) = body.schema_version {
        request = request.with_schema_version(SchemaVersion::new(version)?);
    }
    if let Some(metadata) = body.metadata {
        request = request.with_metadata(metadata);
    }
    if let Some(expected) = body.expected_sequence {
        request = request.expect_sequence(sequence("expected_sequence", expected)?);
    }
    if let Some(event_id) = body.event_id {
        request = request.with_event_id(EventId::from_uuid(event_id)?);
    }
    blocking(&state, move |store| {
        let event = store.append(&id, request)?;
        Ok((StatusCode::CREATED, ApiJson(EventDto::from(event))))
    })
    .await
}

/// One event by sequence.
#[utoipa::path(get, path = "/api/v1/streams/{stream_id}/events/{sequence}", tag = "events",
    params(("stream_id" = String, Path, description = "Stream id"), ("sequence" = u64, Path, description = "Sequence (≥ 1)")),
    responses((status = 200, body = EventDto), (status = 404, body = ErrorResponse)))]
pub async fn get_event(
    State(state): State<AppState>,
    ApiPath((raw, seq)): ApiPath<(String, u64)>,
) -> Result<ApiJson<EventDto>, ApiError> {
    let id = stream_id(&raw)?;
    let seq = sequence("sequence", seq)?;
    blocking(&state, move |store| {
        Ok(ApiJson(EventDto::from(store.event(&id, seq)?)))
    })
    .await
}
