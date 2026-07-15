//! Integration tests: serve the real router on an ephemeral port, feed the
//! tail from a temp JSONL file, and drive it over plain HTTP.

use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use claw_dashboard::{build_router, tail_events, AppState};
use serde_json::{json, Value};

fn unique_temp_file(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("claw-dashboard-{tag}-{nanos}.jsonl"))
}

fn trace_line(session_id: &str, sequence: u64, name: &str, attributes: Value) -> String {
    json!({
        "type": "session_trace",
        "session_id": session_id,
        "sequence": sequence,
        "name": name,
        "timestamp_ms": 1_000 + sequence,
        "attributes": attributes,
    })
    .to_string()
}

fn usage_line(session_id: &str, sequence: u64, input: u64, output: u64) -> String {
    trace_line(
        session_id,
        sequence,
        "analytics",
        json!({
            "namespace": "api",
            "action": "message_usage",
            "model": "claude-sonnet-4-6",
            "input_tokens": input,
            "output_tokens": output,
            "estimated_cost_usd_value": 0.0125,
        }),
    )
}

/// Starts the dashboard on an ephemeral port and returns its address.
async fn serve(events: PathBuf) -> std::net::SocketAddr {
    let state = AppState::new();
    tokio::spawn(tail_events(events, state.aggregator.clone()));
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("ephemeral port should bind");
    let address = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server runs");
    });
    address
}

/// Minimal HTTP GET over a raw socket (no client dependency); returns the
/// body after the header terminator. `read_limit` bounds streaming reads.
fn http_get(address: std::net::SocketAddr, path: &str, read_limit: usize) -> String {
    let mut stream = TcpStream::connect(address).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("read timeout");
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .expect("request written");
    let mut raw = Vec::new();
    let mut buffer = [0_u8; 4096];
    while raw.len() < read_limit {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => raw.extend_from_slice(&buffer[..read]),
            Err(_) => break, // timeout: enough data for streaming asserts
        }
    }
    let text = String::from_utf8_lossy(&raw).to_string();
    match text.split_once("\r\n\r\n") {
        Some((_headers, body)) => body.to_string(),
        None => text,
    }
}

/// Polls `/api/state` until the predicate passes or the deadline expires.
async fn wait_for_state(
    address: std::net::SocketAddr,
    predicate: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let body = http_get(address, "/api/state", usize::MAX);
        // Chunked responses may wrap the JSON payload; extract the braces.
        let json_body = body
            .find('{')
            .and_then(|start| body.rfind('}').map(|end| &body[start..=end]))
            .unwrap_or(&body);
        if let Ok(state) = serde_json::from_str::<Value>(json_body) {
            if predicate(&state) {
                return state;
            }
        }
        assert!(
            Instant::now() < deadline,
            "state did not converge in time; last body: {body}"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ui_and_state_endpoints_serve_live_data() {
    let events = unique_temp_file("state");
    std::fs::write(
        &events,
        format!(
            "{}\n{}\n",
            trace_line("session-a", 0, "turn_started", json!({})),
            usage_line("session-a", 1, 1200, 300),
        ),
    )
    .expect("events file written");

    let address = serve(events.clone()).await;

    let html = http_get(address, "/", usize::MAX);
    assert!(html.contains("claw"), "UI should be served: {html:.>60}");

    let state = wait_for_state(address, |state| {
        state["totals"]["input_tokens"].as_u64() == Some(1200)
    })
    .await;
    assert_eq!(state["totals"]["output_tokens"].as_u64(), Some(300));
    assert_eq!(state["agents"][0]["status"], "running");

    // Live append: totals should move without restarting anything.
    let mut line = usage_line("session-a", 2, 100, 50);
    line.push('\n');
    std::fs::OpenOptions::new()
        .append(true)
        .open(&events)
        .and_then(|mut file| file.write_all(line.as_bytes()))
        .expect("append");
    wait_for_state(address, |state| {
        state["totals"]["input_tokens"].as_u64() == Some(1300)
    })
    .await;

    // Truncation resets the aggregator.
    std::fs::write(&events, "").expect("truncate");
    wait_for_state(address, |state| {
        state["agents"].as_array().is_some_and(Vec::is_empty)
    })
    .await;

    let _ = std::fs::remove_file(events);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sse_stream_pushes_snapshot_frames() {
    let events = unique_temp_file("sse");
    std::fs::write(
        &events,
        format!("{}\n", usage_line("session-sse", 0, 700, 70)),
    )
    .expect("events file written");

    let address = serve(events.clone()).await;
    // Give the tail one interval to ingest before subscribing.
    tokio::time::sleep(Duration::from_millis(600)).await;

    let body = tokio::task::spawn_blocking(move || http_get(address, "/api/stream", 16_384))
        .await
        .expect("blocking read");
    assert!(
        body.contains("data:"),
        "SSE stream should push a data frame: {body:.>80}"
    );
    assert!(
        body.contains("\"input_tokens\":700"),
        "snapshot frame should carry ingested usage: {body:.>200}"
    );

    let _ = std::fs::remove_file(events);
}
