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
[--output <dir>] [--resume] [--max-cost X] [--build-cmd <cmd|off>] [--no-scaffold] \
[--timeout-secs N] [--archetype <nombre>]\n\
                            /app <prompt> [same options]\n\
                            /improve <prompt> [same options; opera sobre el proyecto actual]\n\
                     Tips: --approve pauses after planning for a go/no-go;\n\
                     --dry-run solo planifica: Director → Arquitectos → Subdirector escriben \
docs/plan.json y docs/backlog.json con el desglose de presupuesto, y se detiene ahí \
sin desarrollar nada.\n\
                     --resume reanuda un build interrumpido; sin él, /web y /app detectan \
el estado pendiente y ofrecen reanudar desde la última fase completada.\n\
                     --max-cost <usd> (alias --max-cost-usd) is OFF by default \
(subscription accounts); con tope, el build avisa al 80% y se corta en el siguiente \
checkpoint al superarlo (reanudable con --resume).\n\
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

/// Extracts the value of `--max-cost`, accepting both `--max-cost <usd>`
/// (advances `index` past the value) and `--max-cost=<usd>` — the same
/// conventions as `--archetype`. The value must be a positive number: a
/// typo silently parsed as "no ceiling" would defeat the whole flag.
pub(crate) fn take_max_cost_value(tokens: &[&str], index: &mut usize) -> Result<f64, String> {
    const MISSING: &str = "--max-cost espera un número positivo (USD), p. ej. --max-cost 5";
    let raw = if let Some(value) = tokens[*index].strip_prefix("--max-cost=") {
        value
    } else {
        *index += 1;
        tokens.get(*index).copied().ok_or(MISSING)?
    };
    raw.parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value > 0.0)
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

/// Everything the slash-command arguments configure. Extracted from the
/// REPL entry point so the flag parsing (`--dry-run`, `--resume`, …) is
/// unit-testable without launching a build.
#[derive(Debug, PartialEq)]
pub(crate) struct BuildArgs {
    pub(crate) prompt: String,
    pub(crate) dry_run: bool,
    pub(crate) resume: bool,
    pub(crate) scaffold: bool,
    pub(crate) approve: bool,
    pub(crate) parallel: usize,
    pub(crate) output: PathBuf,
    pub(crate) max_cost_usd: Option<f64>,
    pub(crate) build_cmd: Option<String>,
    pub(crate) agent_timeout_secs: u64,
    pub(crate) archetype: Option<claw_multiagent::DesignArchetype>,
}

/// Parses the `/web`/`/app`/`/improve` argument string. An empty prompt is
/// `Ok(None)` (the caller prints the usage text); malformed flag values are
/// errors.
pub(crate) fn parse_build_args(raw: &str, mode: BuildMode) -> Result<Option<BuildArgs>, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }

    let mut prompt_words: Vec<&str> = Vec::new();
    let mut args = BuildArgs {
        prompt: String::new(),
        dry_run: false,
        resume: false,
        scaffold: true,
        approve: false,
        parallel: 4,
        // Improve works on the project you are already in; greenfield
        // builds scaffold into a fresh subdirectory.
        output: if mode.is_improve() {
            PathBuf::from(".")
        } else {
            PathBuf::from("./multiagent-project")
        },
        max_cost_usd: None,
        build_cmd: None,
        agent_timeout_secs: 1800,
        archetype: None,
    };
    let tokens: Vec<&str> = raw.split_whitespace().collect();
    let mut index = 0;
    while index < tokens.len() {
        match tokens[index] {
            "--dry-run" => args.dry_run = true,
            "--resume" => args.resume = true,
            "--no-scaffold" => args.scaffold = false,
            "--approve" => args.approve = true,
            "--parallel" => {
                index += 1;
                args.parallel = tokens
                    .get(index)
                    .and_then(|value| value.parse().ok())
                    .ok_or("--parallel expects a number")?;
            }
            "--output" => {
                index += 1;
                args.output =
                    PathBuf::from(*tokens.get(index).ok_or("--output expects a directory")?);
            }
            "--max-cost-usd" => {
                index += 1;
                args.max_cost_usd = Some(
                    tokens
                        .get(index)
                        .and_then(|value| value.parse().ok())
                        .ok_or("--max-cost-usd expects a number (USD)")?,
                );
            }
            word if word == "--max-cost" || word.starts_with("--max-cost=") => {
                args.max_cost_usd = Some(take_max_cost_value(&tokens, &mut index)?);
            }
            "--build-cmd" => {
                index += 1;
                args.build_cmd = Some(
                    (*tokens
                        .get(index)
                        .ok_or("--build-cmd expects a command or 'off'")?)
                    .to_string(),
                );
            }
            "--timeout-secs" => {
                index += 1;
                args.agent_timeout_secs = tokens
                    .get(index)
                    .and_then(|value| value.parse().ok())
                    .filter(|value| *value > 0)
                    .ok_or("--timeout-secs expects a positive number of seconds")?;
            }
            word if word == "--archetype" || word.starts_with("--archetype=") => {
                let name = take_archetype_value(&tokens, &mut index)?;
                args.archetype = Some(claw_multiagent::parse_archetype(name)?);
            }
            word => prompt_words.push(word),
        }
        index += 1;
    }
    args.prompt = prompt_words.join(" ");
    if args.prompt.is_empty() {
        return Ok(None);
    }
    Ok(Some(args))
}

/// Affirmative answer to a [s/N] console question (Spanish or English).
pub(crate) fn parse_yes(answer: &str) -> bool {
    matches!(
        answer.trim().to_lowercase().as_str(),
        "s" | "si" | "sí" | "y" | "yes"
    )
}

/// When a greenfield `/web`/`/app` targets a project with a pending
/// (interrupted) build and `--resume` was not passed, offer to resume from
/// the last completed phase instead of re-planning everything. Reads the
/// answer from stdin; any error or non-affirmative answer starts fresh.
fn offer_resume(project_dir: &std::path::Path) -> bool {
    use std::io::Write as _;
    let Some(phase) = claw_multiagent::pending_resume_phase(project_dir) else {
        return false;
    };
    println!(
        "[multiagent] hay un build interrumpido en {} — última fase completada: {phase}",
        project_dir.display()
    );
    print!("[multiagent] ¿Reanudar desde ahí (plan/backlog y tareas hechas se reutilizan)? [s/N] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return false;
    }
    if parse_yes(&answer) {
        true
    } else {
        println!("[multiagent] empezando de cero (usa --resume si cambias de idea)");
        false
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
    let Some(args) = parse_build_args(raw.unwrap_or(""), mode)? else {
        println!("{USAGE}");
        return Ok(());
    };
    let BuildArgs {
        prompt,
        dry_run,
        mut resume,
        scaffold,
        approve,
        parallel,
        output,
        max_cost_usd,
        build_cmd,
        agent_timeout_secs,
        archetype,
    } = args;
    let (parallel, parallel_warning) = clamp_parallel(parallel);
    if let Some(warning) = parallel_warning {
        println!("[multiagent] {warning}");
    }

    let original_cwd = std::env::current_dir()?;
    std::fs::create_dir_all(&output)?;
    let project_dir = output.canonicalize()?;
    let catalog = ModelCatalog::load(&original_cwd);

    // Greenfield resume offer: an interrupted /web//app leaves its phase in
    // .multiagent/state.json; relaunching the same command should not
    // silently redo the (paid) planning that already succeeded.
    if !mode.is_improve() && !resume && offer_resume(&project_dir) {
        resume = true;
    }

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
                    "[multiagent] dry-run: planificación completada sin desarrollar; \
                     plan guardado en docs/plan.json y docs/backlog.json"
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
            }
            // Spend is reported for both modes: the dry-run breakdown IS the
            // deliverable that tells you what planning cost.
            if let Some(cost) = summary.cost_usd {
                println!("[multiagent] coste estimado: {cost:.2} USD");
            }
            if !summary.role_spend.is_empty() {
                let rows: Vec<String> = summary
                    .role_spend
                    .iter()
                    .map(|(role, usd)| format!("{role} {usd:.2} USD"))
                    .collect();
                println!(
                    "[multiagent] desglose de presupuesto por rol: {}",
                    rows.join(" · ")
                );
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
    use super::{
        clamp_parallel, parse_build_args, parse_yes, take_archetype_value, take_max_cost_value,
    };
    use claw_multiagent::{parse_archetype, BuildMode, DesignArchetype};
    use std::path::PathBuf;

    #[test]
    fn parse_accepts_dry_run_anywhere_and_keeps_the_prompt() {
        // Flag after the prompt.
        let args = parse_build_args("una tienda online --dry-run", BuildMode::Greenfield)
            .expect("parses")
            .expect("has prompt");
        assert!(args.dry_run);
        assert!(!args.resume);
        assert_eq!(args.prompt, "una tienda online");

        // Flag before/among the prompt words, combined with other flags.
        let args = parse_build_args(
            "--dry-run un dashboard --parallel 2 --archetype=fintech",
            BuildMode::Greenfield,
        )
        .expect("parses")
        .expect("has prompt");
        assert!(args.dry_run);
        assert_eq!(args.prompt, "un dashboard");
        assert_eq!(args.parallel, 2);
        assert_eq!(args.archetype, Some(DesignArchetype::Fintech));

        // Without the flag it stays off.
        let args = parse_build_args("una tienda", BuildMode::Greenfield)
            .expect("parses")
            .expect("has prompt");
        assert!(!args.dry_run);
    }

    #[test]
    fn parse_defaults_match_the_mode_and_flags_override() {
        let greenfield = parse_build_args("x", BuildMode::Greenfield)
            .expect("parses")
            .expect("has prompt");
        assert_eq!(greenfield.output, PathBuf::from("./multiagent-project"));
        assert_eq!(greenfield.parallel, 4);
        assert!(greenfield.scaffold);
        assert_eq!(greenfield.agent_timeout_secs, 1800);
        assert_eq!(greenfield.max_cost_usd, None);

        let improve = parse_build_args("x", BuildMode::Improve)
            .expect("parses")
            .expect("has prompt");
        assert_eq!(improve.output, PathBuf::from("."));

        let full = parse_build_args(
            "arregla el login --resume --no-scaffold --approve --output ./demo \
             --max-cost-usd 2.5 --build-cmd off --timeout-secs 60",
            BuildMode::Improve,
        )
        .expect("parses")
        .expect("has prompt");
        assert_eq!(full.prompt, "arregla el login");
        assert!(full.resume && full.approve && !full.scaffold);
        assert_eq!(full.output, PathBuf::from("./demo"));
        assert_eq!(full.max_cost_usd, Some(2.5));
        assert_eq!(full.build_cmd.as_deref(), Some("off"));
        assert_eq!(full.agent_timeout_secs, 60);
    }

    #[test]
    fn parse_rejects_malformed_values_and_empty_prompts() {
        // Empty input and flags-without-prompt both mean "print usage".
        assert_eq!(parse_build_args("", BuildMode::Greenfield), Ok(None));
        assert_eq!(
            parse_build_args("  --dry-run  ", BuildMode::Greenfield),
            Ok(None)
        );

        for bad in [
            "tienda --parallel muchos",
            "tienda --parallel",
            "tienda --max-cost-usd gratis",
            "tienda --timeout-secs 0",
            "tienda --output",
            "tienda --build-cmd",
        ] {
            assert!(
                parse_build_args(bad, BuildMode::Greenfield).is_err(),
                "`{bad}` must be rejected"
            );
        }
    }

    #[test]
    fn max_cost_flag_accepts_both_token_forms_and_sets_the_ceiling() {
        // Two-token form: `--max-cost 5` (index advances past the value).
        let tokens = ["--max-cost", "5", "resto"];
        let mut index = 0;
        assert_eq!(take_max_cost_value(&tokens, &mut index), Ok(5.0));
        assert_eq!(index, 1, "value token consumed");

        // Inline form: `--max-cost=2.5` (index untouched).
        let tokens = ["--max-cost=2.5"];
        let mut index = 0;
        assert_eq!(take_max_cost_value(&tokens, &mut index), Ok(2.5));
        assert_eq!(index, 0);

        // End-to-end through the argument parser, both forms.
        let args = parse_build_args("una tienda --max-cost 3.5", BuildMode::Greenfield)
            .expect("parses")
            .expect("has prompt");
        assert_eq!(args.max_cost_usd, Some(3.5));
        assert_eq!(args.prompt, "una tienda");
        let args = parse_build_args("--max-cost=0.75 una tienda", BuildMode::Improve)
            .expect("parses")
            .expect("has prompt");
        assert_eq!(args.max_cost_usd, Some(0.75));
        // The legacy spelling keeps working and maps to the same field.
        let args = parse_build_args("una tienda --max-cost-usd 2.5", BuildMode::Greenfield)
            .expect("parses")
            .expect("has prompt");
        assert_eq!(args.max_cost_usd, Some(2.5));

        // Malformed values fail loudly instead of building without a ceiling.
        for bad in [
            "tienda --max-cost",
            "tienda --max-cost gratis",
            "tienda --max-cost=",
            "tienda --max-cost -1",
            "tienda --max-cost 0",
            "tienda --max-cost=nan",
        ] {
            assert!(
                parse_build_args(bad, BuildMode::Greenfield).is_err(),
                "`{bad}` must be rejected"
            );
        }
    }

    #[test]
    fn resume_offer_answer_parsing_is_forgiving_but_defaults_to_no() {
        for yes in ["s", "S", "si", "Sí", " y ", "YES"] {
            assert!(parse_yes(yes), "`{yes}` must be affirmative");
        }
        for no in ["", "n", "no", "nope", "s i", "yess"] {
            assert!(!parse_yes(no), "`{no}` must NOT be affirmative");
        }
    }

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
