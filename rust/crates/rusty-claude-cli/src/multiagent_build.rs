//! `/web` and `/app` REPL commands: launch an autonomous multi-agent build
//! (Director → Subdirector → Architects → parallel developer waves →
//! Supervisor → QA → Docs) without leaving the claw session.

use std::path::PathBuf;
use std::time::Duration;

use claw_multiagent::{run, ModelCatalog, ProjectKind, RunOptions};

const USAGE: &str = "Usage: /web <prompt> [--dry-run] [--parallel N] [--output <dir>]\n\
                            /app <prompt> [--dry-run] [--parallel N] [--output <dir>]\n\
                     Tip: start with --dry-run to review the plan before building.";

/// Parses the slash-command arguments and runs the build. The working
/// directory and agent store are restored afterwards so the REPL session
/// continues where it was.
pub(crate) fn run_multiagent_build(
    kind: ProjectKind,
    raw: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let raw = raw.unwrap_or("").trim();
    if raw.is_empty() {
        println!("{USAGE}");
        return Ok(());
    }

    let mut prompt_words: Vec<&str> = Vec::new();
    let mut dry_run = false;
    let mut parallel = 4_usize;
    let mut output = PathBuf::from("./multiagent-project");
    let tokens: Vec<&str> = raw.split_whitespace().collect();
    let mut index = 0;
    while index < tokens.len() {
        match tokens[index] {
            "--dry-run" => dry_run = true,
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
            word => prompt_words.push(word),
        }
        index += 1;
    }
    let prompt = prompt_words.join(" ");
    if prompt.is_empty() {
        println!("{USAGE}");
        return Ok(());
    }

    let original_cwd = std::env::current_dir()?;
    let original_store = std::env::var_os("CLAWD_AGENT_STORE");
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

    let result = run(&RunOptions {
        kind,
        prompt,
        project_dir,
        catalog,
        parallel,
        dry_run,
        agent_timeout: Duration::from_secs(1800),
    });

    // Restore REPL environment regardless of the outcome.
    std::env::set_current_dir(&original_cwd)?;
    match original_store {
        Some(store) => std::env::set_var("CLAWD_AGENT_STORE", store),
        None => std::env::remove_var("CLAWD_AGENT_STORE"),
    }

    match result {
        Ok(summary) => {
            println!("[multiagent] visión: {}", summary.plan.vision);
            println!(
                "[multiagent] stack: {} · tareas: {} · olas: {}",
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
            }
            Ok(())
        }
        Err(error) => {
            println!("[multiagent] error: {error}");
            Ok(())
        }
    }
}
