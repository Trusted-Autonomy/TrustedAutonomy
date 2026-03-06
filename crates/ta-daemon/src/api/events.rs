// api/events.rs — Server-Sent Events (SSE) stream for real-time TA events.
//
// Subscribes to the FsEventStore and streams events to connected clients.
// Supports `?since=<ISO8601>` for replay from a cursor.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use serde::Deserialize;

use ta_events::store::{EventQueryFilter, EventStore, FsEventStore};

use crate::api::AppState;

#[derive(Debug, Deserialize)]
pub struct EventStreamQuery {
    /// ISO 8601 timestamp — replay events after this cursor.
    pub since: Option<String>,
    /// Filter by event types (comma-separated).
    pub types: Option<String>,
}

/// `GET /api/events` — SSE event stream.
pub async fn event_stream(
    State(state): State<Arc<AppState>>,
    Query(params): Query<EventStreamQuery>,
) -> impl IntoResponse {
    let events_dir = state.events_dir.clone();

    // Parse the initial cursor.
    let initial_cursor = params
        .since
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));

    let type_filter: Vec<String> = params
        .types
        .map(|t| t.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();

    // Use an async_stream to poll for new events.
    let stream = async_stream::stream! {
        let mut cursor = initial_cursor;

        loop {
            let store = FsEventStore::new(&events_dir);
            let filter = EventQueryFilter {
                since: cursor,
                event_types: type_filter.clone(),
                limit: Some(50),
                ..Default::default()
            };

            match store.query(&filter) {
                Ok(envelopes) => {
                    for envelope in envelopes {
                        cursor = Some(envelope.timestamp);
                        let data = serde_json::to_string(&envelope)
                            .unwrap_or_else(|_| "{}".to_string());

                        yield Ok::<_, Infallible>(
                            Event::default()
                                .event(envelope.event_type.clone())
                                .id(envelope.id.to_string())
                                .data(data)
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!("Event store query error: {}", e);
                }
            }

            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}
