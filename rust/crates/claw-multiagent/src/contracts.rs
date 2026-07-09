//! Inter-agent contracts: the typed messages every agent exchanges.
//!
//! The spec requires that no agent receives ambiguous instructions: a
//! [`TaskSpec`] carries every mandatory field (objective, files, interfaces,
//! validations, risks, Definition of Done, ...). All fields are lenient to
//! deserialize (defaults) because they are produced by LLM agents, but the
//! orchestrator validates the invariants that matter (disjoint files per
//! wave, non-empty objectives) before dispatching work.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// Which aplicativo is being built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectKind {
    Web,
    App,
}

impl ProjectKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::App => "app",
        }
    }
}

/// Task complexity drives the model tier that executes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Complexity {
    Simple,
    #[default]
    Medium,
    Complex,
}

/// Product plan produced by the Director General.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Plan {
    pub vision: String,
    pub scope: Vec<String>,
    pub stack: StackDecision,
    pub epics: Vec<Epic>,
    pub milestones: Vec<String>,
    pub risks: Vec<String>,
    pub open_questions: Vec<String>,
}

/// Stack decision: simple static project vs. full stack, with justification.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StackDecision {
    /// "simple" (HTML/CSS/JS/Astro/Vite, no backend) or "fullstack".
    pub kind: String,
    pub frontend: Vec<String>,
    pub backend: Vec<String>,
    pub database: Vec<String>,
    pub orm: Vec<String>,
    pub auth: Vec<String>,
    pub platforms: Vec<String>,
    pub framework: Vec<String>,
    pub deploy: Vec<String>,
    pub justification: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Epic {
    pub name: String,
    pub stories: Vec<UserStory>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UserStory {
    pub as_a: String,
    pub i_want: String,
    pub so_that: String,
    pub acceptance_criteria: Vec<String>,
}

/// Fully specified unit of work produced by the Subdirector Técnico.
/// Rich enough that a developer agent implements it without guessing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskSpec {
    pub id: String,
    pub module: String,
    pub functional_objective: String,
    pub technical_objective: String,
    pub justification: String,
    pub files_to_create: Vec<String>,
    pub files_to_modify: Vec<String>,
    pub interfaces: Vec<String>,
    pub functions: Vec<String>,
    pub data_models: Vec<String>,
    pub endpoints: Vec<String>,
    pub dependencies: Vec<String>,
    pub libraries: Vec<String>,
    pub env_vars: Vec<String>,
    pub edge_cases: Vec<String>,
    pub validations: Vec<String>,
    pub error_handling: Vec<String>,
    pub logging: Vec<String>,
    pub tests_required: Vec<String>,
    pub constraints: Vec<String>,
    pub risks: Vec<String>,
    pub definition_of_done: Vec<String>,
    pub priority: u32,
    pub complexity: Complexity,
    pub estimated_minutes: u32,
    /// Execution wave: tasks in the same wave run in parallel.
    pub wave: u32,
    /// Tasks (ids) that must complete before this one.
    pub depends_on: Vec<String>,
}

impl TaskSpec {
    /// All files this task touches (used for the disjoint-files invariant).
    #[must_use]
    pub fn touched_files(&self) -> BTreeSet<&str> {
        self.files_to_create
            .iter()
            .chain(self.files_to_modify.iter())
            .map(String::as_str)
            .collect()
    }

    /// Renders the spec as the exact prompt a developer agent receives.
    #[must_use]
    pub fn render_prompt(&self) -> String {
        fn section(label: &str, items: &[String]) -> String {
            if items.is_empty() {
                return String::new();
            }
            format!("\n## {label}\n- {}\n", items.join("\n- "))
        }
        format!(
            "# Task {id} — module `{module}`\n\n\
             ## Functional objective\n{functional}\n\n\
             ## Technical objective\n{technical}\n\n\
             ## Justification\n{justification}\n\
             {files_create}{files_modify}{interfaces}{functions}{models}{endpoints}\
             {dependencies}{libraries}{env}{edge}{validations}{errors}{logging}\
             {tests}{constraints}{risks}{dod}\n\
             ## Rules\n\
             - Work ONLY inside module `{module}` and the files listed above.\n\
             - Never touch files owned by other modules.\n\
             - Apply SOLID, Clean Code, strict typing and secure defaults.\n\
             - Do not invent unspecified details; if forced to decide, record the \
               decision and its justification in your final report.\n",
            id = self.id,
            module = self.module,
            functional = self.functional_objective,
            technical = self.technical_objective,
            justification = self.justification,
            files_create = section("Files to create", &self.files_to_create),
            files_modify = section("Files to modify", &self.files_to_modify),
            interfaces = section("Interfaces", &self.interfaces),
            functions = section("Functions / methods (exact signatures)", &self.functions),
            models = section("Data models", &self.data_models),
            endpoints = section("Endpoints", &self.endpoints),
            dependencies = section("Depends on tasks", &self.depends_on),
            libraries = section("Libraries (recommended versions)", &self.libraries),
            env = section("Environment variables", &self.env_vars),
            edge = section("Edge cases", &self.edge_cases),
            validations = section("Validations", &self.validations),
            errors = section("Error handling", &self.error_handling),
            logging = section("Logging & telemetry", &self.logging),
            tests = section("Required tests", &self.tests_required),
            constraints = section("Constraints", &self.constraints),
            risks = section("Risks", &self.risks),
            dod = section("Definition of Done", &self.definition_of_done),
        )
    }
}

/// Groups tasks into executable waves enforcing the two invariants the spec
/// demands: dependencies run in earlier waves, and no two tasks in the same
/// wave touch the same file. Conflicting tasks are pushed to later waves.
#[must_use]
pub fn schedule_waves(tasks: &[TaskSpec]) -> Vec<Vec<usize>> {
    let mut assigned_wave: Vec<u32> = tasks.iter().map(|task| task.wave).collect();

    // Dependencies force later waves.
    let mut moved = true;
    while moved {
        moved = false;
        for (index, task) in tasks.iter().enumerate() {
            for dep_id in &task.depends_on {
                if let Some(dep_index) = tasks.iter().position(|t| &t.id == dep_id) {
                    if assigned_wave[index] <= assigned_wave[dep_index] {
                        assigned_wave[index] = assigned_wave[dep_index] + 1;
                        moved = true;
                    }
                }
            }
        }
    }

    // Same-wave file conflicts: defer the lower-priority task.
    let mut conflict = true;
    while conflict {
        conflict = false;
        for a in 0..tasks.len() {
            for b in (a + 1)..tasks.len() {
                if assigned_wave[a] != assigned_wave[b] {
                    continue;
                }
                let files_a = tasks[a].touched_files();
                let files_b = tasks[b].touched_files();
                if files_a.intersection(&files_b).next().is_some() {
                    let defer = if tasks[a].priority <= tasks[b].priority {
                        b
                    } else {
                        a
                    };
                    assigned_wave[defer] += 1;
                    conflict = true;
                }
            }
        }
    }

    let mut waves: Vec<(u32, Vec<usize>)> = Vec::new();
    for (index, wave) in assigned_wave.iter().enumerate() {
        match waves.iter_mut().find(|(w, _)| w == wave) {
            Some((_, members)) => members.push(index),
            None => waves.push((*wave, vec![index])),
        }
    }
    waves.sort_by_key(|(wave, _)| *wave);
    waves.into_iter().map(|(_, members)| members).collect()
}

/// Supervisor verdict for one completed task.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SupervisionVerdict {
    pub approved: bool,
    pub issues: Vec<SupervisionIssue>,
    pub summary: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SupervisionIssue {
    pub severity: String,
    pub file: String,
    pub description: String,
    pub suggested_fix: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, wave: u32, priority: u32, files: &[&str], deps: &[&str]) -> TaskSpec {
        TaskSpec {
            id: id.to_string(),
            wave,
            priority,
            files_to_create: files.iter().map(ToString::to_string).collect(),
            depends_on: deps.iter().map(ToString::to_string).collect(),
            ..TaskSpec::default()
        }
    }

    #[test]
    fn same_wave_file_conflicts_are_deferred() {
        // Priority 1 is the highest (matches the Subdirector schema).
        let tasks = vec![
            task("a", 0, 1, &["src/shared.ts"], &[]),
            task("b", 0, 5, &["src/shared.ts"], &[]),
            task("c", 0, 5, &["src/other.ts"], &[]),
        ];
        let waves = schedule_waves(&tasks);
        assert_eq!(waves.len(), 2);
        // Top-priority "a" keeps wave 0; "b" is deferred; "c" untouched.
        assert!(waves[0].contains(&0) && waves[0].contains(&2));
        assert_eq!(waves[1], vec![1]);
    }

    #[test]
    fn dependencies_force_later_waves() {
        let tasks = vec![
            task("api", 0, 1, &["src/api.ts"], &[]),
            task("ui", 0, 1, &["src/ui.tsx"], &["api"]),
            task("e2e", 0, 1, &["tests/e2e.ts"], &["ui"]),
        ];
        let waves = schedule_waves(&tasks);
        assert_eq!(waves, vec![vec![0], vec![1], vec![2]]);
    }

    #[test]
    fn task_spec_parses_from_partial_json() {
        let spec: TaskSpec = serde_json::from_str(
            r#"{"id":"t1","module":"auth","functional_objective":"login","complexity":"complex"}"#,
        )
        .expect("lenient parse");
        assert_eq!(spec.id, "t1");
        assert_eq!(spec.complexity, Complexity::Complex);
        assert!(spec.files_to_modify.is_empty());
    }

    #[test]
    fn rendered_prompt_contains_isolation_rules_and_sections() {
        let mut spec = task("t1", 0, 1, &["src/cart.ts"], &[]);
        spec.module = "cart".to_string();
        spec.functional_objective = "shopping cart".to_string();
        spec.definition_of_done = vec!["tests pass".to_string()];
        let prompt = spec.render_prompt();
        assert!(prompt.contains("module `cart`"));
        assert!(prompt.contains("src/cart.ts"));
        assert!(prompt.contains("Never touch files owned by other modules"));
        assert!(prompt.contains("Definition of Done"));
    }
}
