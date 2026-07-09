//! The pipeline: Director → Subdirector → Architects (parallel) →
//! Developer waves (parallel, disjoint files) → Supervisor per delivery →
//! Fixer → QA → Docs. Deterministic Rust control flow; agents do the work.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::agents::{extract_json, spawn_agent, wait_all, AgentResult};
use crate::catalog::ModelCatalog;
use crate::contracts::{schedule_waves, Plan, ProjectKind, SupervisionVerdict, TaskSpec};
use crate::roles::{system_prompt, write_role_instruction_files, Role};

pub struct RunOptions {
    pub kind: ProjectKind,
    pub prompt: String,
    pub project_dir: PathBuf,
    pub catalog: ModelCatalog,
    /// Max developer agents running at once inside a wave.
    pub parallel: usize,
    /// Stop after planning (Director + Subdirector), printing the plan.
    pub dry_run: bool,
    /// Per-agent timeout.
    pub agent_timeout: Duration,
}

pub struct RunSummary {
    pub plan: Plan,
    pub tasks: usize,
    pub waves: usize,
    pub completed: usize,
    pub failed: usize,
    pub supervision_issues: usize,
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
Rules: modules must be independent; two tasks in the same wave must NEVER touch the
same file; assign complexity honestly (it selects the AI model per task)."#;

pub fn run(options: &RunOptions) -> Result<RunSummary, String> {
    options.catalog.validate_credentials()?;
    let docs = options.project_dir.join("docs");
    std::fs::create_dir_all(&docs).map_err(|error| error.to_string())?;
    // Keep every agent artifact inside the generated project.
    std::env::set_var("CLAWD_AGENT_STORE", options.project_dir.join(".multiagent"));
    std::env::set_current_dir(&options.project_dir).map_err(|error| error.to_string())?;

    // ---- Phase 1: Director General ----
    log_phase("Director General: interpretando el prompt y creando el plan");
    let director_report = run_single(
        Role::Director,
        &options.catalog.director,
        options,
        &format!(
            "User prompt:\n{}\n\nProduce the complete product plan.\n{}",
            options.prompt, DIRECTOR_JSON_SCHEMA
        ),
    )?;
    let plan: Plan = parse_agent_json(&director_report, "Director")?;
    save_doc(
        &docs,
        "plan.json",
        &serde_json::to_string_pretty(&plan).unwrap_or_default(),
    )?;
    save_doc(&docs, "director-report.md", &director_report.report)?;

    // ---- Phase 2: Subdirector Técnico ----
    log_phase("Subdirector Técnico: creando el backlog ejecutable");
    let plan_json = serde_json::to_string(&plan).unwrap_or_default();
    let subdirector_report = run_single(
        Role::Subdirector,
        &options.catalog.director,
        options,
        &format!(
            "Approved plan:\n```json\n{plan_json}\n```\n\nBreak it into fully specified,\
             parallelizable TaskSpecs.\n{SUBDIRECTOR_JSON_SCHEMA}"
        ),
    )?;
    let tasks_value =
        extract_json(&subdirector_report.report).ok_or("Subdirector produced no JSON backlog")?;
    let tasks: Vec<TaskSpec> =
        serde_json::from_value(tasks_value.get("tasks").cloned().unwrap_or(tasks_value))
            .map_err(|error| format!("Subdirector backlog did not validate: {error}"))?;
    if tasks.is_empty() {
        return Err("Subdirector produced an empty backlog".to_string());
    }
    save_doc(
        &docs,
        "backlog.json",
        &serde_json::to_string_pretty(&tasks).unwrap_or_default(),
    )?;

    // Instruction files per model family (CLAUDE.md / AGENTS.md / GROK.md).
    let instruction_files =
        write_role_instruction_files(&options.project_dir, &options.catalog, options.kind)?;
    log_phase(&format!(
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
        });
    }

    // ---- Phase 3: Architects (parallel) ----
    log_phase("Arquitectos: diseñando la solución en paralelo");
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
                "Plan:\n```json\n{plan_json}\n```\n\nDeliver your complete design \
                 document in Markdown, justifying every decision."
            ),
        )?;
        architect_handles.push(handle);
    }
    for result in wait_all(architect_handles, options.agent_timeout, |result| {
        log_phase(&format!("  arquitecto {} → {}", result.name, result.status));
    }) {
        save_doc(&docs, &format!("{}.md", result.name), &result.report)?;
    }

    // ---- Phase 4: Developer waves + Supervisor per delivery ----
    let supervision_md = docs.join("SUPERVISION.md");
    let mut completed = 0_usize;
    let mut failed = 0_usize;
    let mut supervision_issues = 0_usize;

    for (wave_index, wave) in waves.iter().enumerate() {
        log_phase(&format!(
            "Ola {}/{}: {} tareas en paralelo",
            wave_index + 1,
            waves.len(),
            wave.len()
        ));
        for chunk in wave.chunks(options.parallel.max(1)) {
            let mut handles = Vec::new();
            for &task_index in chunk {
                let task = &tasks[task_index];
                let model = options.catalog.model_for(task.complexity);
                let handle = spawn_agent(
                    &format!("dev-{}", task.id),
                    &task.functional_objective,
                    model,
                    Role::Developer.subagent_type(),
                    &system_prompt(Role::Developer, options.kind),
                    &task.render_prompt(),
                )?;
                log_phase(&format!(
                    "  {} → modelo {} (complejidad {:?})",
                    task.id, model, task.complexity
                ));
                handles.push((task_index, handle));
            }
            let handle_list: Vec<_> = handles.iter().map(|(_, h)| h.clone()).collect();
            let results = wait_all(handle_list, options.agent_timeout, |result| {
                log_phase(&format!("  entrega {} → {}", result.name, result.status));
            });
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
                        supervise_delivery(options, &supervision_md, task, result)?;
                }
            }
        }
    }

    // ---- Phase 5: QA ----
    log_phase("Agente QA: generando y ejecutando pruebas");
    let qa = run_single(
        Role::Qa,
        &options.catalog.medium,
        options,
        "Generate and run the unit/integration/E2E/smoke tests this project needs. \
         Report coverage, failures found and fixes applied.",
    )?;
    save_doc(&docs, "qa-report.md", &qa.report)?;

    // ---- Phase 6: Documentation ----
    log_phase("Agente de Documentación: README, ADRs y changelog");
    let docs_agent = run_single(
        Role::Docs,
        &options.catalog.simple,
        options,
        "Create/update README.md, docs/ARCHITECTURE.md, docs/adr/ (one ADR per key \
         decision from docs/*.md), CHANGELOG.md and deployment/development guides, \
         reflecting the project as actually built.",
    )?;
    save_doc(&docs, "docs-report.md", &docs_agent.report)?;

    Ok(RunSummary {
        plan,
        tasks: tasks.len(),
        waves: waves.len(),
        completed,
        failed,
        supervision_issues,
    })
}

/// Spawns the Supervisor for one delivery; on issues, records them in
/// SUPERVISION.md, triggers the reindex hook, and dispatches the Fixer
/// (superior model) with the exact issue list.
fn supervise_delivery(
    options: &RunOptions,
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
        log_phase(&format!(
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
    )?;
    let mut results = wait_all(vec![handle], options.agent_timeout, |_| {});
    let result = results.pop().ok_or("agent produced no result")?;
    if result.status == "timeout" {
        return Err(format!("{} timed out", role.title()));
    }
    Ok(result)
}

fn parse_agent_json<T: serde::de::DeserializeOwned>(
    result: &AgentResult,
    who: &str,
) -> Result<T, String> {
    let value = extract_json(&result.report)
        .ok_or_else(|| format!("{who} produced no JSON (status {})", result.status))?;
    serde_json::from_value(value).map_err(|error| format!("{who} JSON did not validate: {error}"))
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

fn log_phase(message: &str) {
    println!("[multiagent] {message}");
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
}
