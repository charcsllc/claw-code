//! CLI: `claw-multiagent web|app "<prompt>"` builds a complete project
//! autonomously through the specialized agent hierarchy.

use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};

use claw_multiagent::{run, ModelCatalog, ProjectKind, RunOptions};

#[derive(Debug, Parser)]
#[command(
    name = "claw-multiagent",
    about = "Autonomous multi-agent platform: builds WEB and APP projects from one prompt"
)]
struct Args {
    #[command(subcommand)]
    aplicativo: Aplicativo,
}

#[derive(Debug, Subcommand)]
enum Aplicativo {
    /// Build a website (landing, ecommerce, SaaS, CRM, dashboard, ...).
    Web(CommonArgs),
    /// Build an application (Windows/Linux/macOS/Android/iOS/tablets).
    App(CommonArgs),
}

#[derive(Debug, clap::Args)]
struct CommonArgs {
    /// What to build, e.g. "un ecommerce para vender productos electrónicos".
    prompt: String,

    /// Directory where the project is generated (created if missing).
    #[arg(long, default_value = "./multiagent-project")]
    output: PathBuf,

    /// Max developer agents running in parallel inside a wave.
    #[arg(long, default_value_t = 4)]
    parallel: usize,

    /// Plan only: run Director + Subdirector, print the plan, stop.
    #[arg(long)]
    dry_run: bool,

    /// Per-agent timeout in seconds.
    #[arg(long, default_value_t = 1800)]
    agent_timeout_secs: u64,

    /// Model for simple tasks (overrides .claw/multiagent.json).
    #[arg(long)]
    simple_model: Option<String>,
    /// Model for medium tasks.
    #[arg(long)]
    medium_model: Option<String>,
    /// Model for complex tasks.
    #[arg(long)]
    complex_model: Option<String>,
    /// Model for Director/Subdirector/Architects.
    #[arg(long)]
    director_model: Option<String>,
    /// Model for Supervisor/Fixer (the spec demands a superior model).
    #[arg(long)]
    supervisor_model: Option<String>,

    /// Start claw-dashboard (or reuse a running one), point the telemetry
    /// events file at this build, and open the browser to watch every
    /// agent and the workflow phase live.
    #[arg(long)]
    dashboard: bool,

    /// Resume an interrupted build: reuse the saved plan/designs/backlog
    /// and skip tasks already completed.
    #[arg(long)]
    resume: bool,

    /// Cost ceiling in USD. DISABLED by default (subscription accounts
    /// don't bill per token); pass a value to enable enforcement — the
    /// build aborts cleanly between waves when spend exceeds it.
    /// Requires telemetry (--dashboard or CLAW_DASHBOARD_EVENTS).
    #[arg(long)]
    max_cost_usd: Option<f64>,

    /// Build-gate command run after each wave (auto-detected from
    /// package.json/Cargo.toml when omitted; pass "off" to disable).
    #[arg(long)]
    build_cmd: Option<String>,
}

const DASHBOARD_PORT: u16 = 4110;

/// Points `CLAW_DASHBOARD_EVENTS` at this build, spawns the sibling
/// `claw-dashboard` binary unless one is already listening, and opens the
/// browser. Failures degrade to a warning; the build proceeds either way.
fn launch_dashboard(project_dir: &std::path::Path) {
    let events = std::env::var("CLAW_DASHBOARD_EVENTS")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            project_dir
                .join(".multiagent")
                .join("events.jsonl")
                .display()
                .to_string()
        });
    if let Some(parent) = std::path::Path::new(&events).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::env::set_var("CLAW_DASHBOARD_EVENTS", &events);

    let url = format!("http://127.0.0.1:{DASHBOARD_PORT}");
    let already_running =
        std::net::TcpListener::bind(("127.0.0.1", DASHBOARD_PORT)).map_or(true, |probe| {
            drop(probe);
            false
        });
    if already_running {
        println!("[multiagent] dashboard ya activo en {url}");
    } else {
        let binary_name = if cfg!(windows) {
            "claw-dashboard.exe"
        } else {
            "claw-dashboard"
        };
        let sibling = std::env::current_exe().ok().and_then(|exe| {
            let candidate = exe.with_file_name(binary_name);
            candidate.exists().then_some(candidate)
        });
        match sibling {
            Some(binary) => {
                match std::process::Command::new(binary)
                    .args(["--events", &events])
                    .args(["--port", &DASHBOARD_PORT.to_string()])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                {
                    Ok(_) => println!("[multiagent] dashboard iniciado en {url}"),
                    Err(error) => {
                        eprintln!("[multiagent] aviso: no se pudo iniciar el dashboard: {error}");
                        return;
                    }
                }
            }
            None => {
                eprintln!(
                    "[multiagent] aviso: claw-dashboard no está junto a este binario; \
                     compílalo con `cargo build -p claw-dashboard`"
                );
                return;
            }
        }
    }
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(opener)
        .arg(&url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn main() {
    let args = Args::parse();
    let (kind, common) = match args.aplicativo {
        Aplicativo::Web(common) => (ProjectKind::Web, common),
        Aplicativo::App(common) => (ProjectKind::App, common),
    };

    if let Err(error) = execute(kind, common) {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn execute(kind: ProjectKind, common: CommonArgs) -> Result<(), String> {
    std::fs::create_dir_all(&common.output).map_err(|error| error.to_string())?;
    let project_dir = common
        .output
        .canonicalize()
        .map_err(|error| error.to_string())?;

    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let mut catalog = ModelCatalog::load(&cwd);
    if let Some(model) = common.simple_model {
        catalog.simple = model;
    }
    if let Some(model) = common.medium_model {
        catalog.medium = model;
    }
    if let Some(model) = common.complex_model {
        catalog.complex = model;
    }
    if let Some(model) = common.director_model {
        catalog.director = model;
    }
    if let Some(model) = common.supervisor_model {
        catalog.supervisor = model;
    }

    if common.dashboard {
        launch_dashboard(&project_dir);
    }

    println!("[multiagent] aplicativo: {}", kind.as_str());
    println!("[multiagent] proyecto:   {}", project_dir.display());
    println!(
        "[multiagent] modelos:    simple={} medium={} complex={} director={} supervisor={}",
        catalog.simple, catalog.medium, catalog.complex, catalog.director, catalog.supervisor
    );
    if std::env::var("CLAW_DASHBOARD_EVENTS").is_err() {
        println!(
            "[multiagent] tip: exporta CLAW_DASHBOARD_EVENTS y abre claw-dashboard \
             para ver los agentes en vivo"
        );
    }

    let summary = run(&RunOptions {
        kind,
        prompt: common.prompt,
        project_dir,
        catalog,
        parallel: common.parallel,
        dry_run: common.dry_run,
        agent_timeout: Duration::from_secs(common.agent_timeout_secs),
        resume: common.resume,
        max_cost_usd: common.max_cost_usd,
        build_command: common.build_cmd,
    })?;

    println!("\n[multiagent] === resumen ===");
    println!("  visión:        {}", summary.plan.vision);
    println!("  stack:         {}", summary.plan.stack.kind);
    println!("  tareas:        {}", summary.tasks);
    println!("  olas:          {}", summary.waves);
    if common.dry_run {
        println!("  (dry-run: planificación completada, ejecución omitida)");
    } else {
        println!("  completadas:   {}", summary.completed);
        println!("  fallidas:      {}", summary.failed);
        println!("  issues de supervisión: {}", summary.supervision_issues);
        if summary.resumed_tasks > 0 {
            println!("  reanudadas (omitidas): {}", summary.resumed_tasks);
        }
        if summary.budget_aborted {
            println!("  ⚠ abortado por presupuesto (--max-cost-usd)");
        }
        if summary.user_aborted {
            println!("  ⚠ abortado por usuario (Ctrl+C) — reanuda con --resume");
        }
    }
    Ok(())
}
