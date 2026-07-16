//! Cross-build memory: every Supervisor issue is distilled into one line of
//! `.multiagent/lessons.md`, and every developer agent of a LATER build (or
//! a later task of the same build) receives those lines in its prompt. The
//! cheapest way to stop a project from repeating its own mistakes.
//!
//! The file is a plain Markdown bullet list, capped FIFO (newest at the end)
//! and deduplicated exactly, written atomically like `BuildState`.

use std::path::{Path, PathBuf};

/// Maximum lines kept in `lessons.md`; older lines are dropped first (FIFO).
pub const LESSONS_CAP: usize = 40;

/// Prompt section header injected into developer prompts when lessons exist.
pub const LESSONS_SECTION_HEADER: &str =
    "## Lessons from previous builds in THIS project (avoid repeating these)";

/// Where the lessons live, next to the resume state.
#[must_use]
pub fn lessons_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".multiagent").join("lessons.md")
}

/// One supervision issue distilled to a single reusable line:
/// `- [severity] module: description`. Newlines are flattened and the
/// description capped so one verbose issue cannot dominate the file.
#[must_use]
pub fn distill_issue_line(severity: &str, module: &str, description: &str) -> String {
    let flatten = |text: &str| {
        text.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string()
    };
    let mut description = flatten(description);
    const MAX_DESCRIPTION: usize = 240;
    if description.chars().count() > MAX_DESCRIPTION {
        description = description.chars().take(MAX_DESCRIPTION).collect();
        description.push('…');
    }
    format!(
        "- [{}] {}: {description}",
        flatten(severity),
        flatten(module)
    )
}

/// Merges `new_lines` into the existing lessons text: exact duplicates are
/// skipped, new lines append at the END, and the total is capped to `cap`
/// lines FIFO (the oldest lines fall off the top). Pure so the policy is
/// testable without touching the filesystem.
#[must_use]
pub fn merge_lessons(existing: &str, new_lines: &[String], cap: usize) -> String {
    let mut lines: Vec<String> = existing
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .collect();
    for line in new_lines {
        let line = line.trim_end();
        if line.is_empty() || lines.iter().any(|existing| existing == line) {
            continue;
        }
        lines.push(line.to_string());
    }
    if lines.len() > cap {
        lines.drain(..lines.len() - cap);
    }
    if lines.is_empty() {
        return String::new();
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Appends distilled lessons to `.multiagent/lessons.md` (cap + dedup via
/// [`merge_lessons`]). Atomic temp+rename, same contract as `BuildState`:
/// a crash mid-write must never corrupt the accumulated memory. Best-effort —
/// lessons are advisory, so failures are swallowed.
pub fn record_lessons(project_dir: &Path, new_lines: &[String]) {
    if new_lines.iter().all(|line| line.trim().is_empty()) {
        return;
    }
    let path = lessons_path(project_dir);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let merged = merge_lessons(&existing, new_lines, LESSONS_CAP);
    let temp = path.with_extension("md.tmp");
    if std::fs::write(&temp, merged).is_ok() {
        let _ = std::fs::rename(&temp, &path);
    }
}

/// The lessons content when the file exists and is not blank.
#[must_use]
pub fn load_lessons(project_dir: &Path) -> Option<String> {
    let content = std::fs::read_to_string(lessons_path(project_dir)).ok()?;
    let trimmed = content.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// The prompt section a developer receives when lessons exist.
#[must_use]
pub fn lessons_prompt_section(content: &str) -> String {
    format!("\n\n{LESSONS_SECTION_HEADER}\n{}\n", content.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "multiagent-lessons-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        dir
    }

    #[test]
    fn distilled_lines_flatten_whitespace_and_cap_length() {
        assert_eq!(
            distill_issue_line("high", "auth", "token  leaks\nin logs"),
            "- [high] auth: token leaks in logs"
        );
        let long = "x".repeat(500);
        let line = distill_issue_line("low", "ui", &long);
        assert!(line.chars().count() < 260, "capped: {}", line.len());
        assert!(line.ends_with('…'));
    }

    #[test]
    fn merge_deduplicates_exactly_and_appends_newest_last() {
        let existing = "- [high] auth: a\n- [low] ui: b\n";
        let merged = merge_lessons(
            existing,
            &[
                "- [low] ui: b".to_string(),  // exact duplicate → skipped
                "- [med] api: c".to_string(), // new → appended at the end
                "   ".to_string(),            // blank → ignored
                "- [med] api: c".to_string(), // duplicate of the new line too
            ],
            LESSONS_CAP,
        );
        assert_eq!(merged, "- [high] auth: a\n- [low] ui: b\n- [med] api: c\n");
    }

    #[test]
    fn merge_caps_fifo_dropping_the_oldest() {
        let existing: String = (0..LESSONS_CAP)
            .map(|i| format!("- [low] m: lesson {i}\n"))
            .collect();
        let merged = merge_lessons(
            &existing,
            &["- [high] m: the newest lesson".to_string()],
            LESSONS_CAP,
        );
        let lines: Vec<&str> = merged.lines().collect();
        assert_eq!(lines.len(), LESSONS_CAP, "cap holds");
        // Oldest (lesson 0) fell off the top; newest is last.
        assert_eq!(lines[0], "- [low] m: lesson 1");
        assert_eq!(lines[LESSONS_CAP - 1], "- [high] m: the newest lesson");
    }

    #[test]
    fn record_and_load_round_trip_atomically() {
        let dir = temp_dir("roundtrip");
        assert_eq!(load_lessons(&dir), None, "no file yet");

        record_lessons(&dir, &["- [high] auth: first".to_string()]);
        record_lessons(
            &dir,
            &[
                "- [high] auth: first".to_string(), // dedup across writes
                "- [low] ui: second".to_string(),
            ],
        );
        let content = load_lessons(&dir).expect("lessons exist");
        assert_eq!(content, "- [high] auth: first\n- [low] ui: second");
        // Atomic write leaves no temp file behind.
        assert!(!lessons_path(&dir).with_extension("md.tmp").exists());

        // Blank-only input never creates or truncates the file.
        record_lessons(&dir, &[String::new()]);
        assert!(load_lessons(&dir).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prompt_section_carries_the_header_and_content() {
        let section = lessons_prompt_section("- [high] auth: never log tokens");
        assert!(section.contains(LESSONS_SECTION_HEADER));
        assert!(section.contains("never log tokens"));
    }
}
