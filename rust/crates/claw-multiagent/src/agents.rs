//! Agent execution layer: spawns real parallel sub-agents through the claw
//! `Agent` tool (each one an independent conversation loop on its own
//! thread, with role-restricted tools) and awaits their manifests.
//!
//! Everything here inherits the ecosystem integrations for free: agents
//! emit dashboard telemetry when `CLAW_DASHBOARD_EVENTS` is set, and their
//! manifests/outputs persist under the agent store.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// A spawned agent we can await.
#[derive(Debug, Clone)]
pub struct AgentHandle {
    pub name: String,
    pub manifest_file: PathBuf,
    pub output_file: PathBuf,
}

/// Terminal result of an awaited agent.
#[derive(Debug, Clone)]
pub struct AgentResult {
    pub name: String,
    pub status: String,
    pub report: String,
    pub error: Option<String>,
}

impl AgentResult {
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.status == "completed"
    }
}

/// Spawns one sub-agent (returns immediately; the agent runs on its own
/// thread). `system_context` is prepended to the prompt because the Agent
/// tool derives its own system prompt from the subagent type.
pub fn spawn_agent(
    name: &str,
    description: &str,
    model: &str,
    subagent_type: &str,
    system_context: &str,
    prompt: &str,
    allowed_write_paths: &[String],
) -> Result<AgentHandle, String> {
    let full_prompt = format!("{system_context}\n\n---\n\n{prompt}");
    let mut input = json!({
        "name": name,
        "description": description,
        "prompt": full_prompt,
        "subagent_type": subagent_type,
        "model": model,
    });
    if !allowed_write_paths.is_empty() {
        input["allowed_write_paths"] = json!(allowed_write_paths);
    }
    let raw = tools::execute_tool("Agent", &input)?;
    let manifest: Value =
        serde_json::from_str(&raw).map_err(|error| format!("invalid agent manifest: {error}"))?;
    let manifest_file = manifest["manifestFile"]
        .as_str()
        .ok_or("agent manifest missing manifestFile")?;
    let output_file = manifest["outputFile"]
        .as_str()
        .ok_or("agent manifest missing outputFile")?;
    Ok(AgentHandle {
        name: name.to_string(),
        manifest_file: PathBuf::from(manifest_file),
        output_file: PathBuf::from(output_file),
    })
}

/// Reads the agent's current status from its manifest ("running" until the
/// agent thread persists a terminal state).
fn read_status(handle: &AgentHandle) -> (String, Option<String>) {
    let Ok(content) = std::fs::read_to_string(&handle.manifest_file) else {
        return ("running".to_string(), None);
    };
    let Ok(manifest) = serde_json::from_str::<Value>(&content) else {
        return ("running".to_string(), None);
    };
    let status = manifest["status"].as_str().unwrap_or("running").to_string();
    let error = manifest["error"].as_str().map(ToString::to_string);
    (status, error)
}

fn collect_result(handle: &AgentHandle) -> AgentResult {
    let (status, error) = read_status(handle);
    let report = std::fs::read_to_string(&handle.output_file).unwrap_or_default();
    AgentResult {
        name: handle.name.clone(),
        status,
        report,
        error,
    }
}

/// Waits for all handles, invoking `on_complete` for each agent **as soon as
/// it finishes** (the spec's Supervisor reviews deliveries one by one, not in
/// batch). Agents still running past `timeout` are reported as timed out.
pub fn wait_all(
    handles: Vec<AgentHandle>,
    timeout: Duration,
    mut on_complete: impl FnMut(&AgentResult),
) -> Vec<AgentResult> {
    let deadline = Instant::now() + timeout;
    let mut pending: Vec<AgentHandle> = handles;
    let mut results: Vec<AgentResult> = Vec::new();

    while !pending.is_empty() {
        let mut still_pending = Vec::new();
        for handle in pending {
            let (status, _) = read_status(&handle);
            if status == "running" {
                still_pending.push(handle);
            } else {
                let result = collect_result(&handle);
                on_complete(&result);
                results.push(result);
            }
        }
        pending = still_pending;
        if pending.is_empty() {
            break;
        }
        if Instant::now() >= deadline {
            for handle in pending {
                let result = AgentResult {
                    name: handle.name.clone(),
                    status: "timeout".to_string(),
                    report: std::fs::read_to_string(&handle.output_file).unwrap_or_default(),
                    error: Some(format!("agent timed out after {}s", timeout.as_secs())),
                };
                on_complete(&result);
                results.push(result);
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    results
}

/// Extracts the first JSON object from an agent's free-form report: prefers
/// a ```json fence, falls back to the outermost brace span.
#[must_use]
pub fn extract_json(text: &str) -> Option<Value> {
    if let Some(fence_start) = text.find("```json") {
        let after = &text[fence_start + 7..];
        if let Some(fence_end) = after.find("```") {
            if let Ok(value) = serde_json::from_str::<Value>(after[..fence_end].trim()) {
                return Some(value);
            }
        }
    }
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str(&text[start..=end]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_prefers_fenced_block() {
        let text = "Report.\n```json\n{\"ok\": true}\n```\nnoise {\"ok\": false}";
        let value = extract_json(text).expect("fenced json");
        assert_eq!(value["ok"], true);
    }

    #[test]
    fn extract_json_falls_back_to_brace_span() {
        let text = "Plan follows: {\"vision\": \"x\", \"epics\": []} end.";
        let value = extract_json(text).expect("brace json");
        assert_eq!(value["vision"], "x");
    }

    #[test]
    fn extract_json_returns_none_without_object() {
        assert!(extract_json("no json here").is_none());
    }

    #[test]
    fn wait_all_reports_timeout_for_never_finishing_agents() {
        let dir = std::env::temp_dir().join(format!(
            "multiagent-wait-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        let manifest = dir.join("m.json");
        let output = dir.join("o.md");
        std::fs::write(&manifest, r#"{"status":"running"}"#).expect("manifest");
        std::fs::write(&output, "partial").expect("output");

        let mut seen = Vec::new();
        let results = wait_all(
            vec![AgentHandle {
                name: "stuck".to_string(),
                manifest_file: manifest,
                output_file: output,
            }],
            Duration::from_millis(50),
            |result| seen.push(result.status.clone()),
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, "timeout");
        assert_eq!(seen, vec!["timeout".to_string()]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn wait_all_collects_completed_agent_report() {
        let dir = std::env::temp_dir().join(format!(
            "multiagent-done-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        let manifest = dir.join("m.json");
        let output = dir.join("o.md");
        std::fs::write(&manifest, r#"{"status":"completed"}"#).expect("manifest");
        std::fs::write(&output, "the report").expect("output");

        let results = wait_all(
            vec![AgentHandle {
                name: "done".to_string(),
                manifest_file: manifest,
                output_file: output,
            }],
            Duration::from_secs(1),
            |_| {},
        );
        assert!(results[0].succeeded());
        assert!(results[0].report.contains("the report"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
