//! Local web dashboard for claw telemetry.
//!
//! Tails the JSONL events file written by claw when `CLAW_DASHBOARD_EVENTS`
//! is set, aggregates token usage and agent activity, and serves a live UI
//! on localhost. Everything stays on this machine.

pub mod aggregator;

use std::convert::Infallible;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, Json};
use axum::routing::get;
use axum::Router;
use futures_util::Stream;

use aggregator::{Aggregator, Snapshot};

const UI_HTML: &str = include_str!("ui.html");
pub const TAIL_INTERVAL: Duration = Duration::from_millis(300);
const SSE_POLL_INTERVAL: Duration = Duration::from_millis(400);

#[derive(Clone)]
pub struct AppState {
    pub aggregator: Arc<Mutex<Aggregator>>,
}

impl AppState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            aggregator: Arc::new(Mutex::new(Aggregator::new())),
        }
    }

    fn snapshot(&self) -> Snapshot {
        self.aggregator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .snapshot(now_ms())
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

/// Builds the dashboard router: `/` (UI), `/api/state` (JSON snapshot),
/// `/api/stream` (SSE push, emits only when the aggregator changes).
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(serve_ui))
        .route("/api/state", get(serve_state))
        .route("/api/stream", get(serve_stream))
        .with_state(state)
}

async fn serve_ui() -> Html<&'static str> {
    Html(UI_HTML)
}

async fn serve_state(State(state): State<AppState>) -> Json<Snapshot> {
    Json(state.snapshot())
}

async fn serve_stream(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = async_stream(state);
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Yields a snapshot event whenever the aggregator's version changes. The
/// last emitted version travels in the unfold state so it persists across
/// iterations.
fn async_stream(state: AppState) -> impl Stream<Item = Result<Event, Infallible>> {
    futures_util::stream::unfold(
        (state, tokio::time::interval(SSE_POLL_INTERVAL), None::<u64>),
        |(state, mut interval, last_version)| async move {
            loop {
                interval.tick().await;
                let snapshot = state.snapshot();
                if last_version != Some(snapshot.version) {
                    let version = snapshot.version;
                    let event = Event::default()
                        .json_data(&snapshot)
                        .unwrap_or_else(|_| Event::default().data("{}"));
                    return Some((Ok(event), (state, interval, Some(version))));
                }
            }
        },
    )
}

/// Polls the events file, feeding appended lines to the aggregator. Handles
/// the file not existing yet (waits) and truncation (resets and re-reads).
pub async fn tail_events(path: PathBuf, aggregator: Arc<Mutex<Aggregator>>) {
    let mut offset: u64 = 0;
    let mut partial_line = String::new();
    let mut interval = tokio::time::interval(TAIL_INTERVAL);

    loop {
        interval.tick().await;

        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        let length = metadata.len();
        if length < offset {
            // Truncated or replaced: start over.
            offset = 0;
            partial_line.clear();
            aggregator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .reset();
        }
        if length == offset {
            continue;
        }

        let Ok(mut file) = std::fs::File::open(&path) else {
            continue;
        };
        if file.seek(SeekFrom::Start(offset)).is_err() {
            continue;
        }
        let mut chunk = String::new();
        let Ok(read) = file.read_to_string(&mut chunk) else {
            // Partial UTF-8 at the tail; retry on the next tick.
            continue;
        };
        offset += read as u64;

        partial_line.push_str(&chunk);
        let mut aggregator = aggregator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while let Some(newline) = partial_line.find('\n') {
            let line: String = partial_line.drain(..=newline).collect();
            aggregator.ingest_line(&line);
        }
    }
}

#[must_use]
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
