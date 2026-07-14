//! The pipeline: Director (+ self-resolved open questions) → Architects
//! (parallel) → Subdirector (receives the designs) → Developer waves
//! (parallel, module-scoped writes, retry with model escalation) → build
//! gate per wave → Supervisor per delivery → Fixer → QA → Docs.
//! Deterministic Rust control flow; agents do the work. Supports resuming
//! an interrupted build and an optional cost ceiling.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use telemetry::{AnalyticsEvent, JsonlTelemetrySink, SessionTracer};

use crate::agents::{
    extract_json, poll_result, spawn_agent, timeout_result, wait_all, AgentHandle, AgentResult,
};
use crate::catalog::ModelCatalog;
use crate::contracts::{
    schedule_waves, BuildMode, Complexity, Plan, ProjectKind, SupervisionVerdict, TaskSpec,
};
use crate::roles::{system_prompt, write_role_instruction_files, Role};

/// Emits workflow progress to the dashboard events file (when
/// `CLAW_DASHBOARD_EVENTS` is set) and mirrors every phase to stdout.
pub struct WorkflowLog {
    tracer: Option<SessionTracer>,
}

impl WorkflowLog {
    fn new() -> Self {
        let tracer = std::env::var("CLAW_DASHBOARD_EVENTS")
            .ok()
            .map(|path| path.trim().to_string())
            .filter(|path| !path.is_empty())
            .and_then(|path| JsonlTelemetrySink::new(path).ok())
            .map(|sink| SessionTracer::new("multiagent", std::sync::Arc::new(sink)));
        Self { tracer }
    }

    fn phase(&self, message: &str) {
        println!("[multiagent] {message}");
        if let Some(tracer) = &self.tracer {
            tracer.record_analytics(
                AnalyticsEvent::new("workflow", "phase")
                    .with_property("phase", serde_json::Value::String(message.to_string())),
            );
        }
    }

    fn event(&self, action: &str, properties: &[(&str, String)]) {
        if let Some(tracer) = &self.tracer {
            let mut event = AnalyticsEvent::new("workflow", action);
            for (key, value) in properties {
                event = event.with_property(*key, serde_json::Value::String(value.clone()));
            }
            tracer.record_analytics(event);
        }
    }
}

pub struct RunOptions {
    pub kind: ProjectKind,
    /// Greenfield (create) vs. Improve (modify an existing project in place).
    pub mode: BuildMode,
    pub prompt: String,
    pub project_dir: PathBuf,
    pub catalog: ModelCatalog,
    /// Max developer agents running at once inside a wave.
    pub parallel: usize,
    /// Stop after planning (Director + Architects + Subdirector).
    pub dry_run: bool,
    /// Per-agent timeout.
    pub agent_timeout: Duration,
    /// Resume an interrupted build: reuse saved plan/designs/backlog and
    /// skip tasks already completed.
    pub resume: bool,
    /// Optional cost ceiling in USD. `None` (the default) disables budget
    /// enforcement entirely — the right setting for subscription accounts.
    pub max_cost_usd: Option<f64>,
    /// Build-gate command; auto-detected from the project when `None`.
    pub build_command: Option<String>,
    /// Deterministic project scaffold (create-vite / cargo init) before the
    /// first developer runs, so the build gate passes from minute zero.
    pub scaffold: bool,
    /// Pause after planning and ask for confirmation on stdin before
    /// spending developer runs.
    pub approve: bool,
}

pub struct RunSummary {
    pub plan: Plan,
    pub tasks: usize,
    pub waves: usize,
    pub completed: usize,
    pub failed: usize,
    pub supervision_issues: usize,
    pub resumed_tasks: usize,
    pub budget_aborted: bool,
    pub user_aborted: bool,
    /// Ids of tasks that failed terminally (after the escalated retry).
    pub failed_task_ids: Vec<String>,
    /// Ids of tasks never attempted because a (transitive) dependency failed.
    pub blocked_task_ids: Vec<String>,
    /// This run's spend in USD, when telemetry is active.
    pub cost_usd: Option<f64>,
    /// Improve mode: the dedicated work branch and the branch it forked from,
    /// so the caller can print an accurate review/merge hint.
    pub improve_branch: Option<String>,
    pub base_branch: Option<String>,
}

const DIRECTOR_JSON_SCHEMA: &str = r#"Respond with a single ```json fenced object:
{
  "vision": "...", "scope": ["..."],
  "stack": {"kind": "simple|fullstack", "frontend": [], "backend": [], "database": [],
             "orm": [], "auth": [], "platforms": [], "framework": [], "deploy": [],
             "justification": "..."},
  "epics": [{"name": "...", "stories": [{"as_a": "...", "i_want": "...", "so_that": "...",
             "acceptance_criteria": ["..."]}]}],
  "milestones": ["..."], "risks": ["..."], "open_questions": ["..."]
}"#;

const SUBDIRECTOR_JSON_SCHEMA: &str = r#"Respond with a single ```json fenced object:
{"tasks": [{
  "id": "T1", "module": "auth", "functional_objective": "...", "technical_objective": "...",
  "justification": "...", "files_to_create": ["src/auth/login.ts"], "files_to_modify": [],
  "interfaces": ["..."], "functions": ["exact signatures"], "data_models": [],
  "endpoints": [], "libraries": ["lib@version"], "env_vars": [], "edge_cases": [],
  "validations": [], "error_handling": [], "logging": [], "tests_required": [],
  "constraints": [], "risks": [], "definition_of_done": ["..."],
  "priority": 1, "complexity": "simple|medium|complex", "estimated_minutes": 30,
  "wave": 0, "depends_on": []
}]}
Rules: tasks MUST follow the architects' designs exactly (files, interfaces, names);
modules must be independent; two tasks in the same wave must NEVER touch the same
file; assign complexity honestly (it selects the AI model per task)."#;

/// Process-global Ctrl+C state. The handler can only be installed once per
/// process (a second install fails), so it must be shared across every
/// `/web`/`/app` invocation instead of re-installed per build — and it must
/// do NOTHING outside a build, or it would hijack SIGINT for the whole REPL
/// after the first build finishes.
struct BuildSignal {
    /// True only while a build is running.
    active: AtomicBool,
    /// Set by the first Ctrl+C during an active build.
    aborted: AtomicBool,
}

static BUILD_SIGNAL: std::sync::OnceLock<Arc<BuildSignal>> = std::sync::OnceLock::new();

/// Returns the shared signal, installing the one-time handler on first use.
fn build_signal() -> &'static Arc<BuildSignal> {
    BUILD_SIGNAL.get_or_init(|| {
        let signal = Arc::new(BuildSignal {
            active: AtomicBool::new(false),
            aborted: AtomicBool::new(false),
        });
        let handler_signal = Arc::clone(&signal);
        let _ = ctrlc::set_handler(move || {
            // Outside a build, stay out of the way — the REPL and rustyline
            // own SIGINT then.
            if !handler_signal.active.load(Ordering::Relaxed) {
                return;
            }
            // Second Ctrl+C force-quits; the first finishes the current wave,
            // persists state and skips the remaining phases.
            if handler_signal.aborted.swap(true, Ordering::Relaxed) {
                std::process::exit(130);
            }
            eprintln!(
                "\n[multiagent] Ctrl+C: se termina la ola en curso y se guarda el estado \
                 (reanuda con --resume). Ctrl+C de nuevo para forzar la salida."
            );
        });
        signal
    })
}

/// Marks a build active for its lifetime and clears the flag on drop, so a
/// panic or early return never leaves a stale handler armed for the REPL.
struct BuildGuard(&'static Arc<BuildSignal>);

impl BuildGuard {
    fn new() -> Self {
        let signal = build_signal();
        signal.aborted.store(false, Ordering::Relaxed);
        signal.active.store(true, Ordering::Relaxed);
        Self(signal)
    }

    fn aborted(&self) -> bool {
        self.0.aborted.load(Ordering::Relaxed)
    }
}

impl Drop for BuildGuard {
    fn drop(&mut self) {
        self.0.active.store(false, Ordering::Relaxed);
    }
}

#[allow(clippy::too_many_lines)]
pub fn run(options: &RunOptions) -> Result<RunSummary, String> {
    options.catalog.validate_credentials()?;

    // Fail fast on missing credentials: without this, the run does planning
    // setup, creates docs/ and a git branch, and only then dies inside the
    // first Director agent with a confusing provider error.
    if !has_any_provider_credentials() {
        return Err(
            "no provider credentials found in the environment; run /provider use \
             <preset> <key> (or export ANTHROPIC_API_KEY / ANTHROPIC_AUTH_TOKEN / \
             OPENAI_API_KEY / OLLAMA_HOST) before building"
                .to_string(),
        );
    }

    // In Improve mode the project must already have code: refuse an empty
    // directory (the user wants /web there, not /improve). Checked BEFORE any
    // side effect so a rejected target is not left with docs/ or a new .git/.
    if options.mode.is_improve() && !project_has_sources(&options.project_dir) {
        return Err(format!(
            "improve target `{}` has no source files to improve; create a project \
             first with a web/app build",
            options.project_dir.display()
        ));
    }

    let abort_guard = BuildGuard::new();

    let workflow = WorkflowLog::new();
    workflow.event(
        "started",
        &[
            ("kind", options.kind.as_str().to_string()),
            ("prompt", options.prompt.clone()),
            ("project", options.project_dir.display().to_string()),
        ],
    );
    let docs = options.project_dir.join("docs");
    std::fs::create_dir_all(&docs).map_err(|error| error.to_string())?;
    // Keep every agent artifact inside the generated project.
    std::env::set_var("CLAWD_AGENT_STORE", options.project_dir.join(".multiagent"));
    std::env::set_current_dir(&options.project_dir).map_err(|error| error.to_string())?;

    let _ = init_git_repo(&options.project_dir);

    // Improve commits one change per task; do it on a dedicated branch so
    // the user's current branch stays exactly as they left it. They merge
    // (or discard) the branch after reviewing. On --resume the SAME branch
    // is checked out again instead of orphaning it with a fresh one.
    let mut improve_branch: Option<String> = None;
    let mut base_branch: Option<String> = None;
    if options.mode.is_improve() {
        match ensure_improve_branch(&options.project_dir, options.resume) {
            Some((branch, base)) => {
                workflow.phase(&format!(
                    "Rama de trabajo: {branch} — tu rama original queda intacta"
                ));
                improve_branch = Some(branch);
                base_branch = base;
            }
            None => workflow.phase(
                "Aviso: no se pudo crear una rama de trabajo; los commits irán a la rama actual",
            ),
        }
    }

    let budget = Budget::new(options.max_cost_usd);
    if budget.enabled() {
        workflow.phase(&format!(
            "Presupuesto activo: tope {:.2} USD",
            options.max_cost_usd.unwrap_or_default()
        ));
    }

    // For Improve, a bounded snapshot of the existing repo (tree + manifests)
    // grounds every planning agent in what already exists. Developers still
    // read individual files themselves via their tools.
    let repo_context = if options.mode.is_improve() {
        workflow.phase("Analizando el proyecto existente");
        let digest = repo_digest(&options.project_dir);
        format!(
            "\n\n## Existing project (the codebase you are improving)\n{digest}\n\
             Respect its existing stack, structure and conventions."
        )
    } else {
        String::new()
    };

    // ---- Planning (Director → open questions → Architects → Subdirector),
    //      or reuse from disk when resuming. ----
    let (plan, tasks) = if options.resume && planning_artifacts_exist(&docs) {
        workflow.phase("Reanudando: reutilizando plan, diseños y backlog guardados");
        load_planning_artifacts(&docs)?
    } else {
        // Phase 1: Director General.
        let director_task = if options.mode.is_improve() {
            format!(
                "You are improving an EXISTING project.{repo_context}\n\nRequested \
                 change:\n{}\n\nProduce a FOCUSED plan for THIS CHANGE ONLY — do not \
                 re-plan or rewrite the whole product. `vision` describes the change; \
                 `scope` lists exactly what will be touched; `stack` must reflect the \
                 EXISTING stack you detected from the files (do not switch \
                 frameworks). Keep the change minimal and consistent with the \
                 codebase.\n{}",
                options.prompt, DIRECTOR_JSON_SCHEMA
            )
        } else {
            format!(
                "User prompt:\n{}\n\nProduce the complete product plan.\n{}",
                options.prompt, DIRECTOR_JSON_SCHEMA
            )
        };
        workflow.phase(if options.mode.is_improve() {
            "Director: planificando el cambio sobre el proyecto existente"
        } else {
            "Director General: interpretando el prompt y creando el plan"
        });
        let director_report = run_single(
            Role::Director,
            &options.catalog.director,
            options,
            &director_task,
        )?;
        let plan: Plan = retry_parse_json_with_feedback(
            &director_report,
            "Director",
            Role::Director,
            DIRECTOR_JSON_SCHEMA,
            options,
            &workflow,
        )?;
        save_doc(
            &docs,
            "plan.json",
            &serde_json::to_string_pretty(&plan).unwrap_or_default(),
        )?;
        save_doc(&docs, "director-report.md", &director_report.report)?;

        // Phase 1b: the Director resolves its own open questions, documenting
        // each decision, so downstream agents never receive ambiguity.
        let decisions = if plan.open_questions.is_empty() {
            String::new()
        } else {
            workflow.phase(&format!(
                "Director: resolviendo {} pregunta(s) abierta(s)",
                plan.open_questions.len()
            ));
            let resolution = run_single(
                Role::Director,
                &options.catalog.director,
                options,
                &format!(
                    "Your plan left these open questions:\n- {}\n\nAnswer each one \
                     yourself with the most reasonable decision for this product, \
                     justifying it. Respond in Markdown as a numbered list of \
                     decisions.",
                    plan.open_questions.join("\n- ")
                ),
            )?;
            save_doc(&docs, "decisions.md", &resolution.report)?;
            format!("\n\nResolved decisions (binding):\n{}", resolution.report)
        };
        let plan_json = serde_json::to_string(&plan).unwrap_or_default();

        // Phase 2: Architects design BEFORE the backlog exists, so TaskSpecs
        // are grounded in their decisions.
        workflow.phase("Arquitectos: diseñando la solución en paralelo");
        let architect_roles = architects_for(options.kind, &plan);
        let mut architect_handles = Vec::new();
        for role in &architect_roles {
            let handle = spawn_agent(
                role.slug(),
                role.title(),
                &options.catalog.director,
                role.subagent_type(),
                &system_prompt(*role, options.kind),
                &format!(
                    "Plan:\n```json\n{plan_json}\n```{decisions}{repo_context}\n\n\
                     Deliver your complete design document in Markdown, justifying \
                     every decision.{}",
                    if options.mode.is_improve() {
                        " Design the change to fit the EXISTING architecture; read \
                         the relevant existing files and reuse their patterns."
                    } else {
                        ""
                    }
                ),
                &[],
            )?;
            architect_handles.push(handle);
        }
        let mut designs_digest = String::new();
        for result in wait_all(architect_handles, options.agent_timeout, |result| {
            workflow.phase(&format!("  arquitecto {} → {}", result.name, result.status));
        }) {
            save_doc(&docs, &format!("{}.md", result.name), &result.report)?;
            let _ = write!(
                designs_digest,
                "\n\n## Design by {}\n{}",
                result.name,
                truncate_chars(&result.report, 6_000)
            );
        }

        // Phase 3: Subdirector turns plan + designs into the backlog.
        workflow.phase("Subdirector Técnico: creando el backlog desde los diseños");
        let subdirector_prompt = format!(
            "Approved plan:\n```json\n{plan_json}\n```{decisions}{repo_context}\n\n\
             Architect designs (authoritative):{designs_digest}\n\nBreak {} into \
             fully specified, parallelizable TaskSpecs that implement these \
             designs.{}\n{SUBDIRECTOR_JSON_SCHEMA}",
            if options.mode.is_improve() {
                "the change"
            } else {
                "the project"
            },
            if options.mode.is_improve() {
                " Prefer `files_to_modify` for existing files; only list \
                 `files_to_create` for genuinely new files. Every task must follow \
                 the existing conventions and must NOT rewrite unrelated code."
            } else {
                ""
            }
        );
        let subdirector_report = run_single(
            Role::Subdirector,
            &options.catalog.director,
            options,
            &subdirector_prompt,
        )?;
        let mut tasks = parse_backlog(&subdirector_report.report).and_then(|tasks| {
            validate_backlog(&tasks)?;
            Ok(tasks)
        });
        if let Err(problems) = &tasks {
            // One repair round. Agents are stateless: the retry prompt must
            // carry the full original context plus the faulty output.
            workflow.phase(&format!(
                "  Subdirector: backlog rechazado, reintentando con retroalimentación\n{problems}"
            ));
            let retry_report = run_single(
                Role::Subdirector,
                &options.catalog.director,
                options,
                &format!(
                    "{subdirector_prompt}\n\n---\nA previous attempt at this backlog \
                     was rejected. Previous output (truncated):\n```\n{}\n```\n\n\
                     Problems found:\n{problems}\n\nProduce a corrected backlog that \
                     fixes every problem while still following the designs.",
                    truncate_chars(&subdirector_report.report, 6_000)
                ),
            )?;
            tasks = parse_backlog(&retry_report.report).and_then(|tasks| {
                validate_backlog(&tasks)?;
                Ok(tasks)
            });
        }
        let tasks = tasks.map_err(|error| format!("Subdirector backlog invalid: {error}"))?;
        save_doc(
            &docs,
            "backlog.json",
            &serde_json::to_string_pretty(&tasks).unwrap_or_default(),
        )?;

        (plan, tasks)
    };

    // Instruction files per model family (CLAUDE.md / AGENTS.md / GROK.md).
    // In Improve mode, never overwrite the user's existing files.
    let instruction_files = write_role_instruction_files(
        &options.project_dir,
        &options.catalog,
        options.kind,
        options.mode.is_improve(),
    )?;
    workflow.phase(&format!(
        "Archivos de rol por modelo escritos: {}",
        instruction_files.join(", ")
    ));

    let waves = schedule_waves(&tasks);
    if options.dry_run {
        return Ok(RunSummary {
            plan,
            tasks: tasks.len(),
            waves: waves.len(),
            completed: 0,
            failed: 0,
            supervision_issues: 0,
            resumed_tasks: 0,
            budget_aborted: false,
            user_aborted: false,
            failed_task_ids: Vec::new(),
            blocked_task_ids: Vec::new(),
            cost_usd: budget.spent(),
            improve_branch,
            base_branch,
        });
    }

    // ---- Plan checkpoint (--approve): the costliest mistake is a full
    //      build over a misread prompt — ten seconds of review prevent it.
    if options.approve && !plan_checkpoint_confirmed(&plan, tasks.len(), waves.len(), options) {
        workflow.phase("Plan no aprobado: construcción cancelada (los docs quedan guardados)");
        return Ok(RunSummary {
            plan,
            tasks: tasks.len(),
            waves: waves.len(),
            completed: 0,
            failed: 0,
            supervision_issues: 0,
            resumed_tasks: 0,
            budget_aborted: false,
            user_aborted: true,
            failed_task_ids: Vec::new(),
            blocked_task_ids: Vec::new(),
            cost_usd: budget.spent(),
            improve_branch,
            base_branch,
        });
    }

    // Scaffold, contracts and the design system establish a NEW project's
    // foundation; an existing project already has all three, so Improve mode
    // skips straight to the developer waves.
    let greenfield = !options.mode.is_improve();

    // ---- Deterministic scaffold: generated boilerplate is the most
    //      failure-prone and most token-expensive part of a build, and the
    //      part an LLM adds no value to. ----
    if greenfield && options.scaffold {
        scaffold_project(options, &plan, &workflow);
    }

    // ---- Contracts as CODE (after the scaffold, so they land inside the
    //      real project structure). Cross-module consistency is then
    //      enforced by the typechecker at the build gate instead of by
    //      every developer correctly reading a truncated prose digest. ----
    if greenfield && !docs.join("contracts.md").exists() {
        workflow.phase("Contratos: generando interfaces compartidas como código");
        match run_single(
            Role::SoftwareArchitect,
            &options.catalog.director,
            options,
            "Read docs/plan.json, docs/decisions.md (if present), every architect \
             design document under docs/, and docs/backlog.json. Then CREATE the \
             shared contract files in the repository NOW: type/interface \
             definitions, API route constants and data models that the backlog \
             tasks will import (e.g. `src/types.ts` or `src/contracts/` for \
             TypeScript, a shared module for Rust — follow the scaffolded project \
             structure). Keep them minimal but complete: every interface, endpoint \
             and data model named in the backlog must exist and typecheck. Finally \
             write `docs/contracts.md` listing each file you created and what it \
             defines.",
        ) {
            Ok(contracts) => {
                save_doc(&docs, "contracts-report.md", &contracts.report)?;
                let _ = git_commit(&options.project_dir, "contracts: shared interfaces");
            }
            Err(error) => {
                // Contracts improve coordination but are not load-bearing:
                // developers still get designs via their TaskSpecs.
                workflow.phase(&format!("  aviso: fase de contratos falló ({error})"));
            }
        }
    }

    // ---- Design system: tokens and base components as real files, so
    //      every developer composes over the same visual foundation
    //      instead of writing ad-hoc CSS. ----
    if greenfield
        && options.project_dir.join("package.json").exists()
        && !docs.join("design-system.md").exists()
    {
        workflow.phase("Design system: tokens y componentes base");
        match run_single(
            Role::UxUiDesigner,
            &options.catalog.director,
            options,
            "Read docs/plan.json and the UX/UI design document under docs/. Build \
             the visual foundation NOW as real files: (1) if `tailwindcss` and \
             `@tailwindcss/vite` are in devDependencies, wire the vite plugin and \
             add `@import \"tailwindcss\";` plus an `@theme` block to the main \
             stylesheet; (2) define the design tokens — color palette with dark \
             mode, typography scale, spacing — as Tailwind theme tokens or CSS \
             variables; (3) create accessible base UI components (Button, Card, \
             Input, a Layout/Container) under src/components/ui/ (or the \
             stack-appropriate location); (4) write docs/design-system.md \
             documenting every token and component. Developers will be REQUIRED \
             to use these instead of ad-hoc styles. Verify the project still \
             builds before finishing.",
        ) {
            Ok(design) => {
                save_doc(&docs, "design-system-report.md", &design.report)?;
                let _ = git_commit(&options.project_dir, "design: tokens and base components");
            }
            Err(error) => {
                workflow.phase(&format!("  aviso: fase de design system falló ({error})"));
            }
        }
    }

    // ---- Developer waves + Supervisor per delivery ----
    let mut state = BuildState::load(&options.project_dir);
    let resumed_tasks = if options.resume {
        state.completed.len()
    } else {
        state.completed.clear();
        0
    };
    if resumed_tasks > 0 {
        workflow.phase(&format!(
            "Reanudando: {resumed_tasks} tarea(s) ya completadas, se omiten"
        ));
    }

    let supervision_md = docs.join("SUPERVISION.md");
    let mut completed = resumed_tasks;
    let mut failed = 0_usize;
    let mut supervision_issues = 0_usize;
    let mut budget_aborted = false;
    let mut user_aborted = false;
    // Context for later tasks: what has already been built.
    let mut built_context: Vec<String> = tasks
        .iter()
        .filter(|task| state.completed.contains(&task.id))
        .map(context_line)
        .collect();

    // ---- Graph scheduler: no wave barriers. A task starts the moment its
    //      dependencies are done and no in-flight task holds its files; its
    //      Supervisor reviews in parallel while other tasks keep building.
    //      A task's files stay locked from spawn until its supervision (and
    //      any fixes) conclude, so nothing races a pending review. ----
    workflow.phase(&format!(
        "Scheduler: {} tareas, paralelo {}, sin barreras de ola",
        tasks.len(),
        options.parallel.max(1)
    ));
    let mut failed_tasks: BTreeSet<usize> = BTreeSet::new();
    let mut retried: BTreeSet<usize> = BTreeSet::new();
    // Tasks whose files are locked (developer running or supervision pending).
    let mut busy: BTreeSet<usize> = BTreeSet::new();
    let mut devs: Vec<DevSlot> = Vec::new();
    let mut sups: Vec<SupSlot> = Vec::new();

    'scheduler: loop {
        if abort_guard.aborted() {
            workflow.phase("Construcción abortada por usuario (Ctrl+C)");
            save_doc(&docs, "ABORT.txt", "Build interrupted by user\n")?;
            user_aborted = true;
            break 'scheduler;
        }
        if let Some(spent) = budget.exceeded() {
            workflow.phase(&format!(
                "Presupuesto superado ({spent:.2} USD): abortando limpiamente"
            ));
            budget_aborted = true;
            break 'scheduler;
        }

        // Fill free developer slots with ready tasks.
        while devs.len() < options.parallel.max(1) {
            let Some(task_index) = next_ready_task(&tasks, &state.completed, &failed_tasks, &busy)
            else {
                break;
            };
            let task = &tasks[task_index];
            let model = options.catalog.model_for(task.complexity);
            let context = render_wave_context(&built_context);
            let handle = spawn_developer(options, task, model, &context, None)?;
            workflow.phase(&format!(
                "  {} → modelo {} (complejidad {:?})",
                task.id, model, task.complexity
            ));
            busy.insert(task_index);
            devs.push(DevSlot {
                task_index,
                handle,
                started: Instant::now(),
            });
        }

        if devs.is_empty() && sups.is_empty() {
            // Graph drained: anything neither completed nor failed was held
            // back by a failed dependency — say so in the live log, not
            // only in the final summary.
            let blocked: Vec<&str> = tasks
                .iter()
                .enumerate()
                .filter(|(index, task)| {
                    !state.completed.contains(&task.id) && !failed_tasks.contains(index)
                })
                .map(|(_, task)| task.id.as_str())
                .collect();
            if !blocked.is_empty() {
                workflow.phase(&format!(
                    "Bloqueadas por dependencias fallidas (no se intentaron): {}",
                    blocked.join(", ")
                ));
            }
            break;
        }

        std::thread::sleep(Duration::from_millis(400));

        // Poll developers.
        let mut still_running: Vec<DevSlot> = Vec::new();
        for slot in devs {
            let mut result = match poll_result(&slot.handle) {
                Some(result) => result,
                None if slot.started.elapsed() >= options.agent_timeout => {
                    timeout_result(&slot.handle, options.agent_timeout)
                }
                None => {
                    still_running.push(slot);
                    continue;
                }
            };
            let task = &tasks[slot.task_index];
            workflow.phase(&format!("  entrega {} → {}", result.name, result.status));

            // Per-delivery quick check (#4): a broken module is caught the
            // moment it lands, not at the end of the whole build. Failures
            // are attributed only when the output names this task's files —
            // parallel tasks may be mid-write and their errors are not ours.
            let mut check_failure: Option<String> = None;
            if result.succeeded() {
                if let Err(output) = run_quick_check(&options.project_dir) {
                    if task
                        .touched_files()
                        .iter()
                        .any(|file| output.contains(file))
                    {
                        workflow.phase(&format!("  {}: verificación rápida falló", task.id));
                        check_failure = Some(output);
                    }
                }
            }

            let delivery_failed = !result.succeeded() || check_failure.is_some();
            if delivery_failed && !retried.contains(&slot.task_index) {
                if let Some(escalated) = escalate(task.complexity) {
                    let retry_model = options.catalog.model_for(escalated);
                    workflow.phase(&format!(
                        "  {} falló → reintento con modelo superior {retry_model}",
                        task.id
                    ));
                    // Escalations cost premium tokens: leave an audit trail.
                    append_file(
                        &supervision_md,
                        &format!(
                            "\n- Escalation: task {} — {} → {retry_model}\n",
                            task.id,
                            options.catalog.model_for(task.complexity)
                        ),
                    )?;
                    retried.insert(slot.task_index);
                    let context = render_wave_context(&built_context);
                    let handle = spawn_developer(
                        options,
                        task,
                        retry_model,
                        &context,
                        check_failure.as_deref(),
                    )?;
                    still_running.push(DevSlot {
                        task_index: slot.task_index,
                        handle,
                        started: Instant::now(),
                    });
                    continue;
                }
            }
            if let Some(check) = check_failure {
                // Terminal quick-check failure: the delivery does not count
                // as completed even though the agent reported success.
                result.status = "failed".to_string();
                result.error = Some(truncate_chars(&check, 2_000));
            }
            if result.succeeded() {
                completed += 1;
            } else {
                failed += 1;
                failed_tasks.insert(slot.task_index);
                // A terminally failed delivery has nothing worth a paid
                // review: log it, release its file locks, move on.
                workflow.phase(&format!(
                    "  {}: fallo terminal ({}) — sin supervisión, archivos liberados",
                    task.id,
                    result.error.as_deref().unwrap_or("sin detalle")
                ));
                append_file(
                    &supervision_md,
                    &format!(
                        "\n## Task {} — {}\n\n- Status: failed\n- Approved: false\n\
                         - Summary: delivery failed terminally ({}); not supervised\n",
                        task.id,
                        task.module,
                        result.error.as_deref().unwrap_or("no detail")
                    ),
                )?;
                busy.remove(&slot.task_index);
                continue;
            }

            // Supervisor reviews in parallel; the files stay locked.
            match spawn_supervisor(options, task, &result) {
                Ok(handle) => sups.push(SupSlot {
                    task_index: slot.task_index,
                    handle,
                    started: Instant::now(),
                    delivery: result,
                }),
                Err(error) => {
                    workflow.phase(&format!(
                        "  supervisor de {} no pudo lanzarse ({error}) — issue registrado",
                        task.id
                    ));
                    supervision_issues += 1;
                    finalize_completed_task(
                        task,
                        &mut state,
                        &mut built_context,
                        &options.project_dir,
                    );
                    busy.remove(&slot.task_index);
                }
            }
        }
        devs = still_running;

        // Poll supervisors; process each verdict as it lands.
        let mut still_supervising: Vec<SupSlot> = Vec::new();
        for sup in sups {
            let supervisor_result = match poll_result(&sup.handle) {
                Some(result) => result,
                None if sup.started.elapsed() >= options.agent_timeout => {
                    timeout_result(&sup.handle, options.agent_timeout)
                }
                None => {
                    still_supervising.push(sup);
                    continue;
                }
            };
            let task = &tasks[sup.task_index];
            supervision_issues += process_supervision(
                options,
                &workflow,
                &supervision_md,
                task,
                &sup.delivery,
                &supervisor_result,
            )?;
            if sup.delivery.succeeded() {
                finalize_completed_task(task, &mut state, &mut built_context, &options.project_dir);
            }
            busy.remove(&sup.task_index);
        }
        sups = still_supervising;
    }

    // Full build gate at the end: per-delivery quick checks ran throughout,
    // this is the cross-module confirmation (with Fixer retry on failure).
    if !budget_aborted && !user_aborted {
        run_build_gate(options, &workflow, &supervision_md, waves.len())?;
    }

    if !budget_aborted && !user_aborted {
        // ---- Security gate: deterministic, zero-token checks first ----
        run_security_gate(options, &workflow, &supervision_md)?;

        // ---- Performance budget over the built bundle ----
        run_performance_gate(options, &workflow, &supervision_md)?;

        // ---- Seed data: the first impression must never be an empty
        //      table and a spinner (greenfield only) ----
        if greenfield && !docs.join("seed-data.md").exists() {
            workflow.phase("Seed data: poblando la app con datos de demostración");
            match run_single(
                Role::Developer,
                &options.catalog.medium,
                options,
                "The project is built. Create realistic seed/demo data so the FIRST \
                 RUN shows a fully populated UI — never an empty table and a \
                 spinner. Use whatever fits this stack: fixtures, a seed script \
                 wired into the dev flow, or mock API data. Keep it clearly \
                 demo-labeled and easy to remove. Document what you added in \
                 docs/seed-data.md and verify the project still builds.",
            ) {
                Ok(seed) => {
                    save_doc(&docs, "seed-data-report.md", &seed.report)?;
                    let _ = git_commit(&options.project_dir, "seed: demo data for first run");
                }
                Err(error) => {
                    workflow.phase(&format!("  aviso: fase de seed data falló ({error})"));
                }
            }
        }

        // ---- QA ----
        workflow.phase("Agente QA: generando y ejecutando pruebas");
        let qa = run_single(
            Role::Qa,
            &options.catalog.medium,
            options,
            "Generate and run the unit/integration/E2E/smoke tests this project needs. \
             Report coverage, failures found and fixes applied.",
        )?;
        save_doc(&docs, "qa-report.md", &qa.report)?;

        // Run actual tests as a gate.
        workflow.phase("Ejecutando tests generados por QA");
        match run_tests_for_qa(&options.project_dir, &options.build_command) {
            Ok(test_output) => {
                workflow.event("qa_tests_passed", &[("output", test_output)]);
                let _ = git_commit(&options.project_dir, "qa: all tests passed");
            }
            Err(test_error) => {
                workflow.event("qa_tests_failed", &[("error", test_error.clone())]);
                workflow.phase(&format!(
                    "⚠ QA tests fallaron, dispatch Fixer: {}",
                    test_error
                ));

                // Dispatch Fixer to repair test failures. The build is already
                // done at this point, so a Fixer failure degrades to a warning
                // instead of erroring out the whole run.
                match run_single(
                    Role::Fixer,
                    &options.catalog.supervisor,
                    options,
                    &format!(
                        "The generated tests are failing:\n\n```\n{}\n```\n\n\
                         Analyze the failures, fix the source code or tests to make them pass, \
                         and report what you changed.",
                        truncate_chars(&test_error, 12_000)
                    ),
                ) {
                    Ok(fixer_report) => {
                        save_doc(&docs, "fixer-qa-report.md", &fixer_report.report)?;
                        let _ =
                            git_commit(&options.project_dir, "fix: repair test failures (Fixer)");

                        // Retry tests after fixes.
                        workflow.phase("Reintentando tests después de fixes del Fixer");
                        match run_tests_for_qa(&options.project_dir, &options.build_command) {
                            Ok(test_output) => {
                                workflow.event("qa_tests_fixed", &[("output", test_output)]);
                                let _ = git_commit(
                                    &options.project_dir,
                                    "qa: all tests passed (after fixes)",
                                );
                            }
                            Err(retry_error) => {
                                workflow.event(
                                    "qa_tests_still_failing",
                                    &[("error", retry_error.clone())],
                                );
                                workflow.phase(&format!(
                                    "✗ Tests aún fallan después de fixes:\n{}",
                                    retry_error
                                ));
                            }
                        }
                    }
                    Err(fixer_error) => {
                        workflow.phase(&format!(
                            "⚠ Fixer no disponible ({fixer_error}); los tests quedan fallando"
                        ));
                    }
                }
            }
        }

        // ---- Smoke test: does the product actually start and answer?
        //      (also captures the rendered DOM + screenshots when a
        //      headless Chromium is available) ----
        run_smoke_test(&options.project_dir, &docs, &workflow);

        // ---- Visual QA: critique what actually rendered, not the source ----
        if docs.join("rendered-dom.html").exists() {
            workflow.phase("QA visual: revisando el DOM renderizado");
            match run_single(
                Role::UxUiDesigner,
                &options.catalog.supervisor,
                options,
                "docs/rendered-dom.html is the homepage DOM exactly as rendered \
                 after JavaScript ran (docs/screenshots/ may hold PNGs for \
                 humans). Review it together with the UI source code: visual \
                 hierarchy, empty states (is real content actually showing?), \
                 accessibility (landmarks, alt text, form labels, contrast per \
                 the CSS), responsive classes and dead links. FIX every issue \
                 you find directly in the repository and write docs/visual-qa.md \
                 with what you found and fixed.",
            ) {
                Ok(review) => {
                    save_doc(&docs, "visual-qa-report.md", &review.report)?;
                    let _ = git_commit(&options.project_dir, "visual-qa: UI review fixes");
                }
                Err(error) => {
                    workflow.phase(&format!("  aviso: QA visual falló ({error})"));
                }
            }
        }

        // ---- Deploy pack: real files, not prose ----
        if greenfield && !options.project_dir.join("Dockerfile").exists() {
            workflow.phase("Pack de deploy: Dockerfile, CI y config de plataforma");
            match run_single(
                Role::DevOpsArchitect,
                &options.catalog.medium,
                options,
                "Write the REAL deployment files for this project as built: a \
                 multi-stage Dockerfile, .dockerignore, a CI workflow at \
                 .github/workflows/ci.yml (install, build, tests), and the \
                 platform config that fits the stack (vercel.json or netlify.toml \
                 for static/frontend builds). Files must be complete and working, \
                 not examples. Finish with docs/deploy.md documenting the exact \
                 commands to build and deploy.",
            ) {
                Ok(deploy) => {
                    save_doc(&docs, "deploy-report.md", &deploy.report)?;
                    let _ = git_commit(&options.project_dir, "deploy: deployment pack");
                }
                Err(error) => {
                    workflow.phase(&format!("  aviso: pack de deploy falló ({error})"));
                }
            }
        }

        // ---- Documentation ----
        workflow.phase("Agente de Documentación: README, ADRs y changelog");
        let docs_agent = run_single(
            Role::Docs,
            &options.catalog.simple,
            options,
            "Create/update README.md, docs/ARCHITECTURE.md, docs/adr/ (one ADR per key \
             decision from docs/*.md), CHANGELOG.md and deployment/development guides, \
             reflecting the project as actually built.",
        )?;
        save_doc(&docs, "docs-report.md", &docs_agent.report)?;
        let _ = git_commit(&options.project_dir, "docs: add documentation and README");
    }

    workflow.event(
        "finished",
        &[
            ("tasks", tasks.len().to_string()),
            ("completed", completed.to_string()),
            ("failed", failed.to_string()),
            ("user_aborted", user_aborted.to_string()),
            (
                "cost_usd",
                budget
                    .spent()
                    .map_or_else(|| "unknown".to_string(), |cost| format!("{cost:.2}")),
            ),
        ],
    );
    let failed_task_ids: Vec<String> = failed_tasks
        .iter()
        .map(|&index| tasks[index].id.clone())
        .collect();
    // Tasks the drained scheduler never attempted: with dependents of failed
    // tasks held back, anything neither completed nor failed was blocked by
    // a failed (transitive) dependency. Aborts leave ordinary pending tasks
    // behind too, so the list is only meaningful on a natural drain.
    let blocked_task_ids: Vec<String> = if budget_aborted || user_aborted {
        Vec::new()
    } else {
        tasks
            .iter()
            .enumerate()
            .filter(|(index, task)| {
                !state.completed.contains(&task.id) && !failed_tasks.contains(index)
            })
            .map(|(_, task)| task.id.clone())
            .collect()
    };
    // Persist the outcome next to the other build docs: the terminal
    // scrolls away, docs/SUMMARY.md doesn't.
    let summary_md = format!(
        "# Resumen de la construcción\n\n- Visión: {}\n- Tareas: {} (completadas {completed}, \
         fallidas {failed}, bloqueadas {})\n- Tareas fallidas: {}\n- Issues de supervisión: \
         {supervision_issues}\n- Coste estimado: {}\n- Rama de trabajo: {}\n",
        plan.vision,
        tasks.len(),
        blocked_task_ids.len(),
        if failed_task_ids.is_empty() {
            "ninguna".to_string()
        } else {
            failed_task_ids.join(", ")
        },
        budget.spent().map_or_else(
            || "desconocido (sin telemetría)".to_string(),
            |cost| { format!("{cost:.2} USD") }
        ),
        improve_branch.as_deref().unwrap_or("(rama actual)"),
    );
    let _ = save_doc(&docs, "SUMMARY.md", &summary_md);

    Ok(RunSummary {
        plan,
        tasks: tasks.len(),
        waves: waves.len(),
        completed,
        failed,
        supervision_issues,
        resumed_tasks,
        budget_aborted,
        user_aborted,
        failed_task_ids,
        blocked_task_ids,
        cost_usd: budget.spent(),
        improve_branch,
        base_branch,
    })
}

/// A developer (or escalated retry) in flight.
struct DevSlot {
    task_index: usize,
    handle: AgentHandle,
    started: Instant,
}

/// A supervisor reviewing a landed delivery, in parallel with other work.
struct SupSlot {
    task_index: usize,
    handle: AgentHandle,
    started: Instant,
    delivery: AgentResult,
}

/// Picks the next runnable task: not done/failed/in-flight, every known
/// dependency COMPLETED (a failed dependency blocks its dependents — they
/// would build on a missing foundation and cascade-fail, each burning a
/// developer plus a supervisor run), and no file shared with an in-flight
/// task. Deterministic: lowest priority value first, then backlog order.
/// Blocked tasks are reported separately in the run summary.
#[must_use]
pub fn next_ready_task(
    tasks: &[TaskSpec],
    completed_ids: &BTreeSet<String>,
    failed: &BTreeSet<usize>,
    busy: &BTreeSet<usize>,
) -> Option<usize> {
    let busy_files: BTreeSet<&str> = busy
        .iter()
        .flat_map(|&index| tasks[index].touched_files())
        .collect();
    let mut best: Option<usize> = None;
    for (index, task) in tasks.iter().enumerate() {
        if completed_ids.contains(&task.id) || failed.contains(&index) || busy.contains(&index) {
            continue;
        }
        let deps_settled = task.depends_on.iter().all(|dep| {
            dep == &task.id || completed_ids.contains(dep) || !tasks.iter().any(|t| &t.id == dep)
        });
        if !deps_settled {
            continue;
        }
        if task
            .touched_files()
            .iter()
            .any(|file| busy_files.contains(file))
        {
            continue;
        }
        best = match best {
            Some(current) if (tasks[current].priority, current) <= (task.priority, index) => {
                Some(current)
            }
            _ => Some(index),
        };
    }
    best
}

/// Bookkeeping for a task that finished and passed supervision: resume
/// state, context for later tasks, and a git commit per delivery.
fn finalize_completed_task(
    task: &TaskSpec,
    state: &mut BuildState,
    built_context: &mut Vec<String>,
    project_dir: &Path,
) {
    state.completed.insert(task.id.clone());
    state.save(project_dir);
    built_context.push(context_line(task));
    let commit_msg = format!(
        "{} ({}): {}",
        task.id,
        task.module,
        truncate_chars(&task.functional_objective, 60)
    );
    let _ = git_commit(project_dir, &commit_msg);
}

/// Spawns one developer agent with module-scoped writes, built-so-far
/// context, the contracts pointer, and (on retries) the verification
/// failure to fix.
fn spawn_developer(
    options: &RunOptions,
    task: &TaskSpec,
    model: &str,
    wave_context: &str,
    verification_failure: Option<&str>,
) -> Result<AgentHandle, String> {
    let mut prompt = format!("{}{}", task.render_prompt(), wave_context);
    if options
        .project_dir
        .join("docs")
        .join("contracts.md")
        .exists()
    {
        prompt.push_str(
            "\n\n## Shared contracts\nRead docs/contracts.md and IMPORT the existing \
             contract files; never redefine shared types, API routes or data models.",
        );
    }
    if options
        .project_dir
        .join("docs")
        .join("design-system.md")
        .exists()
    {
        prompt.push_str(
            "\n\n## Design system\nRead docs/design-system.md and USE the existing \
             tokens and base components (src/components/ui/); never write ad-hoc \
             styles or duplicate base components.",
        );
    }
    prompt.push_str(
        "\n\n## Quality bar\n- Security: never hardcode secrets (env vars + \
         .env.example); sanitize user input; httpOnly cookies; strict CORS.\n\
         - Performance: route-level code-splitting, lazy-load heavy \
         components/images, no heavyweight dependencies for trivial work.\n\
         - Accessibility: semantic landmarks, labeled form controls, alt text.",
    );
    if let Some(failure) = verification_failure {
        let _ = write!(
            prompt,
            "\n\n## Previous attempt failed verification\nThe project no longer \
             compiles after the previous attempt at this task. Compiler output:\n\
             ```\n{}\n```\nFix the root cause and make the project build again.",
            truncate_chars(failure, 8_000)
        );
    }
    spawn_agent(
        &format!("dev-{}", task.id),
        &task.functional_objective,
        model,
        Role::Developer.subagent_type(),
        &system_prompt(Role::Developer, options.kind),
        &prompt,
        &write_scope_for(task),
    )
}

/// Cheap per-delivery verification. Full builds stay at the final gate;
/// this catches type/compile breaks the moment a module lands.
fn run_quick_check(project_dir: &Path) -> Result<(), String> {
    let command = if project_dir.join("Cargo.toml").exists() {
        "cargo check --all-targets"
    } else if project_dir.join("tsconfig.json").exists() {
        "npx --yes tsc --noEmit"
    } else {
        return Ok(());
    };
    shell_output(command, project_dir)
}

// ---------- Deterministic scaffold ----------

/// Stack-appropriate non-interactive generator.
#[derive(Debug, PartialEq, Eq)]
pub enum ScaffoldKind {
    /// `npm create vite` with this template.
    Vite(&'static str),
    /// `cargo init` for Rust apps/services.
    CargoInit,
    /// Bare `npm init -y` for plain Node backends.
    NpmInit,
}

/// Maps the Director's stack to a generator. Conservative: only stacks with
/// a well-known non-interactive CLI get one; meta-frameworks with
/// interactive installers (Next/Nuxt/Remix/Astro) are left to the agents.
#[must_use]
pub fn scaffold_kind_for(plan: &Plan) -> Option<ScaffoldKind> {
    let stack = format!(
        "{} {} {} {}",
        plan.stack.kind,
        plan.stack.frontend.join(" "),
        plan.stack.framework.join(" "),
        plan.stack.backend.join(" ")
    )
    .to_lowercase();
    if ["next", "nuxt", "remix", "astro"]
        .iter()
        .any(|framework| stack.contains(framework))
    {
        return None;
    }
    if ["tauri", "rust", "axum", "actix"]
        .iter()
        .any(|marker| stack.contains(marker))
    {
        return Some(ScaffoldKind::CargoInit);
    }
    for (marker, template) in [
        ("react", "react-ts"),
        ("vue", "vue-ts"),
        ("svelte", "svelte-ts"),
        ("solid", "solid-ts"),
        ("preact", "preact-ts"),
    ] {
        if stack.contains(marker) {
            return Some(ScaffoldKind::Vite(template));
        }
    }
    if stack.contains("node") || stack.contains("express") || stack.contains("fastify") {
        return Some(ScaffoldKind::NpmInit);
    }
    None
}

/// Runs the deterministic scaffold so the project builds from minute zero
/// and developers only write product code. Generators run into a staging
/// dir and only files that don't already exist are merged, so docs/ and
/// .multiagent/ are never clobbered. Every failure degrades to a warning —
/// agents can still build from scratch exactly as before.
fn scaffold_project(options: &RunOptions, plan: &Plan, workflow: &WorkflowLog) {
    let dir = &options.project_dir;
    if dir.join("package.json").exists() || dir.join("Cargo.toml").exists() {
        return; // already scaffolded (resume or re-run)
    }
    let Some(kind) = scaffold_kind_for(plan) else {
        workflow.phase("Scaffold: stack sin plantilla determinista; lo generan los agentes");
        return;
    };
    let warn = |error: &str| {
        workflow.phase(&format!(
            "  aviso: scaffold falló ({}); se continúa sin plantilla",
            truncate_chars(error, 300)
        ));
    };
    match kind {
        ScaffoldKind::Vite(template) => {
            workflow.phase(&format!("Scaffold: create-vite ({template})"));
            let staging = dir.join(".scaffold");
            let _ = std::fs::remove_dir_all(&staging);
            let command = format!("npm create vite@latest .scaffold -- --template {template}");
            if let Err(error) = shell_output(&command, dir) {
                warn(&error);
                let _ = std::fs::remove_dir_all(&staging);
                return;
            }
            merge_missing(&staging, dir);
            let _ = std::fs::remove_dir_all(&staging);
            workflow.phase("Scaffold: npm install (+ Tailwind CSS)");
            if let Err(error) = shell_output("npm install --no-audit --no-fund", dir) {
                warn(&error);
            } else if let Err(error) = shell_output(
                // Tailwind v4: the design-system phase wires the vite plugin
                // and tokens; installing here keeps that phase deterministic.
                "npm install --no-audit --no-fund -D tailwindcss @tailwindcss/vite",
                dir,
            ) {
                warn(&error);
            }
        }
        ScaffoldKind::CargoInit => {
            workflow.phase("Scaffold: cargo init");
            if let Err(error) = shell_output("cargo init --vcs none .", dir) {
                warn(&error);
                return;
            }
        }
        ScaffoldKind::NpmInit => {
            workflow.phase("Scaffold: npm init");
            if let Err(error) = shell_output("npm init -y", dir) {
                warn(&error);
                return;
            }
        }
    }
    let _ = git_commit(dir, "scaffold: deterministic project template");
}

/// Recursively copies entries from `from` into `to`, skipping any file that
/// already exists at the destination (descending into shared directories).
fn merge_missing(from: &Path, to: &Path) {
    let Ok(entries) = std::fs::read_dir(from) else {
        return;
    };
    for entry in entries.flatten() {
        let source = entry.path();
        let target = to.join(entry.file_name());
        if target.exists() {
            if source.is_dir() && target.is_dir() {
                merge_missing(&source, &target);
            }
            continue;
        }
        if source.is_dir() {
            copy_dir_recursive(&source, &target);
        } else {
            let _ = std::fs::copy(&source, &target);
        }
    }
}

fn copy_dir_recursive(from: &Path, to: &Path) {
    if std::fs::create_dir_all(to).is_err() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(from) else {
        return;
    };
    for entry in entries.flatten() {
        let source = entry.path();
        let target = to.join(entry.file_name());
        if source.is_dir() {
            copy_dir_recursive(&source, &target);
        } else {
            let _ = std::fs::copy(&source, &target);
        }
    }
}

// ---------- Improve mode (existing-project analysis) ----------

/// Directories never worth showing the planner (deps, build output, VCS,
/// our own artifacts).
const REPO_SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "dist",
    "build",
    ".next",
    ".nuxt",
    ".output",
    ".svelte-kit",
    ".multiagent",
    ".scaffold",
    "docs",
    "vendor",
    ".venv",
    "__pycache__",
    "coverage",
    ".cache",
    ".idea",
    ".vscode",
    ".terraform",
    "Pods",
    "DerivedData",
    ".gradle",
    ".pytest_cache",
    ".mypy_cache",
];

/// Manifest/config files whose full contents anchor the planner in the
/// project's real stack.
const REPO_MANIFEST_FILES: &[&str] = &[
    "package.json",
    "Cargo.toml",
    "tsconfig.json",
    "pyproject.toml",
    "go.mod",
    "requirements.txt",
    "vite.config.ts",
    "vite.config.js",
    "next.config.js",
    "svelte.config.js",
    "tailwind.config.js",
    "tailwind.config.ts",
    "pnpm-workspace.yaml",
    "pnpm-lock.yaml",
    "Dockerfile",
    "docker-compose.yml",
    "Makefile",
    "deno.json",
    "composer.json",
    "Gemfile",
    "go.sum",
];

/// True when the directory contains at least one source file worth
/// improving (used to reject `/improve` on an empty target).
#[must_use]
pub fn project_has_sources(project_dir: &Path) -> bool {
    let mut pending = vec![project_dir.to_path_buf()];
    let mut budget = 2_000_usize;
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if budget == 0 {
                return false;
            }
            budget -= 1;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if !REPO_SKIP_DIRS.contains(&name.as_str()) && !name.starts_with('.') {
                    pending.push(path);
                }
            } else if REPO_MANIFEST_FILES.contains(&name.as_str())
                || matches!(
                    path.extension().and_then(|e| e.to_str()),
                    Some(
                        "ts" | "tsx"
                            | "js"
                            | "jsx"
                            | "rs"
                            | "py"
                            | "go"
                            | "vue"
                            | "svelte"
                            | "html"
                            | "css"
                    )
                )
            {
                return true;
            }
        }
    }
    false
}

/// A bounded snapshot of an existing repo for the planning agents: a file
/// tree (capped) plus the full text of the key manifest files. Developers
/// read individual source files themselves via their tools, so the digest
/// stays small regardless of repo size.
#[must_use]
pub fn repo_digest(project_dir: &Path) -> String {
    let mut files: Vec<(String, u64)> = Vec::new();
    let mut manifests: Vec<(String, String)> = Vec::new();
    let mut pending = vec![project_dir.to_path_buf()];
    let mut budget = 4_000_usize;

    'walk: while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if budget == 0 || files.len() >= 400 {
                break 'walk;
            }
            budget -= 1;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if !REPO_SKIP_DIRS.contains(&name.as_str()) {
                    pending.push(path);
                }
                continue;
            }
            let rel = path
                .strip_prefix(project_dir)
                .unwrap_or(&path)
                .display()
                .to_string();
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            if REPO_MANIFEST_FILES.contains(&name.as_str()) && manifests.len() < 12 {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    manifests.push((rel.clone(), truncate_chars(&content, 3_000)));
                }
            }
            files.push((rel, size));
        }
    }

    files.sort();
    let mut out = String::new();
    if let Some(branch) = current_git_branch(project_dir) {
        let _ = writeln!(out, "### Current branch\n`{branch}`\n");
    }
    // Recent history tells the planners what changed lately, the commit
    // conventions in use, and which areas are active — signal the file
    // tree alone cannot provide.
    if let Some(log) = recent_git_log(project_dir) {
        let _ = write!(out, "### Recent commits\n```\n{log}\n```\n\n");
    }
    // Uncommitted work is the most fragile part of the repo: planners must
    // know about it so their tasks neither clobber nor duplicate it.
    if let Some(status) = git_status_short(project_dir) {
        let _ = write!(
            out,
            "### Uncommitted changes (work in progress — do not discard)\n```\n{status}\n```\n\n"
        );
    }
    out.push_str("### File tree\n```\n");
    for (rel, size) in files.iter().take(400) {
        let _ = writeln!(out, "{rel} ({size} B)");
    }
    if files.len() >= 400 {
        out.push_str("… (tree truncated)\n");
    }
    out.push_str("```\n");
    for (rel, content) in &manifests {
        let _ = write!(out, "\n### {rel}\n```\n{content}\n```\n");
    }
    out
}

/// `git status --short` capped to 60 lines, or `None` when clean/no repo.
fn git_status_short(project_dir: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["status", "--short"])
        .current_dir(project_dir)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return None;
    }
    let mut shown: String = lines
        .iter()
        .take(60)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    if lines.len() > 60 {
        let _ = write!(shown, "\n… ({} more)", lines.len() - 60);
    }
    Some(shown)
}

/// The last 20 one-line commits, or `None` outside a repo with history.
fn recent_git_log(project_dir: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["log", "--oneline", "-n", "20"])
        .current_dir(project_dir)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let log = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !log.is_empty()).then_some(log)
}

// ---------- Security gate ----------

/// Fixed-prefix credential patterns; simple `contains` keeps the scan fast
/// and dependency-free.
const SECRET_MARKERS: &[&str] = &[
    "sk-ant-",
    "sk-proj-",
    "AKIA",
    "ghp_",
    "github_pat_",
    "glpat-",
    "xoxb-",
    "xoxp-",
    "AIzaSy",
    "npm_",
    "-----BEGIN RSA PRIVATE KEY",
    "-----BEGIN OPENSSH PRIVATE KEY",
    "-----BEGIN EC PRIVATE KEY",
];

/// Walks the generated sources looking for hardcoded credentials. Skips
/// vendored/build/docs directories and `.env.example` (placeholders belong
/// there). Capped so a pathological tree cannot stall the gate.
#[must_use]
pub fn scan_for_secrets(project_dir: &Path) -> Vec<String> {
    // Same skip set as the planner digest: vendored/build/VCS trees are not
    // generated sources, and two hand-maintained lists would drift apart.
    const SKIP_DIRS: &[&str] = REPO_SKIP_DIRS;
    let mut findings = Vec::new();
    let mut pending = vec![project_dir.to_path_buf()];
    let mut visited = 0_usize;
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if findings.len() >= 10 || visited >= 4_000 {
                return findings;
            }
            visited += 1;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if !SKIP_DIRS.contains(&name.as_str()) {
                    pending.push(path);
                }
                continue;
            }
            if name == ".env.example" {
                continue;
            }
            // Minified/bundled artifacts routinely embed marker-like strings
            // (e.g. "npm_" in tooling banners) and are not hand-written
            // sources; scanning them yields noise, not leaks.
            if name.ends_with(".min.js") || name.ends_with(".min.css") || name.ends_with(".map") {
                continue;
            }
            // Lockfiles embed integrity hashes that trip marker-like
            // substrings without being leaks (yarn.lock matches `.lock`).
            if name.ends_with(".lock") || name == "package-lock.json" || name == "pnpm-lock.yaml" {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.len() > 512 * 1024 {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            for marker in SECRET_MARKERS {
                if content.contains(marker) {
                    findings.push(format!(
                        "{} contains `{marker}…`",
                        path.strip_prefix(project_dir).unwrap_or(&path).display()
                    ));
                    break;
                }
            }
        }
    }
    findings
}

/// Deterministic security pass (zero tokens until something is found):
/// dependency audit + hardcoded-secret scan; findings go to the Fixer.
fn run_security_gate(
    options: &RunOptions,
    workflow: &WorkflowLog,
    supervision_md: &Path,
) -> Result<(), String> {
    workflow.phase("Gate de seguridad: auditoría de dependencias y escaneo de secretos");
    let dir = &options.project_dir;
    let mut findings: Vec<String> = Vec::new();

    if dir.join("package-lock.json").exists() {
        if let Err(output) = shell_output("npm audit --audit-level=high", dir) {
            findings.push(format!(
                "npm audit found high/critical vulnerabilities:\n{}",
                truncate_chars(&output, 3_000)
            ));
        }
    }
    // cargo-audit is optional tooling; only run it when installed.
    if dir.join("Cargo.lock").exists() && shell_output("cargo audit --version", dir).is_ok() {
        if let Err(output) = shell_output("cargo audit", dir) {
            findings.push(format!(
                "cargo audit found vulnerable dependencies:\n{}",
                truncate_chars(&output, 3_000)
            ));
        }
    }
    for hit in scan_for_secrets(dir) {
        findings.push(format!("possible hardcoded secret: {hit}"));
    }

    if findings.is_empty() {
        workflow.phase("  seguridad: OK");
        return Ok(());
    }
    workflow.phase(&format!(
        "  seguridad: {} hallazgo(s) → despachando Técnico",
        findings.len()
    ));
    append_file(
        supervision_md,
        &format!("\n## Security gate findings\n\n{}\n", findings.join("\n\n")),
    )?;
    match run_single(
        Role::Fixer,
        &options.catalog.supervisor,
        options,
        &format!(
            "Security findings in the repository:\n\n{}\n\nFix them: upgrade or \
             replace vulnerable dependencies, move every hardcoded secret to \
             environment variables, add a complete .env.example, and make sure \
             no secret is ever logged. Report each fix applied.",
            findings.join("\n\n")
        ),
    ) {
        Ok(fixer) => {
            append_file(
                supervision_md,
                &format!("\n### Security fixes\n\n{}\n", fixer.report),
            )?;
        }
        Err(error) => {
            workflow.phase(&format!(
                "  aviso: Fixer de seguridad no disponible ({error})"
            ));
        }
    }
    Ok(())
}

// ---------- Performance budget ----------

/// Per-chunk JS budget over the built output. 400 KB raw ≈ 120 KB gzipped —
/// past that, the first paint pays for it.
const DEFAULT_BUNDLE_BUDGET_KB: u64 = 400;

/// Budget override in KB; junk or zero falls back to the default so a typo
/// cannot silently disable the gate.
#[must_use]
fn parse_perf_budget_kb(raw: Option<&str>) -> u64 {
    raw.and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|&kb| kb > 0)
        .unwrap_or(DEFAULT_BUNDLE_BUDGET_KB)
}

/// Thin env wrapper over [`parse_perf_budget_kb`] (`CLAW_PERF_BUDGET_KB`).
#[must_use]
fn perf_budget_kb() -> u64 {
    parse_perf_budget_kb(std::env::var("CLAW_PERF_BUDGET_KB").ok().as_deref())
}

/// Collects built JS chunks over budget, largest first.
#[must_use]
pub fn oversized_bundles(dist: &Path) -> Vec<(PathBuf, u64)> {
    let budget_bytes = perf_budget_kb() * 1_000;
    let mut heavy = Vec::new();
    let mut pending = vec![dist.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let is_js = path
                .extension()
                .is_some_and(|extension| extension == "js" || extension == "mjs");
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if is_js && metadata.len() > budget_bytes {
                heavy.push((path, metadata.len()));
            }
        }
    }
    heavy.sort_by_key(|(_, size)| std::cmp::Reverse(*size));
    heavy
}

/// Checks the built bundle against the budget; breaches go to the Fixer
/// with the exact file list.
fn run_performance_gate(
    options: &RunOptions,
    workflow: &WorkflowLog,
    supervision_md: &Path,
) -> Result<(), String> {
    let dist = ["dist", "build", ".output/public"]
        .iter()
        .map(|candidate| options.project_dir.join(candidate))
        .find(|path| path.is_dir());
    let Some(dist) = dist else {
        return Ok(()); // nothing built to measure (e.g. pure backend)
    };
    let heavy = oversized_bundles(&dist);
    if heavy.is_empty() {
        workflow.phase("Performance budget: OK");
        return Ok(());
    }
    let listing = heavy
        .iter()
        .map(|(path, size)| {
            format!(
                "- {} ({} KB)",
                path.strip_prefix(&options.project_dir)
                    .unwrap_or(path)
                    .display(),
                size / 1024
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let budget_kb = perf_budget_kb();
    workflow.phase(&format!(
        "Performance budget: {} chunk(s) sobre {budget_kb} KB → despachando Técnico",
        heavy.len()
    ));
    append_file(
        supervision_md,
        &format!("\n## Performance budget exceeded\n\n{listing}\n"),
    )?;
    match run_single(
        Role::Fixer,
        &options.catalog.supervisor,
        options,
        &format!(
            "These built JS chunks exceed the {budget_kb} KB performance budget:\n\n\
             {listing}\n\nReduce them: route-level code-splitting with dynamic \
             imports, lazy-load heavy components, and replace heavyweight \
             dependencies used for trivial work. Rebuild to verify, and report \
             each change.",
        ),
    ) {
        Ok(fixer) => {
            append_file(
                supervision_md,
                &format!("\n### Performance fixes\n\n{}\n", fixer.report),
            )?;
        }
        Err(error) => {
            workflow.phase(&format!(
                "  aviso: Fixer de performance no disponible ({error})"
            ));
        }
    }
    Ok(())
}

// ---------- Plan checkpoint (--approve) ----------

/// Prints the plan summary and waits for explicit confirmation on stdin.
fn plan_checkpoint_confirmed(
    plan: &Plan,
    tasks: usize,
    waves: usize,
    options: &RunOptions,
) -> bool {
    use std::io::Write as _;
    println!("\n[multiagent] === plan listo — revisión (--approve) ===");
    println!("  visión:  {}", plan.vision);
    println!(
        "  stack:   {} (front: {} · back: {})",
        plan.stack.kind,
        plan.stack.frontend.join(", "),
        plan.stack.backend.join(", ")
    );
    println!(
        "  tareas:  {tasks} en ~{waves} olas · paralelo {}",
        options.parallel
    );
    println!(
        "  modelos: {} / {} / {}",
        options.catalog.simple, options.catalog.medium, options.catalog.complex
    );
    println!("  docs:    plan.json, backlog.json y diseños guardados en docs/");
    print!("  ¿Continuar con la construcción? [s/N] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(
        answer.trim().to_lowercase().as_str(),
        "s" | "si" | "sí" | "y" | "yes"
    )
}

// ---------- Smoke test ----------

/// Seconds to wait for the dev server before declaring the smoke test dead.
#[cfg(unix)]
const DEFAULT_SMOKE_TIMEOUT_SECS: u64 = 45;

/// Timeout override in seconds; junk or zero keeps the default.
#[cfg(unix)]
#[must_use]
fn parse_smoke_timeout_secs(raw: Option<&str>) -> u64 {
    raw.and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|&secs| secs > 0)
        .unwrap_or(DEFAULT_SMOKE_TIMEOUT_SECS)
}

/// Thin env wrapper over [`parse_smoke_timeout_secs`]
/// (`CLAW_SMOKE_TIMEOUT_SECS`).
#[cfg(unix)]
#[must_use]
fn smoke_timeout_secs() -> u64 {
    parse_smoke_timeout_secs(std::env::var("CLAW_SMOKE_TIMEOUT_SECS").ok().as_deref())
}

/// Start command for the smoke test, from package.json scripts.
#[must_use]
pub fn smoke_command(project_dir: &Path) -> Option<String> {
    let package = std::fs::read_to_string(project_dir.join("package.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&package).ok()?;
    let scripts = json.get("scripts")?;
    for (script, command) in [
        ("dev", "npm run dev"),
        ("start", "npm start"),
        ("preview", "npm run preview"),
    ] {
        if scripts.get(script).is_some() {
            return Some(command.to_string());
        }
    }
    None
}

/// "Compiles" and "works" are different claims: start the dev server, hit
/// the root route, and record what actually came back in docs/smoke-test.md.
/// Unix-only (needs process groups to reap npm's children); degrades to a
/// warning everywhere else and on every failure.
#[cfg(unix)]
fn run_smoke_test(project_dir: &Path, docs: &Path, workflow: &WorkflowLog) {
    use std::io::{Read as _, Write as _};
    use std::os::unix::process::CommandExt as _;

    let Some(command) = smoke_command(project_dir) else {
        workflow.phase("Smoke test: sin script dev/start en package.json, omitido");
        return;
    };
    workflow.phase(&format!("Smoke test: arrancando `{command}`"));
    let log_path = project_dir.join(".multiagent").join("smoke.log");
    let log = std::fs::File::create(&log_path).ok();
    let mut builder = std::process::Command::new("sh");
    builder.arg("-c").arg(&command).current_dir(project_dir);
    builder.process_group(0);
    match (log.as_ref().and_then(|f| f.try_clone().ok()), log) {
        (Some(stderr), Some(stdout)) => {
            builder.stdout(stdout).stderr(stderr);
        }
        _ => {
            builder
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
        }
    }
    let Ok(mut child) = builder.spawn() else {
        workflow.phase("⚠ Smoke test: no se pudo arrancar el servidor");
        return;
    };

    let ports: [u16; 6] = [5173, 3000, 8080, 4321, 4173, 8000];
    let timeout_secs = smoke_timeout_secs();
    let mut hit: Option<(u16, String)> = None;
    'wait: for _ in 0..timeout_secs {
        std::thread::sleep(Duration::from_secs(1));
        for port in ports {
            let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
            let Ok(mut stream) =
                std::net::TcpStream::connect_timeout(&address, Duration::from_millis(300))
            else {
                continue;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let request =
                format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
            if stream.write_all(request.as_bytes()).is_err() {
                continue;
            }
            let mut buffer = Vec::new();
            let mut limited = stream.take(4096);
            let _ = limited.read_to_end(&mut buffer);
            if buffer.is_empty() {
                continue;
            }
            hit = Some((port, String::from_utf8_lossy(&buffer).to_string()));
            break 'wait;
        }
    }

    // While the server is still alive: capture what actually rendered
    // (screenshot for humans, post-JS DOM for the visual-QA agent).
    if let Some((port, _)) = &hit {
        if capture_rendered_page(docs, *port) {
            workflow.phase("  captura: DOM renderizado y screenshot guardados en docs/");
        }
    }

    // Kill the whole process group: npm's children outlive a plain kill.
    let pid = child.id();
    let _ = std::process::Command::new("kill")
        .args(["-TERM", &format!("-{pid}")])
        .status();
    std::thread::sleep(Duration::from_millis(500));
    let _ = std::process::Command::new("kill")
        .args(["-KILL", &format!("-{pid}")])
        .status();
    let _ = child.wait();

    match hit {
        Some((port, response)) => {
            let status_line = response.lines().next().unwrap_or("").trim().to_string();
            let ok = status_line.contains("200");
            workflow.phase(&format!(
                "{} Smoke test: puerto {port} → {status_line}",
                if ok { "✓" } else { "⚠" }
            ));
            workflow.event(
                "smoke_test",
                &[
                    ("ok", ok.to_string()),
                    ("port", port.to_string()),
                    ("status", status_line.clone()),
                ],
            );
            let _ = save_doc(
                docs,
                "smoke-test.md",
                &format!(
                    "# Smoke test\n\n- command: `{command}`\n- port: {port}\n\
                     - status: {status_line}\n- ok: {ok}\n\n## First bytes\n\n\
                     ```\n{}\n```\n",
                    truncate_chars(&response, 1_500)
                ),
            );
        }
        None => {
            workflow.phase("⚠ Smoke test: el servidor no respondió en ningún puerto conocido");
            workflow.event(
                "smoke_test",
                &[
                    ("ok", "false".to_string()),
                    ("error", "server never answered on a known port".to_string()),
                ],
            );
            let _ = save_doc(
                docs,
                "smoke-test.md",
                &format!(
                    "# Smoke test\n\nFAILED: the dev server never answered on a known \
                     port within {timeout_secs}s (see .multiagent/smoke.log).\n"
                ),
            );
        }
    }
}

#[cfg(not(unix))]
fn run_smoke_test(_project_dir: &Path, _docs: &Path, workflow: &WorkflowLog) {
    workflow.phase("Smoke test: omitido (solo unix)");
}

/// First executable from `candidates` found on PATH.
#[must_use]
fn find_in_path(candidates: &[&str]) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for candidate in candidates {
            if dir.join(candidate).is_file() {
                return Some((*candidate).to_string());
            }
        }
    }
    None
}

/// Captures the running app with a headless Chromium when one is available:
/// a screenshot under docs/screenshots/ (for humans) and the post-JS DOM at
/// docs/rendered-dom.html (for the visual-QA agent — it reviews what the
/// browser actually produced, not what the source promises). Returns whether
/// anything was captured; no browser just means no visual QA.
#[cfg(unix)]
fn capture_rendered_page(docs: &Path, port: u16) -> bool {
    let Some(browser) = find_in_path(&[
        "chromium",
        "chromium-browser",
        "google-chrome",
        "google-chrome-stable",
    ]) else {
        return false;
    };
    let url = format!("http://127.0.0.1:{port}/");
    let shots = docs.join("screenshots");
    let _ = std::fs::create_dir_all(&shots);
    let screenshot = shots.join("home.png");
    let _ = std::process::Command::new(&browser)
        .args([
            "--headless=new",
            "--disable-gpu",
            "--no-sandbox",
            "--window-size=1280,800",
            "--virtual-time-budget=8000",
            &format!("--screenshot={}", screenshot.display()),
            &url,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    if let Ok(output) = std::process::Command::new(&browser)
        .args([
            "--headless=new",
            "--disable-gpu",
            "--no-sandbox",
            "--virtual-time-budget=8000",
            "--dump-dom",
            &url,
        ])
        .stderr(std::process::Stdio::null())
        .output()
    {
        if output.status.success() && !output.stdout.is_empty() {
            let _ = std::fs::write(docs.join("rendered-dom.html"), &output.stdout);
        }
    }
    docs.join("rendered-dom.html").exists() || screenshot.exists()
}

/// Directories a developer agent may write to, derived from its TaskSpec.
/// Empty would mean no enforcement, but `validate_backlog` rejects tasks
/// with no files before any developer is spawned, so a developer's scope is
/// always non-empty in practice.
#[must_use]
pub fn write_scope_for(task: &TaskSpec) -> Vec<String> {
    let mut scope: BTreeSet<String> = BTreeSet::new();
    for file in task.files_to_create.iter().chain(&task.files_to_modify) {
        match Path::new(file).parent() {
            Some(parent) if !parent.as_os_str().is_empty() => {
                scope.insert(parent.display().to_string());
            }
            _ => {
                // Root-level file: allow exactly that file.
                scope.insert(file.clone());
            }
        }
    }
    scope.into_iter().collect()
}

/// One line of inter-wave context per completed task.
fn context_line(task: &TaskSpec) -> String {
    let files = task
        .files_to_create
        .iter()
        .chain(&task.files_to_modify)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "- {} (module {}): {} — files: {}",
        task.id, task.module, task.functional_objective, files
    )
}

/// Renders the context section appended to developer prompts. Capped so
/// late waves in large projects do not blow up the prompt.
#[must_use]
pub fn render_wave_context(built: &[String]) -> String {
    if built.is_empty() {
        return String::new();
    }
    let recent: Vec<&String> = built.iter().rev().take(40).collect();
    let mut lines: Vec<&str> = recent.iter().rev().map(|line| line.as_str()).collect();
    let omitted = built.len().saturating_sub(lines.len());
    let mut section = String::from(
        "\n## Context: modules already implemented in earlier waves\n\
         Reuse their interfaces instead of duplicating them; do not modify their files.\n",
    );
    if omitted > 0 {
        let _ = writeln!(section, "(…{omitted} earlier tasks omitted)");
    }
    for line in lines.drain(..) {
        let _ = writeln!(section, "{line}");
    }
    section
}

/// Next model tier for the failure-retry escalation.
#[must_use]
pub fn escalate(complexity: Complexity) -> Option<Complexity> {
    match complexity {
        Complexity::Simple => Some(Complexity::Medium),
        Complexity::Medium => Some(Complexity::Complex),
        Complexity::Complex => None,
    }
}

// ---------- Resume state (#5) ----------

/// Tasks already completed (persisted after every supervised delivery so an
/// interrupted build can resume without repeating work), plus the Improve
/// work branch so a resumed run continues on the SAME branch instead of
/// orphaning it with a fresh checkout.
#[derive(Default)]
pub struct BuildState {
    pub completed: BTreeSet<String>,
    pub improve_branch: Option<String>,
    pub base_branch: Option<String>,
}

impl BuildState {
    fn state_path(project_dir: &Path) -> PathBuf {
        project_dir.join(".multiagent").join("state.json")
    }

    #[must_use]
    pub fn load(project_dir: &Path) -> Self {
        let Some(value) = std::fs::read_to_string(Self::state_path(project_dir))
            .ok()
            .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
        else {
            return Self::default();
        };
        let completed = value
            .get("completed")
            .and_then(|ids| {
                ids.as_array().map(|ids| {
                    ids.iter()
                        .filter_map(|id| id.as_str().map(ToString::to_string))
                        .collect::<BTreeSet<String>>()
                })
            })
            .unwrap_or_default();
        let field = |name: &str| {
            value
                .get(name)
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string)
        };
        Self {
            completed,
            improve_branch: field("improve_branch"),
            base_branch: field("base_branch"),
        }
    }

    pub fn save(&self, project_dir: &Path) {
        let path = Self::state_path(project_dir);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut payload = serde_json::json!({
            "completed": self.completed.iter().collect::<Vec<_>>(),
        });
        if let (Some(map), Some(branch)) = (payload.as_object_mut(), &self.improve_branch) {
            map.insert(
                "improve_branch".to_string(),
                serde_json::Value::String(branch.clone()),
            );
            if let Some(base) = &self.base_branch {
                map.insert(
                    "base_branch".to_string(),
                    serde_json::Value::String(base.clone()),
                );
            }
        }
        // Atomic temp+rename: a crash mid-write must not corrupt the resume
        // state — a truncated state.json would silently restart every task.
        let temp = path.with_extension("json.tmp");
        if std::fs::write(&temp, format!("{payload:#}\n")).is_ok() {
            let _ = std::fs::rename(&temp, &path);
        }
    }
}

fn planning_artifacts_exist(docs: &Path) -> bool {
    docs.join("plan.json").exists() && docs.join("backlog.json").exists()
}

fn load_planning_artifacts(docs: &Path) -> Result<(Plan, Vec<TaskSpec>), String> {
    let plan_raw = std::fs::read_to_string(docs.join("plan.json"))
        .map_err(|error| format!("could not read saved plan: {error}"))?;
    let plan: Plan = serde_json::from_str(&plan_raw)
        .map_err(|error| format!("saved plan did not validate: {error}"))?;
    let backlog_raw = std::fs::read_to_string(docs.join("backlog.json"))
        .map_err(|error| format!("could not read saved backlog: {error}"))?;
    let tasks: Vec<TaskSpec> = serde_json::from_str(&backlog_raw)
        .map_err(|error| format!("saved backlog did not validate: {error}"))?;
    Ok((plan, tasks))
}

// ---------- Budget (#6, disabled unless a ceiling is passed) ----------

/// Optional cost ceiling. Reads the dashboard telemetry JSONL (the same one
/// the UI consumes) and sums `estimated_cost_usd_value` across requests.
/// With no ceiling configured this struct does nothing.
pub struct Budget {
    ceiling: Option<f64>,
    events_path: Option<PathBuf>,
    /// Spend already present in the events file when this run started: the
    /// file persists across runs of the same project, so without a baseline
    /// the ceiling would compare against *lifetime* spend, not this run's.
    baseline: f64,
}

impl Budget {
    #[must_use]
    pub fn new(ceiling: Option<f64>) -> Self {
        let events_path = std::env::var("CLAW_DASHBOARD_EVENTS")
            .ok()
            .map(|path| path.trim().to_string())
            .filter(|path| !path.is_empty())
            .map(PathBuf::from);
        if ceiling.is_some() && events_path.is_none() {
            eprintln!(
                "[multiagent] aviso: --max-cost-usd requiere telemetría activa \
                 (CLAW_DASHBOARD_EVENTS o --dashboard) para medir el gasto; \
                 el tope no se aplicará"
            );
        }
        // Baseline is captured whenever telemetry exists — not only under a
        // ceiling — so the end-of-run summary can report this run's spend.
        let baseline = events_path
            .as_ref()
            .map_or(0.0, |path| sum_cost_from_events(path));
        Self {
            ceiling,
            events_path,
            baseline,
        }
    }

    /// This run's spend so far in USD, when telemetry is active.
    #[must_use]
    pub fn spent(&self) -> Option<f64> {
        let path = self.events_path.as_ref()?;
        Some((sum_cost_from_events(path) - self.baseline).max(0.0))
    }

    #[must_use]
    pub fn enabled(&self) -> bool {
        self.ceiling.is_some() && self.events_path.is_some()
    }

    /// Returns this run's spend when it exceeds the ceiling.
    #[must_use]
    pub fn exceeded(&self) -> Option<f64> {
        let ceiling = self.ceiling?;
        let spent = self.spent()?;
        (spent > ceiling).then_some(spent)
    }
}

/// Sums `estimated_cost_usd_value` across all message_usage events.
#[must_use]
pub fn sum_cost_from_events(path: &Path) -> f64 {
    let Ok(content) = std::fs::read_to_string(path) else {
        return 0.0;
    };
    content
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|event| {
            event
                .get("attributes")
                .and_then(|attributes| attributes.get("estimated_cost_usd_value"))
                .and_then(serde_json::Value::as_f64)
        })
        .sum()
}

// ---------- Build gate (#2) ----------

/// Picks the build command: explicit option first, then auto-detection.
#[must_use]
pub fn detect_build_command(project_dir: &Path, explicit: Option<&str>) -> Option<String> {
    if let Some(command) = explicit {
        let trimmed = command.trim();
        if trimmed.is_empty() || trimmed == "off" {
            return None;
        }
        return Some(trimmed.to_string());
    }
    if project_dir.join("package.json").exists() {
        // The lockfile pins the package manager: installing with a different
        // one rewrites the resolved tree and breaks reproducibility.
        if project_dir.join("pnpm-lock.yaml").exists() {
            return Some("pnpm install && pnpm run build --if-present".to_string());
        }
        if project_dir.join("yarn.lock").exists() {
            return Some("yarn install && yarn run build --if-present".to_string());
        }
        return Some("npm install --no-audit --no-fund && npm run build --if-present".to_string());
    }
    if project_dir.join("Cargo.toml").exists() {
        return Some("cargo build".to_string());
    }
    None
}

/// Runs the build command after a wave; on failure, dispatches the Fixer
/// (superior model) with the compiler output and re-checks once.
fn run_build_gate(
    options: &RunOptions,
    workflow: &WorkflowLog,
    supervision_md: &Path,
    wave_number: usize,
) -> Result<(), String> {
    let Some(command) =
        detect_build_command(&options.project_dir, options.build_command.as_deref())
    else {
        return Ok(());
    };
    workflow.phase(&format!(
        "Build gate tras la ola {wave_number}: `{command}`"
    ));
    let gate_started = Instant::now();
    for attempt in 1..=2 {
        match shell_output(&command, &options.project_dir) {
            Ok(()) => {
                workflow.phase(&format!(
                    "  build gate: OK ({}s)",
                    gate_started.elapsed().as_secs()
                ));
                return Ok(());
            }
            Err(output) if attempt == 1 => {
                workflow.phase("  build gate: FALLÓ → despachando Técnico con los errores");
                append_file(
                    supervision_md,
                    &format!(
                        "\n## Build gate failed after wave {wave_number}\n\n```\n{}\n```\n",
                        truncate_chars(&output, 4_000)
                    ),
                )?;
                let fixer = run_single(
                    Role::Fixer,
                    &options.catalog.supervisor,
                    options,
                    &format!(
                        "The project build failed after wave {wave_number}.\nCommand: \
                         `{command}`\nOutput (tail):\n```\n{}\n```\nFix the build \
                         directly in the repository and report each fix.",
                        truncate_chars(&output, 8_000)
                    ),
                )?;
                append_file(
                    supervision_md,
                    &format!(
                        "\n### Build fixes (wave {wave_number})\n\n{}\n",
                        fixer.report
                    ),
                )?;
            }
            Err(output) => {
                append_file(
                    supervision_md,
                    &format!(
                        "\n## Build gate STILL failing after fixes (wave {wave_number})\n\n\
                         ```\n{}\n```\n",
                        truncate_chars(&output, 4_000)
                    ),
                )?;
                workflow.phase("  build gate: sigue fallando tras el fix (registrado; continúa)");
            }
        }
    }
    Ok(())
}

/// Runs a shell command in the project dir; `Err` carries combined output.
fn shell_output(command: &str, dir: &Path) -> Result<(), String> {
    let wrapped = if cfg!(windows) {
        command.to_string()
    } else {
        // Bound build time so a hung build cannot stall the pipeline.
        format!("timeout 900 sh -c {}", shell_quote(command))
    };
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(&wrapped)
        .current_dir(dir)
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        let mut combined = String::from_utf8_lossy(&output.stdout).to_string();
        combined.push_str(&String::from_utf8_lossy(&output.stderr));
        Err(combined)
    }
}

fn shell_quote(command: &str) -> String {
    format!("'{}'", command.replace('\'', "'\\''"))
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let tail: String = text
        .chars()
        .rev()
        .take(max)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("…{tail}")
}

/// Spawns the Supervisor for one delivery WITHOUT waiting: verdicts are
/// polled by the scheduler so reviews overlap ongoing development.
fn spawn_supervisor(
    options: &RunOptions,
    task: &TaskSpec,
    delivery: &AgentResult,
) -> Result<AgentHandle, String> {
    spawn_agent(
        &format!("sup-{}", task.id),
        Role::Supervisor.title(),
        &options.catalog.supervisor,
        Role::Supervisor.subagent_type(),
        &system_prompt(Role::Supervisor, options.kind),
        &format!(
            "Task delivered:\n```json\n{}\n```\n\nDeveloper report:\n{}\n\nDelivery \
             status: {}. Inspect the repository files this task touched. Evaluate \
             quality, architecture, conventions, duplication and coverage. \
             Mandatory security lens: XSS, injection (SQL/command), authorization \
             gaps, hardcoded secrets, unsafe CORS/CSP, cookies without httpOnly.\n\
             Respond with ```json {{\"approved\": bool, \"summary\": \"...\", \
             \"issues\": [{{\"severity\": \"low|medium|high\", \"file\": \"...\", \
             \"description\": \"...\", \"suggested_fix\": \"...\"}}]}} ```",
            serde_json::to_string(task).unwrap_or_default(),
            delivery.report,
            delivery.status,
        ),
        &[],
    )
}

/// Processes a finished Supervisor: records the verdict in SUPERVISION.md,
/// triggers the reindex hook, and dispatches the Fixer (superior model)
/// with the exact issue list when the review found problems.
fn process_supervision(
    options: &RunOptions,
    workflow: &WorkflowLog,
    supervision_md: &Path,
    task: &TaskSpec,
    delivery: &AgentResult,
    supervisor: &AgentResult,
) -> Result<usize, String> {
    let Some(verdict) = Some(supervisor)
        .filter(|result| result.succeeded())
        .and_then(|result| extract_json(&result.report))
        .and_then(|value| serde_json::from_value::<SupervisionVerdict>(value).ok())
    else {
        // An unreadable verdict must not pass as "no issues": that would
        // silently neutralize the review gate.
        workflow.phase(&format!(
            "  supervisor de {}: veredicto ilegible — se registra como issue",
            task.id
        ));
        append_file(
            supervision_md,
            &format!(
                "\n## Task {} — {}\n\n- Status: {}\n- Approved: false\n\
                 - Summary: unreadable supervisor verdict (JSON missing/invalid)\n",
                task.id, task.module, delivery.status
            ),
        )?;
        return Ok(1);
    };

    // Registro a registro: append the verdict to SUPERVISION.md.
    let mut entry = format!(
        "\n## Task {} — {}\n\n- Status: {}\n- Approved: {}\n- Summary: {}\n",
        task.id, task.module, delivery.status, verdict.approved, verdict.summary
    );
    for issue in &verdict.issues {
        let _ = writeln!(
            entry,
            "- [{}] `{}` — {} (fix: {})",
            issue.severity, issue.file, issue.description, issue.suggested_fix
        );
    }
    append_file(supervision_md, &entry)?;

    // Reindex hook (codebase-memory): pluggable command so the knowledge
    // graph stays current after every delivery.
    run_reindex_hook();

    if !verdict.issues.is_empty() {
        workflow.phase(&format!(
            "  supervisor: {} issue(s) en {} → despachando Técnico",
            verdict.issues.len(),
            task.id
        ));
        let issues_json = serde_json::to_string_pretty(&verdict.issues).unwrap_or_default();
        let fixer = run_single(
            Role::Fixer,
            &options.catalog.supervisor,
            options,
            &format!(
                "The Supervisor found these issues in task {} (module {}):\n```json\n\
                 {issues_json}\n```\nApply the fixes directly in the repository. \
                 Report each fix applied.",
                task.id, task.module
            ),
        )?;
        append_file(
            supervision_md,
            &format!("\n### Fixes applied for {}\n\n{}\n", task.id, fixer.report),
        )?;
    }
    Ok(verdict.issues.len())
}

/// Runs one agent to completion and returns its result.
fn run_single(
    role: Role,
    model: &str,
    options: &RunOptions,
    prompt: &str,
) -> Result<AgentResult, String> {
    let handle = spawn_agent(
        role.slug(),
        role.title(),
        model,
        role.subagent_type(),
        &system_prompt(role, options.kind),
        prompt,
        &[],
    )?;
    let mut results = wait_all(vec![handle], options.agent_timeout, |_| {});
    let result = results.pop().ok_or("agent produced no result")?;
    // "failed" must not flow downstream as a report: a provider error or
    // agent panic would otherwise be saved/parsed as a real deliverable.
    if !result.succeeded() {
        return Err(format!(
            "{} {}: {}",
            role.title(),
            result.status,
            result
                .error
                .clone()
                .unwrap_or_else(|| "no error detail".to_string())
        ));
    }
    Ok(result)
}

/// Architects relevant to the project: software + devops + UX always; the
/// frontend/backend pair only when the stack is not a simple static site.
fn architects_for(kind: ProjectKind, plan: &Plan) -> Vec<Role> {
    let mut roles = vec![
        Role::SoftwareArchitect,
        Role::UxUiDesigner,
        Role::DevOpsArchitect,
    ];
    let simple = plan.stack.kind.eq_ignore_ascii_case("simple");
    if !simple {
        roles.push(Role::FrontendArchitect);
        roles.push(Role::BackendArchitect);
    } else if kind == ProjectKind::Web {
        roles.push(Role::FrontendArchitect);
    }
    roles
}

/// Optional reindex hook: set `CLAW_MULTIAGENT_REINDEX_CMD` to a shell
/// command (e.g. a codebase-memory MCP reindex invocation) and it runs
/// after every supervised delivery.
fn run_reindex_hook() {
    let Ok(command) = std::env::var("CLAW_MULTIAGENT_REINDEX_CMD") else {
        return;
    };
    if command.trim().is_empty() {
        return;
    }
    let _ = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

fn save_doc(docs: &Path, name: &str, content: &str) -> Result<(), String> {
    std::fs::write(docs.join(name), content).map_err(|error| error.to_string())
}

fn append_file(path: &Path, content: &str) -> Result<(), String> {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    file.write_all(content.as_bytes())
        .map_err(|error| error.to_string())
}

/// Any credential the agent runners could authenticate with.
fn has_any_provider_credentials() -> bool {
    [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "OPENAI_API_KEY",
        "XAI_API_KEY",
        "DASHSCOPE_API_KEY",
        "OLLAMA_HOST",
    ]
    .iter()
    .any(|name| {
        std::env::var(name)
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
    })
}

fn init_git_repo(project_dir: &Path) -> Result<(), String> {
    let _status = std::process::Command::new("git")
        .arg("init")
        .current_dir(project_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|error| format!("git init failed: {error}"))?;
    Ok(())
}

/// Checks out the dedicated branch for an Improve run so the per-task
/// commits never land on the user's current branch. A fresh run records the
/// forked-from branch and creates a new one; `--resume` re-checks-out the
/// branch persisted in `.multiagent/state.json` so every resume continues
/// the SAME branch instead of orphaning it. Returns `(branch, base)`, or
/// `None` when git is unavailable or the checkout failed.
fn ensure_improve_branch(project_dir: &Path, resume: bool) -> Option<(String, Option<String>)> {
    let mut state = BuildState::load(project_dir);
    if resume {
        if let Some(branch) = state.improve_branch.clone() {
            if git_checkout(project_dir, &branch) {
                return Some((branch, state.base_branch.clone()));
            }
            // Branch deleted since the last run: fall through and start anew.
        }
    }
    let base = current_git_branch(project_dir);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    let branch = format!("multiagent/improve-{}", nanos % 1_000_000_000);
    let status = std::process::Command::new("git")
        .args(["checkout", "-b", &branch])
        .current_dir(project_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    state.improve_branch = Some(branch.clone());
    state.base_branch = base.clone();
    state.save(project_dir);
    Some((branch, base))
}

/// The current branch name; works on an unborn HEAD too (fresh repo).
fn current_git_branch(project_dir: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["symbolic-ref", "--short", "-q", "HEAD"])
        .current_dir(project_dir)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !branch.is_empty()).then_some(branch)
}

fn git_checkout(project_dir: &Path, branch: &str) -> bool {
    std::process::Command::new("git")
        .args(["checkout", branch])
        .current_dir(project_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn git_commit(project_dir: &Path, message: &str) -> Result<(), String> {
    let status = std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(project_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|error| format!("git add failed: {error}"))?;
    if !status.success() {
        return Ok(());
    }
    // Explicit identity: without it, commits fail silently on machines
    // where git user.name/user.email were never configured.
    let _status = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Claw Multiagent",
            "-c",
            "user.email=multiagent@claw.local",
            "commit",
            "-m",
            message,
            "--quiet",
        ])
        .current_dir(project_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|error| format!("git commit failed: {error}"))?;
    Ok(())
}

/// Parses the agent's report into `T`; on validation failure, re-runs the
/// role once with a prompt that carries the full original context, the
/// faulty output and the exact error (agents are stateless — a bare "fix
/// your previous response" would reach an agent that never saw it).
fn retry_parse_json_with_feedback<T: serde::de::DeserializeOwned>(
    result: &AgentResult,
    who: &str,
    role: Role,
    schema: &str,
    options: &RunOptions,
    workflow: &WorkflowLog,
) -> Result<T, String> {
    let value = extract_json(&result.report)
        .ok_or_else(|| format!("{who} produced no JSON (status {})", result.status))?;
    let parse_error = match serde_json::from_value::<T>(value) {
        Ok(parsed) => return Ok(parsed),
        Err(error) => error.to_string(),
    };
    workflow.phase(&format!(
        "  {who}: JSON validation falló, reintentando con retroalimentación"
    ));
    let retry_report = run_single(
        role,
        &options.catalog.director,
        options,
        &format!(
            "User prompt:\n{}\n\nA previous attempt produced this response \
             (truncated):\n```\n{}\n```\n\nIts JSON failed validation with:\n\
             {parse_error}\n\nProduce the corrected, complete response.\n{schema}",
            options.prompt,
            truncate_chars(&result.report, 6_000)
        ),
    )?;
    let value = extract_json(&retry_report.report)
        .ok_or_else(|| format!("{who} retry produced no JSON"))?;
    serde_json::from_value(value)
        .map_err(|error| format!("{who} retry JSON still invalid: {error}"))
}

/// Extracts the `{"tasks": [...]}` backlog from the Subdirector's report.
fn parse_backlog(report: &str) -> Result<Vec<TaskSpec>, String> {
    let value = extract_json(report).ok_or("no JSON backlog in the response")?;
    let tasks_value = value.get("tasks").cloned().unwrap_or(value);
    serde_json::from_value(tasks_value)
        .map_err(|error| format!("backlog JSON did not validate: {error}"))
}

/// Structural sanity checks on the backlog before spending developer runs.
/// Also rejects broken dependency graphs (duplicates, unknown/self deps,
/// cycles): `schedule_waves` degrades instead of hanging on them, but they
/// always indicate a Subdirector error worth a repair round.
fn validate_backlog(tasks: &[TaskSpec]) -> Result<(), String> {
    let mut issues = Vec::new();
    if tasks.is_empty() {
        issues.push("the backlog is empty".to_string());
    }
    let mut seen_ids = BTreeSet::new();
    for task in tasks {
        if !seen_ids.insert(task.id.as_str()) {
            issues.push(format!("{}: duplicate task id", task.id));
        }
        if task.files_to_create.is_empty() && task.files_to_modify.is_empty() {
            issues.push(format!(
                "{}: no files to create or modify (orphan task)",
                task.id
            ));
        }
        if task.functional_objective.is_empty() {
            issues.push(format!("{}: missing functional objective", task.id));
        }
        if task.files_to_create.len() + task.files_to_modify.len() > 30 {
            issues.push(format!(
                "{}: too many files ({}) — split into smaller tasks",
                task.id,
                task.files_to_create.len() + task.files_to_modify.len()
            ));
        }
        for dep_id in &task.depends_on {
            if dep_id == &task.id {
                issues.push(format!("{}: depends on itself", task.id));
            } else if !tasks.iter().any(|t| &t.id == dep_id) {
                issues.push(format!(
                    "{}: depends on unknown task \"{dep_id}\" (typo or missing task)",
                    task.id
                ));
            }
        }
    }
    if let Some(cycle_id) = find_dependency_cycle(tasks) {
        issues.push(format!(
            "dependency cycle involving \"{cycle_id}\" — depends_on must form a DAG"
        ));
    }
    if !issues.is_empty() {
        return Err(format!("Backlog validation failed:\n{}", issues.join("\n")));
    }
    Ok(())
}

/// Kahn's algorithm: returns a task id inside a `depends_on` cycle, if any.
/// Self-dependencies and unknown ids are excluded (flagged separately) and
/// duplicate edges/ids are tolerated without underflow.
fn find_dependency_cycle(tasks: &[TaskSpec]) -> Option<String> {
    let exists = |id: &str| tasks.iter().any(|t| t.id == id);
    let mut in_degree = vec![0_usize; tasks.len()];
    for (index, task) in tasks.iter().enumerate() {
        for dep_id in &task.depends_on {
            if dep_id != &task.id && exists(dep_id) {
                in_degree[index] += 1;
            }
        }
    }
    let mut queue: Vec<usize> = (0..tasks.len()).filter(|&i| in_degree[i] == 0).collect();
    let mut visited = 0_usize;
    while let Some(done) = queue.pop() {
        visited += 1;
        for (index, task) in tasks.iter().enumerate() {
            for dep_id in &task.depends_on {
                if dep_id == &tasks[done].id && dep_id != &task.id && in_degree[index] > 0 {
                    in_degree[index] -= 1;
                    if in_degree[index] == 0 {
                        queue.push(index);
                    }
                }
            }
        }
    }
    if visited >= tasks.len() {
        return None;
    }
    in_degree
        .iter()
        .enumerate()
        .find(|(_, degree)| **degree > 0)
        .map(|(index, _)| tasks[index].id.clone())
}

fn run_tests_for_qa(project_dir: &Path, build_cmd: &Option<String>) -> Result<String, String> {
    let test_cmd = match build_cmd.as_deref() {
        Some("off") => return Ok("tests skipped (disabled)".to_string()),
        Some(cmd) if cmd.contains("npm") => "npm test".to_string(),
        Some(cmd) if cmd.contains("cargo") => "cargo test".to_string(),
        // Unknown custom build command: don't guess a test runner — a wrong
        // guess reads as a test failure and wastes a Fixer run.
        Some(cmd) => return Ok(format!("tests skipped (no known test runner for '{cmd}')")),
        None => {
            if project_dir.join("Cargo.toml").exists() {
                "cargo test".to_string()
            } else if project_dir.join("package.json").exists() {
                "npm test".to_string()
            } else {
                return Ok("no test command detected".to_string());
            }
        }
    };
    // Same bound as the build gate: a hung test run (e.g. a watch-mode
    // default) must not stall the pipeline forever.
    let wrapped = if cfg!(windows) {
        test_cmd.clone()
    } else {
        format!("timeout 900 sh -c {}", shell_quote(&test_cmd))
    };
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(&wrapped)
        .current_dir(project_dir)
        .output()
        .map_err(|error| format!("test execution failed: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if output.status.success() {
        Ok(format!("✓ tests passed\n{}", stdout))
    } else {
        Err(format!(
            "✗ tests failed ({test_cmd}):\nstdout:\n{}\nstderr:\n{}",
            stdout, stderr
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn improve_temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "multiagent-improve-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        dir
    }

    #[test]
    fn project_has_sources_detects_code_and_rejects_empty() {
        let empty = improve_temp_dir("empty");
        assert!(!project_has_sources(&empty));
        // A dir with only node_modules/docs is still "empty" for improve.
        std::fs::create_dir_all(empty.join("node_modules")).expect("nm");
        std::fs::write(empty.join("node_modules/x.js"), "ignored").expect("w");
        assert!(!project_has_sources(&empty));

        let project = improve_temp_dir("withcode");
        std::fs::create_dir_all(project.join("src")).expect("src");
        std::fs::write(project.join("src/main.ts"), "export const x = 1;").expect("w");
        assert!(project_has_sources(&project));

        let _ = std::fs::remove_dir_all(empty);
        let _ = std::fs::remove_dir_all(project);
    }

    #[test]
    fn repo_digest_lists_tree_and_inlines_manifests() {
        let project = improve_temp_dir("digest");
        std::fs::create_dir_all(project.join("src/auth")).expect("dirs");
        std::fs::write(
            project.join("package.json"),
            r#"{"name":"demo","dependencies":{"react":"19"}}"#,
        )
        .expect("pkg");
        std::fs::write(project.join("src/auth/login.ts"), "// login").expect("w");
        // Skipped dirs must not appear in the digest.
        std::fs::create_dir_all(project.join("node_modules/dep")).expect("nm");
        std::fs::write(project.join("node_modules/dep/index.js"), "big").expect("w");

        let digest = repo_digest(&project);
        assert!(digest.contains("src/auth/login.ts"), "tree lists sources");
        assert!(digest.contains("package.json"), "manifest listed");
        assert!(digest.contains("\"react\""), "manifest contents inlined");
        assert!(
            !digest.contains("node_modules"),
            "dependency dir must be skipped: {digest}"
        );
        let _ = std::fs::remove_dir_all(project);
    }

    #[test]
    fn build_mode_is_improve() {
        assert!(BuildMode::Improve.is_improve());
        assert!(!BuildMode::Greenfield.is_improve());
        assert!(!BuildMode::default().is_improve());
    }

    #[test]
    fn simple_web_stack_skips_backend_architect() {
        let mut plan = Plan::default();
        plan.stack.kind = "simple".to_string();
        let roles = architects_for(ProjectKind::Web, &plan);
        assert!(roles.contains(&Role::FrontendArchitect));
        assert!(!roles.contains(&Role::BackendArchitect));
    }

    #[test]
    fn fullstack_includes_both_architect_pairs() {
        let mut plan = Plan::default();
        plan.stack.kind = "fullstack".to_string();
        let roles = architects_for(ProjectKind::App, &plan);
        assert!(roles.contains(&Role::FrontendArchitect));
        assert!(roles.contains(&Role::BackendArchitect));
        assert!(roles.contains(&Role::DevOpsArchitect));
    }

    #[test]
    fn escalation_climbs_tiers_and_stops_at_complex() {
        assert_eq!(escalate(Complexity::Simple), Some(Complexity::Medium));
        assert_eq!(escalate(Complexity::Medium), Some(Complexity::Complex));
        assert_eq!(escalate(Complexity::Complex), None);
    }

    #[test]
    fn write_scope_derives_module_directories() {
        let task = TaskSpec {
            files_to_create: vec![
                "src/auth/login.ts".to_string(),
                "src/auth/session.ts".to_string(),
                "tests/auth.spec.ts".to_string(),
            ],
            files_to_modify: vec!["index.html".to_string()],
            ..TaskSpec::default()
        };
        let scope = write_scope_for(&task);
        assert!(scope.contains(&"src/auth".to_string()));
        assert!(scope.contains(&"tests".to_string()));
        assert!(scope.contains(&"index.html".to_string()));
        assert_eq!(scope.len(), 3);
    }

    #[test]
    fn wave_context_lists_built_modules_and_caps_size() {
        assert!(render_wave_context(&[]).is_empty());
        let built: Vec<String> = (0..50).map(|i| format!("- T{i} (m{i}): x")).collect();
        let context = render_wave_context(&built);
        assert!(context.contains("already implemented"));
        assert!(context.contains("- T49"));
        assert!(context.contains("(…10 earlier tasks omitted)"));
        assert!(!context.contains("- T5 "));
    }

    #[test]
    fn build_state_round_trips_completed_ids() {
        let dir = std::env::temp_dir().join(format!(
            "multiagent-state-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        let mut state = BuildState::load(&dir);
        assert!(state.completed.is_empty());
        state.completed.insert("T1".to_string());
        state.completed.insert("T2".to_string());
        state.save(&dir);
        let reloaded = BuildState::load(&dir);
        assert_eq!(reloaded.completed.len(), 2);
        assert!(reloaded.completed.contains("T1"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn budget_sums_cost_from_events_file() {
        let dir = std::env::temp_dir().join(format!(
            "multiagent-budget-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        let events = dir.join("events.jsonl");
        std::fs::write(
            &events,
            concat!(
                r#"{"type":"session_trace","session_id":"a","sequence":0,"name":"analytics","timestamp_ms":1,"attributes":{"namespace":"api","action":"message_usage","estimated_cost_usd_value":0.25}}"#,
                "\n",
                r#"{"type":"session_trace","session_id":"b","sequence":1,"name":"analytics","timestamp_ms":2,"attributes":{"namespace":"api","action":"message_usage","estimated_cost_usd_value":0.50}}"#,
                "\n",
                "not json\n",
            ),
        )
        .expect("events");
        assert!((sum_cost_from_events(&events) - 0.75).abs() < 1e-9);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn budget_disabled_without_ceiling() {
        let budget = Budget {
            ceiling: None,
            events_path: Some(PathBuf::from("/nonexistent")),
            baseline: 0.0,
        };
        assert!(!budget.enabled());
        assert!(budget.exceeded().is_none());
    }

    #[test]
    fn budget_ignores_spend_from_previous_runs() {
        let dir = std::env::temp_dir().join(format!(
            "multiagent-baseline-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        let events = dir.join("events.jsonl");
        let event = |cost: f64| {
            format!(
                r#"{{"type":"session_trace","session_id":"x","sequence":0,"name":"analytics","timestamp_ms":1,"attributes":{{"namespace":"api","action":"message_usage","estimated_cost_usd_value":{cost}}}}}"#
            )
        };
        // 5 USD already spent by earlier runs before this Budget exists.
        std::fs::write(&events, format!("{}\n", event(5.0))).expect("events");
        let budget = Budget {
            ceiling: Some(1.0),
            events_path: Some(events.clone()),
            baseline: sum_cost_from_events(&events),
        };
        assert!(budget.exceeded().is_none(), "old spend must not count");
        // This run spends 1.5 USD → over the 1.0 ceiling.
        let mut content = std::fs::read_to_string(&events).expect("read");
        content.push_str(&format!("{}\n", event(1.5)));
        std::fs::write(&events, content).expect("append");
        assert!(budget.exceeded().is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn build_command_detection_and_off_switch() {
        let dir = std::env::temp_dir().join(format!(
            "multiagent-build-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        assert_eq!(detect_build_command(&dir, None), None);
        std::fs::write(dir.join("package.json"), "{}").expect("pkg");
        assert!(detect_build_command(&dir, None)
            .expect("npm build")
            .starts_with("npm install"));
        assert_eq!(
            detect_build_command(&dir, Some("make check")),
            Some("make check".to_string())
        );
        assert_eq!(detect_build_command(&dir, Some("off")), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn build_command_prefers_lockfile_package_manager() {
        let dir = std::env::temp_dir().join(format!(
            "multiagent-lockfile-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("package.json"), "{}").expect("pkg");
        assert!(detect_build_command(&dir, None)
            .expect("npm build")
            .starts_with("npm install"));
        std::fs::write(dir.join("yarn.lock"), "").expect("yarn lock");
        assert!(detect_build_command(&dir, None)
            .expect("yarn build")
            .starts_with("yarn install"));
        // pnpm wins even when a stale yarn.lock lingers alongside it.
        std::fs::write(dir.join("pnpm-lock.yaml"), "").expect("pnpm lock");
        assert!(detect_build_command(&dir, None)
            .expect("pnpm build")
            .starts_with("pnpm install"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn perf_budget_parses_override_and_rejects_junk() {
        assert_eq!(parse_perf_budget_kb(None), DEFAULT_BUNDLE_BUDGET_KB);
        assert_eq!(parse_perf_budget_kb(Some("250")), 250);
        assert_eq!(parse_perf_budget_kb(Some(" 1024 ")), 1024, "trims");
        // Zero would disable the gate; junk must not change it silently.
        assert_eq!(parse_perf_budget_kb(Some("0")), DEFAULT_BUNDLE_BUDGET_KB);
        assert_eq!(parse_perf_budget_kb(Some("-5")), DEFAULT_BUNDLE_BUDGET_KB);
        assert_eq!(parse_perf_budget_kb(Some("huge")), DEFAULT_BUNDLE_BUDGET_KB);
        assert_eq!(parse_perf_budget_kb(Some("")), DEFAULT_BUNDLE_BUDGET_KB);
    }

    #[cfg(unix)]
    #[test]
    fn smoke_timeout_parses_override_and_rejects_junk() {
        assert_eq!(parse_smoke_timeout_secs(None), DEFAULT_SMOKE_TIMEOUT_SECS);
        assert_eq!(parse_smoke_timeout_secs(Some("120")), 120);
        assert_eq!(
            parse_smoke_timeout_secs(Some("0")),
            DEFAULT_SMOKE_TIMEOUT_SECS
        );
        assert_eq!(
            parse_smoke_timeout_secs(Some("soon")),
            DEFAULT_SMOKE_TIMEOUT_SECS
        );
    }

    #[test]
    fn parse_backlog_accepts_wrapped_and_bare_task_arrays() {
        let wrapped = r#"Backlog: {"tasks": [{"id": "T1", "module": "auth",
            "functional_objective": "login", "files_to_create": ["src/a.ts"]}]}"#;
        let tasks = parse_backlog(wrapped).expect("wrapped backlog");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "T1");
        assert!(parse_backlog("no json at all").is_err());
    }

    #[test]
    fn validate_backlog_flags_empty_orphan_and_oversized_tasks() {
        assert!(validate_backlog(&[]).unwrap_err().contains("empty"));

        let orphan = TaskSpec {
            id: "T1".to_string(),
            functional_objective: "x".to_string(),
            ..TaskSpec::default()
        };
        assert!(validate_backlog(std::slice::from_ref(&orphan))
            .unwrap_err()
            .contains("orphan"));

        let oversized = TaskSpec {
            id: "T2".to_string(),
            functional_objective: "x".to_string(),
            files_to_create: (0..31).map(|i| format!("src/f{i}.ts")).collect(),
            ..TaskSpec::default()
        };
        assert!(validate_backlog(std::slice::from_ref(&oversized))
            .unwrap_err()
            .contains("too many files"));

        let ok = TaskSpec {
            id: "T3".to_string(),
            functional_objective: "login".to_string(),
            files_to_create: vec!["src/auth/login.ts".to_string()],
            ..TaskSpec::default()
        };
        assert!(validate_backlog(std::slice::from_ref(&ok)).is_ok());
    }

    fn spec(id: &str, file: &str, deps: &[&str], priority: u32) -> TaskSpec {
        TaskSpec {
            id: id.to_string(),
            functional_objective: format!("build {id}"),
            files_to_create: vec![file.to_string()],
            depends_on: deps.iter().map(ToString::to_string).collect(),
            priority,
            ..TaskSpec::default()
        }
    }

    #[test]
    fn scheduler_respects_dependencies_and_file_locks() {
        let tasks = vec![
            spec("T1", "src/a.ts", &[], 1),
            spec("T2", "src/b.ts", &["T1"], 1),
            spec("T3", "src/a.ts", &[], 2),
        ];
        let mut completed = BTreeSet::new();
        let failed = BTreeSet::new();
        let mut busy = BTreeSet::new();

        // T2 blocked by dependency; T3 shares a file with T1 → T1 first.
        assert_eq!(next_ready_task(&tasks, &completed, &failed, &busy), Some(0));
        busy.insert(0);
        // T1 running: T3 conflicts on src/a.ts, T2 dependency pending → none.
        assert_eq!(next_ready_task(&tasks, &completed, &failed, &busy), None);
        // T1 delivered and supervised: both unblock; priority 1 (T2) wins.
        busy.remove(&0);
        completed.insert("T1".to_string());
        assert_eq!(next_ready_task(&tasks, &completed, &failed, &busy), Some(1));
        busy.insert(1);
        assert_eq!(next_ready_task(&tasks, &completed, &failed, &busy), Some(2));
    }

    #[test]
    fn scheduler_blocks_dependents_of_failed_tasks() {
        let tasks = vec![
            spec("T1", "src/a.ts", &[], 1),
            spec("T2", "src/b.ts", &["T1"], 1),
            spec("T3", "src/c.ts", &[], 2),
        ];
        let completed = BTreeSet::new();
        let mut failed = BTreeSet::new();
        failed.insert(0);
        // T2 would build on T1's missing foundation → blocked; the
        // independent T3 still runs.
        assert_eq!(
            next_ready_task(&tasks, &completed, &failed, &BTreeSet::new()),
            Some(2)
        );
        let mut failed_all = failed.clone();
        failed_all.insert(2);
        assert_eq!(
            next_ready_task(&tasks, &completed, &failed_all, &BTreeSet::new()),
            None
        );
    }

    #[test]
    fn scaffold_kind_matches_stack() {
        let mut plan = Plan::default();
        plan.stack.frontend = vec!["React".to_string()];
        assert_eq!(
            scaffold_kind_for(&plan),
            Some(ScaffoldKind::Vite("react-ts"))
        );

        // Meta-frameworks with interactive CLIs are left to the agents.
        plan.stack.framework = vec!["Next.js".to_string()];
        assert_eq!(scaffold_kind_for(&plan), None);

        let mut rust_plan = Plan::default();
        rust_plan.stack.backend = vec!["Rust (axum)".to_string()];
        assert_eq!(scaffold_kind_for(&rust_plan), Some(ScaffoldKind::CargoInit));

        let mut node_plan = Plan::default();
        node_plan.stack.backend = vec!["Express".to_string()];
        assert_eq!(scaffold_kind_for(&node_plan), Some(ScaffoldKind::NpmInit));

        assert_eq!(scaffold_kind_for(&Plan::default()), None);
    }

    #[test]
    fn build_state_round_trips_improve_branch() {
        let dir = std::env::temp_dir().join(format!(
            "multiagent-state-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("dir");

        // No state file yet: everything empty.
        let empty = BuildState::load(&dir);
        assert!(empty.completed.is_empty());
        assert_eq!(empty.improve_branch, None);

        let mut state = BuildState::default();
        state.completed.insert("T1".to_string());
        state.improve_branch = Some("multiagent/improve-42".to_string());
        state.base_branch = Some("main".to_string());
        state.save(&dir);

        // A --resume must find the SAME branch, not mint a new one.
        let loaded = BuildState::load(&dir);
        assert!(loaded.completed.contains("T1"));
        assert_eq!(
            loaded.improve_branch.as_deref(),
            Some("multiagent/improve-42")
        );
        assert_eq!(loaded.base_branch.as_deref(), Some("main"));

        // Branch fields are optional in the payload: absent stays None.
        let mut bare = BuildState::default();
        bare.completed.insert("T2".to_string());
        bare.save(&dir);
        let reloaded = BuildState::load(&dir);
        assert_eq!(reloaded.improve_branch, None);
        assert_eq!(reloaded.base_branch, None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn smoke_command_prefers_dev_script() {
        let dir = std::env::temp_dir().join(format!(
            "multiagent-smoke-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        assert_eq!(smoke_command(&dir), None);
        std::fs::write(
            dir.join("package.json"),
            r#"{"scripts": {"start": "node .", "dev": "vite"}}"#,
        )
        .expect("pkg");
        assert_eq!(smoke_command(&dir), Some("npm run dev".to_string()));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn merge_missing_never_overwrites_existing_files() {
        let root = std::env::temp_dir().join(format!(
            "multiagent-merge-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let staging = root.join(".scaffold");
        let target = root.join("project");
        std::fs::create_dir_all(staging.join("src")).expect("staging");
        std::fs::create_dir_all(target.join("src")).expect("target");
        std::fs::write(staging.join("index.html"), "from template").expect("w");
        std::fs::write(staging.join("src/main.ts"), "template main").expect("w");
        std::fs::write(target.join("index.html"), "user content").expect("w");

        merge_missing(&staging, &target);

        // Existing file untouched; missing file (inside shared dir) copied.
        assert_eq!(
            std::fs::read_to_string(target.join("index.html")).expect("r"),
            "user content"
        );
        assert_eq!(
            std::fs::read_to_string(target.join("src/main.ts")).expect("r"),
            "template main"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn qa_tests_skip_when_disabled_unknown_or_undetected() {
        let dir = std::env::temp_dir().join(format!(
            "multiagent-qa-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        // Disabled explicitly.
        assert!(run_tests_for_qa(&dir, &Some("off".to_string()))
            .expect("off")
            .contains("skipped"));
        // Unknown custom runner: skip instead of guessing "<cmd> test".
        assert!(run_tests_for_qa(&dir, &Some("make build".to_string()))
            .expect("unknown runner")
            .contains("skipped"));
        // Nothing to detect in an empty project.
        assert!(run_tests_for_qa(&dir, &None)
            .expect("no manifest")
            .contains("no test command"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
