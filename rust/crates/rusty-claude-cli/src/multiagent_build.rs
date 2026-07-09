//! `/web` and `/app` REPL commands: launch an autonomous multi-agent build
//! (Director → Subdirector → Architects → parallel developer waves →
//! Supervisor → QA → Docs) without leaving the claw session.

use std::path::PathBuf;
use std::time::Duration;

use claw_multiagent::{run, ModelCatalog, ProjectKind, RunOptions};

const USAGE: &str = "Usage: /web <prompt> [--dry-run] [--approve] [--parallel N] \
[--output <dir>] [--resume] [--max-cost-usd X] [--build-cmd <cmd|off>] [--no-scaffold]\n\
                            /app <prompt> [same options]\n\
                     Tips: --approve pauses after planning for a go/no-go;\n\
                     --dry-run stops after planning entirely.\n\
                     --max-cost-usd is OFF by default (subscription accounts).";

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
    let mut resume = false;
    let mut scaffold = true;
    let mut approve = false;
    let mut parallel = 4_usize;
    let mut output = PathBuf::from("./multiagent-project");
    let mut max_cost_usd: Option<f64> = None;
    let mut build_cmd: Option<String> = None;
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
        resume,
        max_cost_usd,
        build_command: build_cmd,
        scaffold,
        approve,
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
