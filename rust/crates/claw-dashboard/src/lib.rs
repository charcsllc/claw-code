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
/// the file not existing yet (waits), truncation, and rename-replacement
/// (resets and re-reads).
pub async fn tail_events(path: PathBuf, aggregator: Arc<Mutex<Aggregator>>) {
    let mut offset: u64 = 0;
    let mut partial: Vec<u8> = Vec::new();
    #[cfg(unix)]
    let mut file_identity: Option<(u64, u64)> = None;
    let mut interval = tokio::time::interval(TAIL_INTERVAL);

    loop {
        interval.tick().await;

        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        let mut reset = false;
        #[cfg(unix)]
        {
            // Length-only detection misses a rename-replace with an
            // equal-or-longer file; the (device, inode) pair does not.
            use std::os::unix::fs::MetadataExt;
            let identity = (metadata.dev(), metadata.ino());
            if file_identity != Some(identity) {
                reset = file_identity.is_some();
                file_identity = Some(identity);
            }
        }
        let length = metadata.len();
        if length < offset || reset {
            // Truncated or replaced: start over.
            offset = 0;
            partial.clear();
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
        // Raw bytes, not `read_to_string`: one invalid UTF-8 byte (e.g. a
        // writer killed mid-append) would otherwise fail the read forever
        // without ever advancing the offset, silently freezing the tail.
        let mut chunk = Vec::new();
        let Ok(read) = file.read_to_end(&mut chunk) else {
            continue;
        };
        if read == 0 {
            continue;
        }
        offset += read as u64;
        partial.extend_from_slice(&chunk);

        // Consume only up to the last newline: the remainder may be a line
        // still being appended (possibly splitting a multi-byte character),
        // so it stays buffered for the next tick.
        let Some(last_newline) = partial.iter().rposition(|&byte| byte == b'\n') else {
            continue;
        };
        let complete: Vec<u8> = partial.drain(..=last_newline).collect();
        let text = String::from_utf8_lossy(&complete);
        // Single pass over the drained buffer (the previous per-line
        // `drain(..=newline)` memmoved the tail once per line — quadratic on
        // large histories) and one lock scope per tick.
        let mut aggregator = aggregator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for line in text.lines() {
            aggregator.ingest_line(line);
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
