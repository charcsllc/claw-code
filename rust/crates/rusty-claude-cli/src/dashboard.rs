//! `claw --dashboard` support: spawns the local `claw-dashboard` web UI and
//! wires this process's telemetry to the shared JSONL events file.
//!
//! Everything is local — claw appends telemetry to a file on disk and the
//! dashboard tails it; nothing leaves the machine.

use std::env;
use std::fs;
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use api::{JsonlTelemetrySink, SessionTracer};

const DASHBOARD_DEFAULT_PORT: u16 = 4110;
const DASHBOARD_DEFAULT_EVENTS: &str = ".claw/telemetry/events.jsonl";

/// Handles `claw --dashboard`: ensures `CLAW_DASHBOARD_EVENTS` points at a
/// shared events file, spawns `claw-dashboard` (unless one is already
/// listening), and opens the browser. Failures degrade to a stderr warning
/// without blocking the session.
pub(crate) fn setup_dashboard() {
    let events = env::var("CLAW_DASHBOARD_EVENTS")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DASHBOARD_DEFAULT_EVENTS.to_string());
    if let Some(parent) = Path::new(&events).parent() {
        let _ = fs::create_dir_all(parent);
    }
    // Activate telemetry for this claw process.
    env::set_var("CLAW_DASHBOARD_EVENTS", &events);

    let url = format!("http://127.0.0.1:{DASHBOARD_DEFAULT_PORT}");
    let already_running =
        TcpListener::bind(("127.0.0.1", DASHBOARD_DEFAULT_PORT)).map_or(true, |probe| {
            drop(probe);
            false
        });
    if already_running {
        eprintln!("claw-dashboard already listening on {url}");
    } else {
        // The dashboard binary is expected next to the claw binary (both
        // live in the same cargo target/bin directory when installed).
        let sibling = env::current_exe().ok().and_then(|exe| {
            let candidate = exe.with_file_name(dashboard_binary_name());
            candidate.exists().then_some(candidate)
        });
        match sibling {
            Some(binary) => {
                match Command::new(binary)
                    .args(["--events", &events])
                    .args(["--port", &DASHBOARD_DEFAULT_PORT.to_string()])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                {
                    Ok(_) => eprintln!("claw-dashboard started on {url}"),
                    Err(error) => {
                        eprintln!("warning: could not start claw-dashboard: {error}");
                        return;
                    }
                }
            }
            None => {
                eprintln!(
                    "warning: claw-dashboard binary not found next to claw.\n\
                     Build it with `cargo build -p claw-dashboard` or run it manually:\n\
                     claw-dashboard --events {events}"
                );
                return;
            }
        }
    }
    open_in_browser(&url);
}

fn dashboard_binary_name() -> &'static str {
    if cfg!(windows) {
        "claw-dashboard.exe"
    } else {
        "claw-dashboard"
    }
}

fn open_in_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    let _ = Command::new(opener)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Builds a session tracer writing JSONL telemetry to the path in
/// `CLAW_DASHBOARD_EVENTS`, so `claw-dashboard` can render live token and
/// agent activity. Local file only — nothing leaves the machine. Returns
/// `None` when the variable is unset, empty, or the file cannot be opened.
pub(crate) fn dashboard_session_tracer(session_id: &str) -> Option<SessionTracer> {
    let path = env::var("CLAW_DASHBOARD_EVENTS").ok()?;
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    match JsonlTelemetrySink::new(path) {
        Ok(sink) => Some(SessionTracer::new(session_id, Arc::new(sink))),
        Err(error) => {
            eprintln!("warning: CLAW_DASHBOARD_EVENTS ({path}) could not be opened: {error}");
            None
        }
    }
}
