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
use std::time::Duration;

use telemetry::{AnalyticsEvent, JsonlTelemetrySink, SessionTracer};

use crate::agents::{extract_json, spawn_agent, wait_all, AgentResult};
use crate::catalog::ModelCatalog;
use crate::contracts::{
    schedule_waves, Complexity, Plan, ProjectKind, SupervisionVerdict, TaskSpec,
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

#[allow(clippy::too_many_lines)]
pub fn run(options: &RunOptions) -> Result<RunSummary, String> {
    options.catalog.validate_credentials()?;

    let aborted = Arc::new(AtomicBool::new(false));
    let aborted_clone = Arc::clone(&aborted);
    let _ = ctrlc::set_handler(move || {
        aborted_clone.store(true, Ordering::Relaxed);
    });

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

    let _ = init_git_repo(&options.project_dir, &workflow);

    let budget = Budget::new(options.max_cost_usd);
    if budget.enabled() {
        workflow.phase(&format!(
            "Presupuesto activo: tope {:.2} USD",
            options.max_cost_usd.unwrap_or_default()
        ));
    }

    // ---- Planning (Director → open questions → Architects → Subdirector),
    //      or reuse from disk when resuming. ----
    let (plan, tasks) = if options.resume && planning_artifacts_exist(&docs) {
        workflow.phase("Reanudando: reutilizando plan, diseños y backlog guardados");
        load_planning_artifacts(&docs)?
    } else {
        // Phase 1: Director General.
        workflow.phase("Director General: interpretando el prompt y creando el plan");
        let director_report = run_single(
            Role::Director,
            &options.catalog.director,
            options,
            &format!(
                "User prompt:\n{}\n\nProduce the complete product plan.\n{}",
                options.prompt, DIRECTOR_JSON_SCHEMA
            ),
        )?;
        let plan: Plan = retry_parse_json_with_feedback(
            &director_report,
            "Director",
            Role::Director,
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
                    "Plan:\n```json\n{plan_json}\n```{decisions}\n\nDeliver your \
                     complete design document in Markdown, justifying every decision."
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
        let subdirector_report = run_single(
            Role::Subdirector,
            &options.catalog.director,
            options,
            &format!(
                "Approved plan:\n```json\n{plan_json}\n```{decisions}\n\nArchitect \
                 designs (authoritative):{designs_digest}\n\nBreak the project into \
                 fully specified, parallelizable TaskSpecs that implement these \
                 designs.\n{SUBDIRECTOR_JSON_SCHEMA}"
            ),
        )?;
        let mut tasks_value = extract_json(&subdirector_report.report)
            .ok_or("Subdirector produced no JSON backlog")?;
        let mut tasks: Result<Vec<TaskSpec>, serde_json::Error> = serde_json::from_value(
            tasks_value
                .get("tasks")
                .cloned()
                .unwrap_or_else(|| tasks_value.clone()),
        );
        if tasks.is_err() {
            workflow
                .phase("  Subdirector: JSON validation falló, reintentando con retroalimentación");
            let parse_error = tasks.as_ref().err().unwrap().to_string();
            let retry_report = run_single(
                Role::Subdirector,
                &options.catalog.director,
                options,
                &format!(
                    "Your previous response failed validation:\n\n{}\n\n\
                     Please correct it and produce valid JSON again, ensuring \
                     the structure is `{{\"tasks\": [...]}}` with each task \
                     fully specified.",
                    parse_error
                ),
            )?;
            tasks_value = extract_json(&retry_report.report)
                .ok_or("Subdirector retry produced no JSON backlog")?;
            tasks =
                serde_json::from_value(tasks_value.get("tasks").cloned().unwrap_or(tasks_value));
        }
        let tasks = tasks.map_err(|error| format!("Subdirector backlog invalid: {error}"))?;
        if tasks.is_empty() {
            return Err("Subdirector produced an empty backlog".to_string());
        }
        validate_backlog_against_designs(&tasks)?;
        save_doc(
            &docs,
            "backlog.json",
            &serde_json::to_string_pretty(&tasks).unwrap_or_default(),
        )?;
        (plan, tasks)
    };

    // Instruction files per model family (CLAUDE.md / AGENTS.md / GROK.md).
    let instruction_files =
        write_role_instruction_files(&options.project_dir, &options.catalog, options.kind)?;
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
        });
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
    // Inter-wave context: what earlier waves already built.
    let mut built_context: Vec<String> = tasks
        .iter()
        .filter(|task| state.completed.contains(&task.id))
        .map(context_line)
        .collect();

    'waves: for (wave_index, wave) in waves.iter().enumerate() {
        if aborted.load(Ordering::Relaxed) {
            workflow.phase("Construcción abortada por usuario (Ctrl+C)");
            save_doc(&docs, "ABORT.txt", "Build interrupted by user\n")?;
            break 'waves;
        }
        if let Some(spent) = budget.exceeded() {
            workflow.phase(&format!(
                "Presupuesto superado ({spent:.2} USD): abortando limpiamente antes de la ola {}",
                wave_index + 1
            ));
            budget_aborted = true;
            break 'waves;
        }
        let pending: Vec<usize> = wave
            .iter()
            .copied()
            .filter(|&index| !state.completed.contains(&tasks[index].id))
            .collect();
        if pending.is_empty() {
            continue;
        }
        workflow.phase(&format!(
            "Ola {}/{}: {} tareas en paralelo",
            wave_index + 1,
            waves.len(),
            pending.len()
        ));
        let wave_context = render_wave_context(&built_context);

        for chunk in pending.chunks(options.parallel.max(1)) {
            let mut handles = Vec::new();
            for &task_index in chunk {
                let task = &tasks[task_index];
                let model = options.catalog.model_for(task.complexity);
                let handle = spawn_developer(options, task, model, &wave_context)?;
                workflow.phase(&format!(
                    "  {} → modelo {} (complejidad {:?})",
                    task.id, model, task.complexity
                ));
                handles.push((task_index, handle));
            }
            let handle_list: Vec<_> = handles.iter().map(|(_, h)| h.clone()).collect();
            let mut results = wait_all(handle_list, options.agent_timeout, |result| {
                workflow.phase(&format!("  entrega {} → {}", result.name, result.status));
            });

            // Retry failures once, escalating to the next model tier.
            for result in &mut results {
                if result.succeeded() {
                    continue;
                }
                let Some((task_index, _)) = handles.iter().find(|(_, h)| h.name == result.name)
                else {
                    continue;
                };
                let task = &tasks[*task_index];
                let Some(escalated) = escalate(task.complexity) else {
                    continue;
                };
                let retry_model = options.catalog.model_for(escalated);
                workflow.phase(&format!(
                    "  {} falló → reintento con modelo superior {retry_model}",
                    task.id
                ));
                let retry_handle = spawn_developer(options, task, retry_model, &wave_context)?;
                let mut retry_results = wait_all(vec![retry_handle], options.agent_timeout, |_| {});
                if let Some(retry) = retry_results.pop() {
                    workflow.phase(&format!("  reintento {} → {}", task.id, retry.status));
                    *result = retry;
                }
            }

            // Supervisor reviews each delivery as it lands, one by one.
            for result in &results {
                let task = handles
                    .iter()
                    .find(|(_, h)| h.name == result.name)
                    .map(|(index, _)| &tasks[*index]);
                if result.succeeded() {
                    completed += 1;
                } else {
                    failed += 1;
                }
                if let Some(task) = task {
                    supervision_issues +=
                        supervise_delivery(options, &workflow, &supervision_md, task, result)?;
                    if result.succeeded() {
                        state.completed.insert(task.id.clone());
                        state.save(&options.project_dir);
                        built_context.push(context_line(task));
                        let commit_msg = format!(
                            "{} ({}): {}",
                            task.id,
                            task.module,
                            truncate_chars(&task.functional_objective, 60)
                        );
                        let _ = git_commit(&options.project_dir, &commit_msg);
                    }
                }
            }
        }

        // Build gate: the project must still build after every wave.
        run_build_gate(options, &workflow, &supervision_md, wave_index + 1)?;
    }

    if !budget_aborted {
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

                // Dispatch Fixer to repair test failures.
                let fixer_report = run_single(
                    Role::Fixer,
                    &options.catalog.supervisor,
                    options,
                    &format!(
                        "The generated tests are failing:\n\n```\n{}\n```\n\n\
                         Analyze the failures, fix the source code or tests to make them pass, \
                         and report what you changed.",
                        test_error
                    ),
                )?;
                save_doc(&docs, "fixer-qa-report.md", &fixer_report.report)?;
                let _ = git_commit(&options.project_dir, "fix: repair test failures (Fixer)");

                // Retry tests after fixes.
                workflow.phase("Reintentando tests después de fixes del Fixer");
                match run_tests_for_qa(&options.project_dir, &options.build_command) {
                    Ok(test_output) => {
                        workflow.event("qa_tests_fixed", &[("output", test_output)]);
                        let _ =
                            git_commit(&options.project_dir, "qa: all tests passed (after fixes)");
                    }
                    Err(retry_error) => {
                        workflow.event("qa_tests_still_failing", &[("error", retry_error.clone())]);
                        workflow.phase(&format!(
                            "✗ Tests aún fallan después de fixes:\n{}",
                            retry_error
                        ));
                    }
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
        ],
    );
    Ok(RunSummary {
        plan,
        tasks: tasks.len(),
        waves: waves.len(),
        completed,
        failed,
        supervision_issues,
        resumed_tasks,
        budget_aborted,
    })
}

/// Spawns one developer agent with module-scoped writes and inter-wave
/// context appended to its TaskSpec prompt.
fn spawn_developer(
    options: &RunOptions,
    task: &TaskSpec,
    model: &str,
    wave_context: &str,
) -> Result<crate::agents::AgentHandle, String> {
    let prompt = format!("{}{}", task.render_prompt(), wave_context);
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

/// Directories a developer agent may write to, derived from its TaskSpec.
/// Empty (no enforcement) when the spec lists no files.
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

/// Tasks already completed, persisted after every supervised delivery so an
/// interrupted build can resume without repeating work.
#[derive(Default)]
pub struct BuildState {
    pub completed: BTreeSet<String>,
}

impl BuildState {
    fn state_path(project_dir: &Path) -> PathBuf {
        project_dir.join(".multiagent").join("state.json")
    }

    #[must_use]
    pub fn load(project_dir: &Path) -> Self {
        let completed = std::fs::read_to_string(Self::state_path(project_dir))
            .ok()
            .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
            .and_then(|value| {
                value.get("completed").and_then(|ids| {
                    ids.as_array().map(|ids| {
                        ids.iter()
                            .filter_map(|id| id.as_str().map(ToString::to_string))
                            .collect::<BTreeSet<String>>()
                    })
                })
            })
            .unwrap_or_default();
        Self { completed }
    }

    pub fn save(&self, project_dir: &Path) {
        let path = Self::state_path(project_dir);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let payload = serde_json::json!({
            "completed": self.completed.iter().collect::<Vec<_>>(),
        });
        let _ = std::fs::write(path, format!("{payload:#}\n"));
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
        Self {
            ceiling,
            events_path,
        }
    }

    #[must_use]
    pub fn enabled(&self) -> bool {
        self.ceiling.is_some() && self.events_path.is_some()
    }

    /// Returns the spend when it exceeds the ceiling.
    #[must_use]
    pub fn exceeded(&self) -> Option<f64> {
        let ceiling = self.ceiling?;
        let path = self.events_path.as_ref()?;
        let spent = sum_cost_from_events(path);
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
    for attempt in 1..=2 {
        match shell_output(&command, &options.project_dir) {
            Ok(()) => {
                workflow.phase("  build gate: OK");
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

/// Spawns the Supervisor for one delivery; on issues, records them in
/// SUPERVISION.md, triggers the reindex hook, and dispatches the Fixer
/// (superior model) with the exact issue list.
fn supervise_delivery(
    options: &RunOptions,
    workflow: &WorkflowLog,
    supervision_md: &Path,
    task: &TaskSpec,
    delivery: &AgentResult,
) -> Result<usize, String> {
    let supervisor = run_single(
        Role::Supervisor,
        &options.catalog.supervisor,
        options,
        &format!(
            "Task delivered:\n```json\n{}\n```\n\nDeveloper report:\n{}\n\nDelivery \
             status: {}. Inspect the repository files this task touched. Evaluate \
             quality, architecture, conventions, duplication, security and coverage.\n\
             Respond with ```json {{\"approved\": bool, \"summary\": \"...\", \
             \"issues\": [{{\"severity\": \"low|medium|high\", \"file\": \"...\", \
             \"description\": \"...\", \"suggested_fix\": \"...\"}}]}} ```",
            serde_json::to_string(task).unwrap_or_default(),
            delivery.report,
            delivery.status,
        ),
    )?;
    let verdict: SupervisionVerdict = extract_json(&supervisor.report)
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();

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
    if result.status == "timeout" {
        return Err(format!("{} timed out", role.title()));
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

fn init_git_repo(project_dir: &Path, _workflow: &WorkflowLog) -> Result<(), String> {
    let _status = std::process::Command::new("git")
        .arg("init")
        .current_dir(project_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|error| format!("git init failed: {error}"))?;
    Ok(())
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
    let _status = std::process::Command::new("git")
        .arg("commit")
        .arg("-m")
        .arg(message)
        .arg("--quiet")
        .current_dir(project_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|error| format!("git commit failed: {error}"))?;
    Ok(())
}

fn retry_parse_json_with_feedback<T: serde::de::DeserializeOwned>(
    result: &AgentResult,
    who: &str,
    role: Role,
    options: &RunOptions,
    workflow: &WorkflowLog,
) -> Result<T, String> {
    let value = extract_json(&result.report)
        .ok_or_else(|| format!("{who} produced no JSON (status {})", result.status))?;
    if let Ok(parsed) = serde_json::from_value::<T>(value.clone()) {
        return Ok(parsed);
    }
    let parse_error = serde_json::from_value::<T>(value)
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    workflow.phase(&format!(
        "  {who}: JSON validation falló, reintentando con retroalimentación"
    ));
    let retry_report = run_single(
        role,
        &options.catalog.director,
        options,
        &format!(
            "Your previous response failed validation:\n\n{}\n\n\
             Please correct it and produce valid JSON again.",
            parse_error
        ),
    )?;
    let value = extract_json(&retry_report.report)
        .ok_or_else(|| format!("{who} retry produced no JSON"))?;
    serde_json::from_value(value)
        .map_err(|error| format!("{who} retry JSON still invalid: {error}"))
}

fn validate_backlog_against_designs(tasks: &[TaskSpec]) -> Result<(), String> {
    let mut issues = Vec::new();
    for task in tasks {
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
    }
    if !issues.is_empty() {
        return Err(format!("Backlog validation failed:\n{}", issues.join("\n")));
    }
    Ok(())
}

fn run_tests_for_qa(project_dir: &Path, build_cmd: &Option<String>) -> Result<String, String> {
    let test_cmd = if let Some(cmd) = build_cmd {
        if cmd == "off" {
            return Ok("tests skipped (disabled)".to_string());
        }
        if cmd.contains("npm") {
            "npm test".to_string()
        } else if cmd.contains("cargo") {
            "cargo test".to_string()
        } else {
            format!("{} test", cmd)
        }
    } else {
        if project_dir.join("Cargo.toml").exists() {
            "cargo test".to_string()
        } else if project_dir.join("package.json").exists() {
            "npm test".to_string()
        } else {
            return Ok("no test command detected".to_string());
        }
    };
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(&test_cmd)
        .current_dir(project_dir)
        .output()
        .map_err(|error| format!("test execution failed: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if output.status.success() {
        Ok(format!("✓ tests passed\n{}", stdout))
    } else {
        Err(format!(
            "✗ tests failed:\nstdout:\n{}\nstderr:\n{}",
            stdout, stderr
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        };
        assert!(!budget.enabled());
        assert!(budget.exceeded().is_none());
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
}
