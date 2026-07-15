//! CLI: `claw-multiagent web|app "<prompt>"` builds a complete project
//! autonomously through the specialized agent hierarchy.

use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};

use claw_multiagent::{run, BuildMode, ModelCatalog, ProjectKind, RunOptions};

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
    /// Improve an EXISTING project: add a feature or fix, in place.
    Improve(CommonArgs),
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

    /// Skip the deterministic project scaffold (create-vite / cargo init)
    /// and let the agents generate every file, like before.
    #[arg(long)]
    no_scaffold: bool,

    /// Pause after planning: show the plan summary and ask for
    /// confirmation before spending developer runs.
    #[arg(long)]
    approve: bool,
}

const DASHBOARD_PORT: u16 = 4110;

/// True when the port answers `GET /api/state` like a claw-dashboard (an
/// unrelated listener must not be reported as "dashboard ya activo").
fn dashboard_alive(port: u16) -> bool {
    use std::io::{Read as _, Write as _};
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut stream) =
        std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_millis(400))
    else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
    let request =
        format!("GET /api/state HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut buffer = Vec::new();
    let mut limited = stream.take(4096);
    let _ = limited.read_to_end(&mut buffer);
    let response = String::from_utf8_lossy(&buffer);
    response.starts_with("HTTP/1.1 200") && response.contains("generated_ms")
}

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

    // An occupied port is not necessarily OUR dashboard: probe /api/state
    // and only reuse a listener that answers like one; otherwise walk up
    // the port range for a free slot.
    let mut chosen: Option<(u16, bool)> = None;
    for port in DASHBOARD_PORT..DASHBOARD_PORT + 10 {
        if dashboard_alive(port) {
            chosen = Some((port, true));
            break;
        }
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
            chosen = Some((port, false));
            break;
        }
        eprintln!(
            "[multiagent] aviso: el puerto {port} lo usa otro proceso (no dashboard); probando {}",
            port + 1
        );
    }
    let Some((port, reuse)) = chosen else {
        eprintln!(
            "[multiagent] aviso: sin puerto libre en {DASHBOARD_PORT}..{} para el dashboard",
            DASHBOARD_PORT + 9
        );
        return;
    };
    let url = format!("http://127.0.0.1:{port}");
    if reuse {
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
                    .args(["--port", &port.to_string()])
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
    let (kind, mode, common) = match args.aplicativo {
        Aplicativo::Web(common) => (ProjectKind::Web, BuildMode::Greenfield, common),
        Aplicativo::App(common) => (ProjectKind::App, BuildMode::Greenfield, common),
        // Improve keeps the Web role set (architects/UX), but runs the
        // existing-project pipeline; the stack is detected from the repo.
        Aplicativo::Improve(common) => (ProjectKind::Web, BuildMode::Improve, common),
    };

    if let Err(error) = execute(kind, mode, common) {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn execute(kind: ProjectKind, mode: BuildMode, common: CommonArgs) -> Result<(), String> {
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

    // 0 developer slots would deadlock the scheduler; beyond 16 the agent
    // processes just contend for CPU/IO.
    let parallel = common.parallel.clamp(1, 16);
    if parallel != common.parallel {
        eprintln!(
            "[multiagent] aviso: --parallel {} fuera de rango; usando {parallel}",
            common.parallel
        );
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
        mode,
        prompt: common.prompt,
        project_dir,
        catalog,
        parallel,
        dry_run: common.dry_run,
        agent_timeout: Duration::from_secs(common.agent_timeout_secs),
        resume: common.resume,
        max_cost_usd: common.max_cost_usd,
        build_command: common.build_cmd,
        scaffold: !common.no_scaffold,
        approve: common.approve,
        // Archetype forcing is a REPL (/web, /app) affordance; the
        // standalone CLI keeps keyword detection over the plan.
        archetype: None,
    })?;

    println!("\n[multiagent] === resumen ===");
    println!("  visión:        {}", summary.plan.vision);
    println!("  stack:         {}", summary.plan.stack.kind);
    println!("  tareas:        {}", summary.tasks);
    println!("  niveles de dependencia: {}", summary.waves);
    if common.dry_run {
        println!("  (dry-run: planificación completada, ejecución omitida)");
    } else {
        println!("  completadas:   {}", summary.completed);
        println!("  fallidas:      {}", summary.failed);
        if !summary.failed_task_ids.is_empty() {
            println!("    → {}", summary.failed_task_ids.join(", "));
        }
        if !summary.blocked_task_ids.is_empty() {
            println!(
                "  bloqueadas por dependencias fallidas: {} ({})",
                summary.blocked_task_ids.len(),
                summary.blocked_task_ids.join(", ")
            );
        }
        println!("  issues de supervisión: {}", summary.supervision_issues);
        if summary.resumed_tasks > 0 {
            println!("  reanudadas (omitidas): {}", summary.resumed_tasks);
        }
        if let Some(cost) = summary.cost_usd {
            println!("  coste estimado: {cost:.2} USD");
        }
        if summary.budget_aborted {
            println!("  ⚠ abortado por presupuesto (--max-cost-usd)");
        }
        if summary.user_aborted {
            println!("  ⚠ abortado por usuario (Ctrl+C) — reanuda con --resume");
        }
    }
    if let Some(branch) = &summary.improve_branch {
        let base = summary.base_branch.as_deref().unwrap_or("<tu-rama>");
        println!("  rama de trabajo: {branch}");
        println!("    revisa:   git diff {base}...{branch}");
        println!("    integra:  git checkout {base} && git merge {branch}");
        println!("    descarta: git checkout {base} && git branch -D {branch}");
    }
    Ok(())
}
