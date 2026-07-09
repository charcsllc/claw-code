//! Autonomous multi-agent platform for building WEB and APP projects.
//!
//! From a single user prompt, a hierarchy of specialized agents plans,
//! architects, implements (in parallel waves over disjoint modules),
//! supervises, tests and documents a complete project:
//!
//! ```text
//! Director General ─► Subdirector Técnico ─► Arquitectos (paralelo)
//!        │                                        │
//!        └── plan.json / backlog.json / CLAUDE.md-AGENTS.md por modelo
//!                                                 ▼
//!            Olas de Agentes Desarrolladores (paralelo, archivos disjuntos)
//!                    │ entrega a entrega
//!                    ▼
//!            Supervisor Técnico ─► SUPERVISION.md ─► Técnico (fixes)
//!                    ▼
//!               Agente QA ─► Agente de Documentación
//! ```
//!
//! Models are selected per task complexity from any provider with
//! credentials configured (Anthropic, OpenAI, xAI, DashScope, Ollama), and
//! every agent reports live to `claw-dashboard` when
//! `CLAW_DASHBOARD_EVENTS` is set.

pub mod agents;
pub mod catalog;
pub mod contracts;
pub mod orchestrator;
pub mod roles;

pub use catalog::ModelCatalog;
pub use contracts::{ProjectKind, TaskSpec};
pub use orchestrator::{run, RunOptions, RunSummary};
