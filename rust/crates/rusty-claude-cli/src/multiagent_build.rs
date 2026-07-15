//! `/web` and `/app` REPL commands: launch an autonomous multi-agent build
//! (Director → Subdirector → Architects → parallel developer waves →
//! Supervisor → QA → Docs) without leaving the claw session.

use std::path::PathBuf;
use std::time::Duration;

use claw_multiagent::{run, BuildMode, ModelCatalog, ProjectKind, RunOptions};

/// Restores the REPL's working directory and `CLAWD_AGENT_STORE` when
/// dropped, so a build that errors, `?`s, or panics after mutating them
/// never strands the interactive session in the generated project.
struct ReplEnvGuard {
    cwd: PathBuf,
    store: Option<std::ffi::OsString>,
}

impl ReplEnvGuard {
    fn capture(cwd: PathBuf) -> Self {
        Self {
            cwd,
            store: std::env::var_os("CLAWD_AGENT_STORE"),
        }
    }
}

impl Drop for ReplEnvGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.cwd);
        match &self.store {
            Some(store) => std::env::set_var("CLAWD_AGENT_STORE", store),
            None => std::env::remove_var("CLAWD_AGENT_STORE"),
        }
    }
}

const USAGE: &str = "Usage: /web <prompt> [--dry-run] [--approve] [--parallel N] \
[--output <dir>] [--resume] [--max-cost-usd X] [--build-cmd <cmd|off>] [--no-scaffold] \
[--timeout-secs N] [--archetype <nombre>]\n\
                            /app <prompt> [same options]\n\
                            /improve <prompt> [same options; opera sobre el proyecto actual]\n\
                     Tips: --approve pauses after planning for a go/no-go;\n\
                     --dry-run stops after planning entirely.\n\
                     --max-cost-usd is OFF by default (subscription accounts).\n\
                     --archetype fuerza la dirección de arte (landing, ecommerce, dashboard, \
saas, content, fintech, social, booking, general) en vez de detectarla del plan.\n\
                     Env: CLAW_MA_{SIMPLE,MEDIUM,COMPLEX,DIRECTOR,SUPERVISOR}_MODEL \
overrides per-role models; CLAW_PERF_BUDGET_KB, CLAW_IMG_BUDGET_KB and \
CLAW_SMOKE_TIMEOUT_SECS tune the gates.";

/// Extracts the value of `--archetype`, accepting both `--archetype <n>`
/// (advances `index` past the value) and `--archetype=<n>`.
pub(crate) fn take_archetype_value<'a>(
    tokens: &[&'a str],
    index: &mut usize,
) -> Result<&'a str, String> {
    const MISSING: &str = "--archetype espera un nombre (landing, ecommerce, dashboard, \
                           saas, content, fintech, social, booking, general)";
    if let Some(value) = tokens[*index].strip_prefix("--archetype=") {
        if value.is_empty() {
            return Err(MISSING.to_string());
        }
        return Ok(value);
    }
    *index += 1;
    tokens
        .get(*index)
        .copied()
        .ok_or_else(|| MISSING.to_string())
}

/// Developer-slot bounds: 0 would deadlock the scheduler and beyond 16 the
/// per-agent processes contend for CPU/IO without building any faster.
pub(crate) fn clamp_parallel(requested: usize) -> (usize, Option<String>) {
    match requested {
        0 => (1, Some("--parallel 0 no es válido; usando 1".to_string())),
        1..=16 => (requested, None),
        _ => (
            16,
            Some(format!(
                "--parallel {requested} supera el máximo razonable; usando 16"
            )),
        ),
    }
}

/// Parses the slash-command arguments and runs the build. The working
/// directory and agent store are restored afterwards so the REPL session
/// continues where it was.
pub(crate) fn run_multiagent_build(
    kind: ProjectKind,
    mode: BuildMode,
    raw: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let raw = raw.unwrap_or("").trim();
    if raw.is_empty() {
        println!("{USAGE}");
        return Ok(());
    }

    let mut prompt_words: Vec<&str> = Vec::new();
    let mut dry_run = false;
    let mut resume = false;
    let mut scaffold = true;
    let mut approve = false;
    let mut parallel = 4_usize;
    // Improve works on the project you are already in; greenfield builds
    // scaffold into a fresh subdirectory.
    let mut output = if mode.is_improve() {
        PathBuf::from(".")
    } else {
        PathBuf::from("./multiagent-project")
    };
    let mut max_cost_usd: Option<f64> = None;
    let mut build_cmd: Option<String> = None;
    let mut agent_timeout_secs = 1800_u64;
    let mut archetype: Option<claw_multiagent::DesignArchetype> = None;
    let tokens: Vec<&str> = raw.split_whitespace().collect();
    let mut index = 0;
    while index < tokens.len() {
        match tokens[index] {
            "--dry-run" => dry_run = true,
            "--resume" => resume = true,
            "--no-scaffold" => scaffold = false,
            "--approve" => approve = true,
            "--parallel" => {
                index += 1;
                parallel = tokens
                    .get(index)
                    .and_then(|value| value.parse().ok())
                    .ok_or("--parallel expects a number")?;
            }
            "--output" => {
                index += 1;
                output = PathBuf::from(*tokens.get(index).ok_or("--output expects a directory")?);
            }
            "--max-cost-usd" => {
                index += 1;
                max_cost_usd = Some(
                    tokens
                        .get(index)
                        .and_then(|value| value.parse().ok())
                        .ok_or("--max-cost-usd expects a number (USD)")?,
                );
            }
            "--build-cmd" => {
                index += 1;
                build_cmd = Some(
                    (*tokens
                        .get(index)
                        .ok_or("--build-cmd expects a command or 'off'")?)
                    .to_string(),
                );
            }
            "--timeout-secs" => {
                index += 1;
                agent_timeout_secs = tokens
                    .get(index)
                    .and_then(|value| value.parse().ok())
                    .filter(|value| *value > 0)
                    .ok_or("--timeout-secs expects a positive number of seconds")?;
            }
            word if word == "--archetype" || word.starts_with("--archetype=") => {
                let name = take_archetype_value(&tokens, &mut index)?;
                archetype = Some(claw_multiagent::parse_archetype(name)?);
            }
            word => prompt_words.push(word),
        }
        index += 1;
    }
    let prompt = prompt_words.join(" ");
    if prompt.is_empty() {
        println!("{USAGE}");
        return Ok(());
    }
    let (parallel, parallel_warning) = clamp_parallel(parallel);
    if let Some(warning) = parallel_warning {
        println!("[multiagent] {warning}");
    }

    let original_cwd = std::env::current_dir()?;
    std::fs::create_dir_all(&output)?;
    let project_dir = output.canonicalize()?;
    let catalog = ModelCatalog::load(&original_cwd);

    println!(
        "[multiagent] {} → {} (paralelo {}, modelos {}/{}/{})",
        kind.as_str(),
        project_dir.display(),
        parallel,
        catalog.simple,
        catalog.medium,
        catalog.complex
    );

    // The build mutates process cwd + CLAWD_AGENT_STORE; this guard restores
    // both when it drops — on the happy path, on `?`, AND on a panic inside
    // run(). Without it, an error after set_current_dir left the REPL in the
    // generated project's directory.
    let _env_guard = ReplEnvGuard::capture(original_cwd);

    let result = run(&RunOptions {
        kind,
        mode,
        prompt,
        project_dir,
        catalog,
        parallel,
        dry_run,
        agent_timeout: Duration::from_secs(agent_timeout_secs),
        resume,
        max_cost_usd,
        build_command: build_cmd,
        scaffold,
        approve,
        archetype,
    });

    match result {
        Ok(summary) => {
            println!("[multiagent] visión: {}", summary.plan.vision);
            println!(
                "[multiagent] stack: {} · tareas: {} · niveles: {}",
                summary.plan.stack.kind, summary.tasks, summary.waves
            );
            if dry_run {
                println!(
                    "[multiagent] dry-run: plan guardado en docs/plan.json y docs/backlog.json"
                );
            } else {
                println!(
                    "[multiagent] completadas: {} · fallidas: {} · issues de supervisión: {}",
                    summary.completed, summary.failed, summary.supervision_issues
                );
                if !summary.failed_task_ids.is_empty() {
                    println!(
                        "[multiagent] tareas fallidas: {}",
                        summary.failed_task_ids.join(", ")
                    );
                }
                if !summary.blocked_task_ids.is_empty() {
                    println!(
                        "[multiagent] bloqueadas por dependencias fallidas: {}",
                        summary.blocked_task_ids.join(", ")
                    );
                }
                if let Some(cost) = summary.cost_usd {
                    println!("[multiagent] coste estimado: {cost:.2} USD");
                }
                if !summary.role_spend.is_empty() {
                    let rows: Vec<String> = summary
                        .role_spend
                        .iter()
                        .map(|(role, usd)| format!("{role} {usd:.2} USD"))
                        .collect();
                    println!("[multiagent] gasto por rol: {}", rows.join(" · "));
                }
            }
            if let Some(branch) = &summary.improve_branch {
                let base = summary.base_branch.as_deref().unwrap_or("<tu-rama>");
                println!("[multiagent] rama de trabajo: {branch}");
                println!("[multiagent]   revisa:   git diff {base}...{branch}");
                println!("[multiagent]   integra:  git checkout {base} && git merge {branch}");
                println!("[multiagent]   descarta: git checkout {base} && git branch -D {branch}");
            }
            Ok(())
        }
        Err(error) => {
            println!("[multiagent] error: {error}");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{clamp_parallel, take_archetype_value};
    use claw_multiagent::{parse_archetype, DesignArchetype};

    #[test]
    fn archetype_flag_accepts_both_token_forms() {
        // Two-token form: `--archetype dashboard` (index advances past it).
        let tokens = ["--archetype", "dashboard", "resto"];
        let mut index = 0;
        assert_eq!(take_archetype_value(&tokens, &mut index), Ok("dashboard"));
        assert_eq!(index, 1, "value token consumed");

        // Inline form: `--archetype=fintech` (index untouched).
        let tokens = ["--archetype=fintech"];
        let mut index = 0;
        assert_eq!(take_archetype_value(&tokens, &mut index), Ok("fintech"));
        assert_eq!(index, 0);

        // Missing values fail with the name list, never silently.
        for tokens in [vec!["--archetype"], vec!["--archetype="]] {
            let mut index = 0;
            let error = take_archetype_value(&tokens, &mut index).unwrap_err();
            assert!(error.contains("landing"), "{error}");
            assert!(error.contains("general"), "{error}");
        }
    }

    #[test]
    fn archetype_names_resolve_case_insensitively_with_clear_errors() {
        // The CLI value goes straight to claw_multiagent::parse_archetype.
        assert_eq!(parse_archetype("Dashboard"), Ok(DesignArchetype::Dashboard));
        assert_eq!(parse_archetype("BOOKING"), Ok(DesignArchetype::Booking));
        let error = parse_archetype("blog").unwrap_err();
        assert!(error.contains("blog"), "{error}");
        assert!(error.contains("válidos"), "{error}");
        for name in [
            "landing",
            "ecommerce",
            "dashboard",
            "saas",
            "content",
            "fintech",
            "social",
            "booking",
            "general",
        ] {
            assert!(error.contains(name), "error must list `{name}`: {error}");
        }
    }

    #[test]
    fn clamp_parallel_bounds_and_warns() {
        assert_eq!(clamp_parallel(4), (4, None));
        assert_eq!(clamp_parallel(1).0, 1);
        assert_eq!(clamp_parallel(16).0, 16);

        let (fixed, warning) = clamp_parallel(0);
        assert_eq!(fixed, 1);
        assert!(warning.is_some());

        let (capped, warning) = clamp_parallel(64);
        assert_eq!(capped, 16);
        assert!(warning.unwrap().contains("16"));
    }
}
