//! Agent roles: the specialized hierarchy (Director → Subdirector →
//! Architects → Developers, with Supervisor/QA/Docs as transversal roles),
//! their prompts, and the per-model instruction files (CLAUDE.md for
//! Anthropic models, AGENTS.md for OpenAI, etc.) the spec requires.

use std::fmt::Write as _;
use std::path::Path;

use api::ProviderKind;

use crate::catalog::{provider_for_model, ModelCatalog};
use crate::contracts::ProjectKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Director,
    Subdirector,
    SoftwareArchitect,
    FrontendArchitect,
    BackendArchitect,
    DevOpsArchitect,
    UxUiDesigner,
    Developer,
    Supervisor,
    Fixer,
    Qa,
    Docs,
}

impl Role {
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Director => "Director General (Chief Orchestrator)",
            Self::Subdirector => "Subdirector Técnico",
            Self::SoftwareArchitect => "Arquitecto de Software",
            Self::FrontendArchitect => "Arquitecto Frontend",
            Self::BackendArchitect => "Arquitecto Backend",
            Self::DevOpsArchitect => "Arquitecto DevOps",
            Self::UxUiDesigner => "Diseñador UX/UI",
            Self::Developer => "Agente Desarrollador",
            Self::Supervisor => "Supervisor Técnico",
            Self::Fixer => "Técnico de Correcciones",
            Self::Qa => "Agente QA",
            Self::Docs => "Agente de Documentación",
        }
    }

    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Director => "director",
            Self::Subdirector => "subdirector",
            Self::SoftwareArchitect => "software-architect",
            Self::FrontendArchitect => "frontend-architect",
            Self::BackendArchitect => "backend-architect",
            Self::DevOpsArchitect => "devops-architect",
            Self::UxUiDesigner => "uxui-designer",
            Self::Developer => "developer",
            Self::Supervisor => "supervisor",
            Self::Fixer => "fixer",
            Self::Qa => "qa",
            Self::Docs => "docs",
        }
    }

    /// Subagent type: read-only planning roles use `Plan`; roles that write
    /// code use `general-purpose`; the supervisor verifies without writing.
    #[must_use]
    pub const fn subagent_type(self) -> &'static str {
        match self {
            Self::Director
            | Self::Subdirector
            | Self::SoftwareArchitect
            | Self::FrontendArchitect
            | Self::BackendArchitect
            | Self::DevOpsArchitect
            | Self::UxUiDesigner => "Plan",
            Self::Supervisor => "Verification",
            Self::Developer | Self::Fixer | Self::Qa | Self::Docs => "general-purpose",
        }
    }

    #[must_use]
    pub const fn responsibility(self) -> &'static str {
        match self {
            Self::Director => {
                "Interpreta el prompt, completa requisitos, define visión, alcance, \
                 roadmap, épicas, historias, criterios de aceptación, riesgos y stack."
            }
            Self::Subdirector => {
                "Convierte el plan en tareas ejecutables: módulos, bounded contexts, \
                 dependencias, complejidad, paralelización y modelo de IA por tarea."
            }
            Self::SoftwareArchitect => {
                "Diseña capas, interfaces, casos de uso, entidades, eventos e \
                 inyección de dependencias, justificando cada decisión."
            }
            Self::FrontendArchitect => {
                "Define UX/UI, design system, componentes, props, estados, rutas, \
                 accesibilidad, SEO y performance."
            }
            Self::BackendArchitect => {
                "Diseña API, contratos, endpoints, DTO, validaciones, seguridad, \
                 colas, cache y observabilidad (OpenAPI incluido)."
            }
            Self::DevOpsArchitect => "Diseña Docker, CI/CD, entornos, secrets y monitoring.",
            Self::UxUiDesigner => {
                "Define branding, colores, tipografías, iconografía y \
                 microinteracciones con justificación de usabilidad."
            }
            Self::Developer => {
                "Implementa exactamente su TaskSpec dentro de su módulo, sin tocar \
                 archivos de otros módulos."
            }
            Self::Supervisor => {
                "Revisa cada entrega al terminar: calidad, arquitectura, duplicidad, \
                 seguridad. Registra bugs/fixes en SUPERVISION.md y deriva al Técnico."
            }
            Self::Fixer => "Aplica las correcciones reportadas por el Supervisor.",
            Self::Qa => {
                "Genera y ejecuta unit/integration/E2E/smoke tests y valida \
                 cobertura, errores y accesibilidad."
            }
            Self::Docs => "Mantiene README, arquitectura, ADRs, changelog y guías al día.",
        }
    }
}

const QUALITY_RULES: &str = "Quality rules (mandatory): SOLID, DRY, KISS, YAGNI, Clean \
Architecture, high cohesion / low coupling, strict typing, security by default, tests, \
and documented decisions. Architecture quality beats implementation speed.";

/// System prompt for a role, in the context of a WEB or APP build.
#[must_use]
pub fn system_prompt(role: Role, kind: ProjectKind) -> String {
    let aplicativo = match kind {
        ProjectKind::Web => {
            "You are building a WEB project. Decide (or respect the Director's decision) \
             between a simple static site (HTML/CSS/JS, Astro, Vite — no API, no DB, no \
             backend) and a full-stack build (React/Next/Vue/Nuxt/Svelte + Node/NestJS/\
             Express/Fastify/Rust + PostgreSQL/MySQL/SQLite/MongoDB/Redis + Prisma/Drizzle \
             + Clerk/AuthJS/JWT/OAuth, deploy on Coolify). Use full stack ONLY when needed."
        }
        ProjectKind::App => {
            "You are building an APP for desktop and/or mobile (Windows, Linux, macOS, \
             Android, iOS, tablets). Choose (or respect the Director's decision) among \
             Electron, Tauri, Flutter, React Native, MAUI, Kotlin, Swift, and decide if \
             the app is offline, online, hybrid or full stack."
        }
    };
    format!(
        "You are the {title} in an autonomous multi-agent software platform.\n\
         Responsibility: {responsibility}\n\n{aplicativo}\n\n{QUALITY_RULES}",
        title = role.title(),
        responsibility = role.responsibility(),
    )
}

/// Filename of the instruction file each model family reads by convention.
#[must_use]
pub fn instruction_filename_for_model(model: &str) -> &'static str {
    match provider_for_model(model) {
        ProviderKind::Anthropic => "CLAUDE.md",
        ProviderKind::Xai => "GROK.md",
        ProviderKind::OpenAi => "AGENTS.md",
    }
}

/// Writes one instruction file per model family used in this run, listing
/// each role, the model that executes it, and its responsibility — the spec
/// requires the Director/Subdirector to leave these files in the project.
///
/// When `preserve_existing` is set (Improve mode over an existing repo), a
/// file that already exists is left untouched — the user's own `CLAUDE.md`
/// (their project instructions) must never be overwritten.
pub fn write_role_instruction_files(
    project_dir: &Path,
    catalog: &ModelCatalog,
    kind: ProjectKind,
    preserve_existing: bool,
) -> Result<Vec<String>, String> {
    let assignments: Vec<(Role, &str)> = vec![
        (Role::Director, catalog.director.as_str()),
        (Role::Subdirector, catalog.director.as_str()),
        (Role::SoftwareArchitect, catalog.director.as_str()),
        (Role::FrontendArchitect, catalog.director.as_str()),
        (Role::BackendArchitect, catalog.director.as_str()),
        (Role::DevOpsArchitect, catalog.director.as_str()),
        (Role::UxUiDesigner, catalog.director.as_str()),
        (Role::Developer, catalog.medium.as_str()),
        (Role::Supervisor, catalog.supervisor.as_str()),
        (Role::Fixer, catalog.supervisor.as_str()),
        (Role::Qa, catalog.medium.as_str()),
        (Role::Docs, catalog.simple.as_str()),
    ];

    let mut written = Vec::new();
    for filename in ["CLAUDE.md", "AGENTS.md", "GROK.md"] {
        let roles_for_file: Vec<&(Role, &str)> = assignments
            .iter()
            .filter(|(_, model)| instruction_filename_for_model(model) == filename)
            .collect();
        if roles_for_file.is_empty() {
            continue;
        }
        let mut body = format!(
            "# Multi-agent build — model instructions ({})\n\n\
             This project ({}) is built by an autonomous agent hierarchy. If you are a \
             model reading this file, locate your role below and stay strictly within \
             its responsibility.\n\n| Role | Model | Responsibility |\n|---|---|---|\n",
            filename,
            kind.as_str(),
        );
        for (role, model) in roles_for_file {
            let _ = writeln!(
                body,
                "| {} | `{}` | {} |",
                role.title(),
                model,
                role.responsibility()
            );
        }
        body.push_str(
            "\nShared rules: never modify files owned by another module; follow the \
             TaskSpec exactly; justify any decision the spec does not cover.\n",
        );
        let path = project_dir.join(filename);
        if preserve_existing && path.exists() {
            // The user's own instruction file stays; our role table is
            // supplementary (each agent also gets its role via its system
            // prompt).
            continue;
        }
        std::fs::write(&path, body).map_err(|error| error.to_string())?;
        written.push(filename.to_string());
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instruction_filenames_follow_provider_conventions() {
        assert_eq!(
            instruction_filename_for_model("claude-opus-4-6"),
            "CLAUDE.md"
        );
        assert_eq!(instruction_filename_for_model("gpt-4o"), "AGENTS.md");
        assert_eq!(instruction_filename_for_model("qwen-plus"), "AGENTS.md");
        assert_eq!(instruction_filename_for_model("grok-3"), "GROK.md");
    }

    #[test]
    fn planning_roles_are_read_only_and_developers_write() {
        assert_eq!(Role::Director.subagent_type(), "Plan");
        assert_eq!(Role::Supervisor.subagent_type(), "Verification");
        assert_eq!(Role::Developer.subagent_type(), "general-purpose");
    }

    #[test]
    fn instruction_files_written_per_model_family() {
        let dir = std::env::temp_dir().join(format!(
            "multiagent-roles-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        let catalog = ModelCatalog {
            // Docs role → AGENTS.md
            simple: "qwen-turbo".to_string(),
            ..ModelCatalog::default()
        };

        let files = write_role_instruction_files(&dir, &catalog, ProjectKind::Web, false)
            .expect("instruction files");
        assert!(files.contains(&"CLAUDE.md".to_string()));
        assert!(files.contains(&"AGENTS.md".to_string()));

        let claude = std::fs::read_to_string(dir.join("CLAUDE.md")).expect("claude md");
        assert!(claude.contains("Director General"));
        let agents = std::fs::read_to_string(dir.join("AGENTS.md")).expect("agents md");
        assert!(agents.contains("Documentación"));
        assert!(agents.contains("qwen-turbo"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn preserve_existing_keeps_the_users_instruction_file() {
        let dir = std::env::temp_dir().join(format!(
            "roles-preserve-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("CLAUDE.md"), "USER PROJECT INSTRUCTIONS").expect("seed");

        let catalog = ModelCatalog::default();
        let written = write_role_instruction_files(&dir, &catalog, ProjectKind::Web, true)
            .expect("instruction files");

        // The user's CLAUDE.md is untouched and not reported as written.
        assert_eq!(
            std::fs::read_to_string(dir.join("CLAUDE.md")).expect("claude"),
            "USER PROJECT INSTRUCTIONS"
        );
        assert!(!written.contains(&"CLAUDE.md".to_string()));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn system_prompt_reflects_aplicativo_and_quality() {
        let web = system_prompt(Role::Director, ProjectKind::Web);
        assert!(web.contains("WEB project"));
        assert!(web.contains("SOLID"));
        let app = system_prompt(Role::Developer, ProjectKind::App);
        assert!(app.contains("Electron, Tauri, Flutter"));
    }
}
