//! Golden-file snapshots of every prompt the pipeline sends: role system
//! prompts, architect/designer planning briefs, the design-system build
//! prompt, the visual-QA rubric, and the Director/Subdirector JSON output
//! contracts.
//!
//! WHY: prompts are the product here. A one-word edit can change what nine
//! agents build, so prompt changes must be REVIEWED like code. This test
//! makes every change visible: the golden file's diff in the PR *is* the
//! prompt review.
//!
//! To update after an intentional prompt change:
//!
//! ```bash
//! CLAW_UPDATE_GOLDENS=1 cargo test -p claw-multiagent --test prompt_goldens
//! ```
//!
//! then commit the regenerated `tests/goldens/*.txt` together with the code
//! change and review their diff in the PR.

use std::fmt::Write as _;
use std::path::PathBuf;

use claw_multiagent::contracts::ProjectKind;
use claw_multiagent::design::{
    design_system_prompt, designer_planning_brief, visual_qa_prompt, DesignArchetype,
};
use claw_multiagent::orchestrator::{DIRECTOR_JSON_SCHEMA, SUBDIRECTOR_JSON_SCHEMA};
use claw_multiagent::roles::{architect_planning_brief, system_prompt, Role};

/// All twelve roles of the hierarchy (kept in declaration order).
const ALL_ROLES: [Role; 12] = [
    Role::Director,
    Role::Subdirector,
    Role::SoftwareArchitect,
    Role::FrontendArchitect,
    Role::BackendArchitect,
    Role::DevOpsArchitect,
    Role::UxUiDesigner,
    Role::Developer,
    Role::Supervisor,
    Role::Fixer,
    Role::Qa,
    Role::Docs,
];

/// The architects with a structured six-section brief.
const BRIEFED_ARCHITECTS: [Role; 4] = [
    Role::SoftwareArchitect,
    Role::FrontendArchitect,
    Role::BackendArchitect,
    Role::DevOpsArchitect,
];

fn goldens_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("goldens")
}

/// Every snapshotted prompt: `(golden file name, current content)`.
fn snapshots() -> Vec<(String, String)> {
    let mut snaps = Vec::new();
    for role in ALL_ROLES {
        snaps.push((
            format!("system_prompt_web_{}.txt", role.slug()),
            system_prompt(role, ProjectKind::Web),
        ));
    }
    for role in BRIEFED_ARCHITECTS {
        snaps.push((
            format!("architect_brief_{}.txt", role.slug()),
            architect_planning_brief(role).to_string(),
        ));
    }
    snaps.push((
        "designer_brief_saas.txt".to_string(),
        designer_planning_brief(DesignArchetype::Saas),
    ));
    snaps.push((
        "design_system_prompt_saas.txt".to_string(),
        design_system_prompt(DesignArchetype::Saas, "src/styles/design-tokens.css"),
    ));
    snaps.push(("visual_qa_prompt.txt".to_string(), visual_qa_prompt()));
    snaps.push((
        "director_json_schema.txt".to_string(),
        DIRECTOR_JSON_SCHEMA.to_string(),
    ));
    snaps.push((
        "subdirector_json_schema.txt".to_string(),
        SUBDIRECTOR_JSON_SCHEMA.to_string(),
    ));
    snaps
}

/// Readable line diff (golden vs. actual), capped so a full rewrite does
/// not flood the failure output.
fn render_diff(golden: &str, actual: &str) -> String {
    let golden_lines: Vec<&str> = golden.lines().collect();
    let actual_lines: Vec<&str> = actual.lines().collect();
    let mut out = String::new();
    let mut shown = 0_usize;
    for index in 0..golden_lines.len().max(actual_lines.len()) {
        let expected = golden_lines.get(index).copied();
        let got = actual_lines.get(index).copied();
        if expected == got {
            continue;
        }
        if shown >= 12 {
            let _ = writeln!(out, "  … (more differences truncated)");
            break;
        }
        shown += 1;
        let _ = writeln!(out, "  line {}:", index + 1);
        if let Some(expected) = expected {
            let _ = writeln!(out, "    - golden: {expected}");
        } else {
            let _ = writeln!(out, "    - golden: <no line>");
        }
        if let Some(got) = got {
            let _ = writeln!(out, "    + actual: {got}");
        } else {
            let _ = writeln!(out, "    + actual: <no line>");
        }
    }
    out
}

#[test]
fn prompts_match_their_golden_files() {
    let update = std::env::var("CLAW_UPDATE_GOLDENS").is_ok_and(|value| value.trim() == "1");
    let dir = goldens_dir();
    if update {
        std::fs::create_dir_all(&dir).expect("goldens dir");
    }

    let mut failures = String::new();
    for (name, actual) in snapshots() {
        let path = dir.join(&name);
        if update {
            std::fs::write(&path, &actual)
                .unwrap_or_else(|error| panic!("cannot write {}: {error}", path.display()));
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(golden) if golden == actual => {}
            Ok(golden) => {
                let _ = writeln!(
                    failures,
                    "\n== {name} changed ==\n{}",
                    render_diff(&golden, &actual)
                );
            }
            Err(_) => {
                let _ = writeln!(failures, "\n== {name} has no golden file yet ==");
            }
        }
    }

    assert!(
        failures.is_empty(),
        "prompt(s) drifted from their golden files:{failures}\n\
         If the change is intentional, regenerate with\n\
         \n    CLAW_UPDATE_GOLDENS=1 cargo test -p claw-multiagent --test prompt_goldens\n\
         \nand commit the updated tests/goldens/*.txt — their diff in the PR is the \
         prompt review."
    );
}

#[test]
fn golden_inventory_is_complete() {
    // 12 role system prompts + 4 architect briefs + designer brief +
    // design-system prompt + visual QA + 2 JSON schemas = 21 snapshots.
    let snaps = snapshots();
    assert_eq!(snaps.len(), 21, "snapshot inventory changed");
    // Names are unique (two snapshots writing one file would mask drift).
    let mut names: Vec<&String> = snaps.iter().map(|(name, _)| name).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), 21, "duplicate golden file names");
    // No snapshot is accidentally empty except the roles without a brief
    // (all four briefed architects have content by construction).
    for (name, content) in &snaps {
        assert!(!content.trim().is_empty(), "{name} snapshot is empty");
    }
}
