//! End-to-end planning pipeline against the mock Anthropic service.
//!
//! Exercises the real `claw-multiagent` binary in `--dry-run` mode:
//! Director → Architects (parallel) → Subdirector, all through the actual
//! Agent-tool runtime and API client, answered by content routes registered
//! on [`MockAnthropicService`]. The developer waves and gates are outside
//! `--dry-run` by design — they need npm/git side effects, not more API
//! coverage.
//!
//! Environment is injected EXCLUSIVELY into the child process (the
//! workspace runs tests in parallel; `std::env::set_var` here would race
//! every other test in this binary and beyond).

use std::path::PathBuf;
use std::process::Command;

use mock_anthropic_service::{MockAnthropicService, ROUTED_SCENARIO_PREFIX};

/// Distinct substrings of each planning prompt (see orchestrator.rs): the
/// Director's instruction, the architects' shared instruction, and the
/// Subdirector's backlog instruction.
const DIRECTOR_PATTERN: &str = "Produce the complete product plan";
const ARCHITECT_PATTERN: &str = "Deliver your complete design document";
const SUBDIRECTOR_PATTERN: &str = "parallelizable TaskSpecs";

const DIRECTOR_RESPONSE: &str = r#"Plan listo.
```json
{
  "vision": "Tienda online de electrónica con catálogo y carrito",
  "scope": ["catálogo de productos", "carrito de compra"],
  "stack": {"kind": "simple", "frontend": ["react"], "justification": "demo estática"},
  "epics": [{"name": "Catálogo", "stories": []}],
  "milestones": ["mvp"],
  "risks": [],
  "open_questions": []
}
```"#;

const ARCHITECT_RESPONSE: &str =
    "## Documento de arquitectura\n\nComponentes de catálogo y carrito sobre tokens del \
     design system; sin backend.";

const SUBDIRECTOR_RESPONSE: &str = r#"Backlog listo.
```json
{"tasks": [
  {"id": "T1", "module": "catalog", "functional_objective": "Página de catálogo",
   "technical_objective": "Grid de productos", "files_to_create": ["src/catalog.ts"],
   "priority": 1, "complexity": "simple", "wave": 0, "depends_on": []},
  {"id": "T2", "module": "cart", "functional_objective": "Carrito de compra",
   "technical_objective": "Estado del carrito", "files_to_create": ["src/cart.ts"],
   "priority": 2, "complexity": "simple", "wave": 1, "depends_on": ["T1"]}
]}
```"#;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "multiagent-e2e-{tag}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

#[test]
fn dry_run_planning_pipeline_completes_against_the_mock() {
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let server = runtime
        .block_on(MockAnthropicService::spawn())
        .expect("mock service");
    runtime.block_on(async {
        server.route(DIRECTOR_PATTERN, DIRECTOR_RESPONSE).await;
        server.route(ARCHITECT_PATTERN, ARCHITECT_RESPONSE).await;
        server
            .route(SUBDIRECTOR_PATTERN, SUBDIRECTOR_RESPONSE)
            .await;
    });

    let root = temp_dir("root");
    let project = root.join("project");
    let config_home = root.join("config"); // empty: no saved OAuth leaks in
    std::fs::create_dir_all(&config_home).expect("config home");

    let output = Command::new(env!("CARGO_BIN_EXE_claw-multiagent"))
        .args([
            "web",
            "una tienda online de electrónica",
            "--dry-run",
            "--output",
        ])
        .arg(&project)
        .args(["--agent-timeout-secs", "120"])
        // cwd controls `.claw/multiagent.json` lookup: keep it hermetic.
        .current_dir(&root)
        .env("ANTHROPIC_BASE_URL", server.base_url())
        .env("ANTHROPIC_API_KEY", "test-key-multiagent")
        .env("CLAW_CONFIG_HOME", &config_home)
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env_remove("CLAW_DASHBOARD_EVENTS")
        .env_remove("CLAWD_AGENT_STORE")
        .env_remove("CLAW_MA_SIMPLE_MODEL")
        .env_remove("CLAW_MA_MEDIUM_MODEL")
        .env_remove("CLAW_MA_COMPLEX_MODEL")
        .env_remove("CLAW_MA_DIRECTOR_MODEL")
        .env_remove("CLAW_MA_SUPERVISOR_MODEL")
        .output()
        .expect("claw-multiagent binary runs");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "dry-run failed\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );

    // The planning artifacts were produced from the routed responses.
    let plan_raw =
        std::fs::read_to_string(project.join("docs/plan.json")).expect("plan.json written");
    assert!(
        plan_raw.contains("Tienda online de electrónica"),
        "{plan_raw}"
    );
    let backlog_raw =
        std::fs::read_to_string(project.join("docs/backlog.json")).expect("backlog.json written");
    assert!(backlog_raw.contains("\"T1\""), "{backlog_raw}");
    assert!(backlog_raw.contains("\"T2\""), "{backlog_raw}");
    assert!(
        project.join("docs/director-report.md").exists(),
        "director report saved"
    );

    // The summary reflects the parsed plan (2 tasks in 2 dependency waves).
    assert!(stdout.contains("tareas:        2"), "{stdout}");
    assert!(stdout.contains("dry-run"), "{stdout}");

    // Every pipeline phase actually reached the mock: one Director call,
    // the architect fan-out (software/UX/devops/frontend for a simple web
    // stack), and one Subdirector call — all attributed to their routes.
    // The API client also preflights each call via /v1/messages/count_tokens
    // with the same body; count only the real message calls.
    let captured = runtime.block_on(server.captured_requests());
    let count = |pattern: &str| {
        captured
            .iter()
            .filter(|request| request.path == "/v1/messages")
            .filter(|request| request.scenario == format!("{ROUTED_SCENARIO_PREFIX}{pattern}"))
            .count()
    };
    assert_eq!(count(DIRECTOR_PATTERN), 1, "director calls");
    assert_eq!(count(ARCHITECT_PATTERN), 4, "architect fan-out");
    assert_eq!(count(SUBDIRECTOR_PATTERN), 1, "subdirector calls");
    assert_eq!(
        captured
            .iter()
            .filter(|request| request.path == "/v1/messages")
            .count(),
        6,
        "no unexpected message traffic"
    );
    // Nothing fell through to the parity scenarios: every request (message
    // or count_tokens preflight) was answered by a content route.
    assert!(
        captured
            .iter()
            .all(|request| request.scenario.starts_with(ROUTED_SCENARIO_PREFIX)),
        "unrouted request captured: {:?}",
        captured
            .iter()
            .map(|request| request.scenario.clone())
            .collect::<Vec<_>>()
    );

    let _ = std::fs::remove_dir_all(&root);
}
