//! Thin binary entry point; the router, aggregator and tail logic live in
//! the library so integration tests can drive them directly.

use std::path::PathBuf;

use clap::Parser;

use claw_dashboard::{build_router, tail_events, AppState};

#[derive(Debug, Parser)]
#[command(
    name = "claw-dashboard",
    about = "Live token usage and multi-agent workflow dashboard for claw"
)]
struct Args {
    /// JSONL telemetry events file to watch (written by claw when
    /// CLAW_DASHBOARD_EVENTS points at the same path).
    #[arg(
        long,
        env = "CLAW_DASHBOARD_EVENTS",
        default_value = ".claw/telemetry/events.jsonl"
    )]
    events: PathBuf,

    /// Port for the local HTTP server.
    #[arg(long, short, default_value_t = 4110)]
    port: u16,

    /// Interface to bind. Keep it loopback unless you know what you expose.
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Truncate the events file on startup, discarding history from
    /// previous sessions (the file grows unbounded otherwise).
    #[arg(long)]
    truncate: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    if args.truncate && args.events.exists() {
        std::fs::File::create(&args.events)?;
        println!("truncated events file: {}", args.events.display());
    }
    let state = AppState::new();

    tokio::spawn(tail_events(args.events.clone(), state.aggregator.clone()));

    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind((args.host.as_str(), args.port)).await?;
    let address = listener.local_addr()?;
    println!("claw-dashboard listening on http://{address}");
    println!("watching events file: {}", args.events.display());
    println!(
        "run claw with CLAW_DASHBOARD_EVENTS={} to feed it",
        args.events.display()
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
