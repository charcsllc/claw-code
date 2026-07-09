//! Builds the dashboard state from telemetry JSONL lines.
//!
//! The aggregator consumes only [`TelemetryEvent::SessionTrace`] records:
//! `SessionTracer` mirrors every analytics/http event as a session trace, so
//! consuming the other variants as well would double count.

use std::collections::{BTreeMap, VecDeque};

use serde::Serialize;
use serde_json::{Map, Value};
use telemetry::{SessionTraceRecord, TelemetryEvent};

/// A running agent with no events for this long is flagged as stale.
const STALE_AFTER_MS: u64 = 120_000;
/// Maximum number of usage samples kept for the dashboard chart.
const HISTORY_CAPACITY: usize = 600;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct TokenTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
}

impl TokenTotals {
    #[must_use]
    pub const fn total_tokens(&self) -> u64 {
        self.input_tokens
            + self.output_tokens
            + self.cache_creation_input_tokens
            + self.cache_read_input_tokens
    }

    fn add(&mut self, other: TokenTotals) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_creation_input_tokens += other.cache_creation_input_tokens;
        self.cache_read_input_tokens += other.cache_read_input_tokens;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Idle,
    Running,
    Blocked,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentState {
    pub id: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub status: AgentStatus,
    pub tokens: TokenTotals,
    pub total_tokens: u64,
    pub estimated_cost_usd: f64,
    pub turns: u64,
    pub requests: u64,
    pub first_seen_ms: u64,
    pub last_event_ms: u64,
    pub last_action: String,
    pub stale: bool,
}

impl AgentState {
    fn new(id: String, timestamp_ms: u64) -> Self {
        let label = id.clone();
        Self {
            id,
            label,
            model: None,
            status: AgentStatus::Idle,
            tokens: TokenTotals::default(),
            total_tokens: 0,
            estimated_cost_usd: 0.0,
            turns: 0,
            requests: 0,
            first_seen_ms: timestamp_ms,
            last_event_ms: timestamp_ms,
            last_action: "waiting".to_string(),
            stale: false,
        }
    }
}

/// One `message_usage` sample, kept for the cumulative token chart.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct UsageSample {
    pub timestamp_ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    /// Monotonic change counter; SSE consumers emit only when it moves.
    pub version: u64,
    pub generated_ms: u64,
    pub totals: TokenTotals,
    pub total_tokens: u64,
    pub estimated_cost_usd: f64,
    pub turns: u64,
    pub requests: u64,
    pub running: usize,
    pub completed: usize,
    pub failed: usize,
    pub agents: Vec<AgentState>,
    pub history: Vec<UsageSample>,
    pub parse_errors: u64,
}

#[derive(Debug, Default)]
pub struct Aggregator {
    agents: BTreeMap<String, AgentState>,
    history: VecDeque<UsageSample>,
    parse_errors: u64,
    version: u64,
}

impl Aggregator {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Forgets all accumulated state (used when the events file is truncated).
    pub fn reset(&mut self) {
        self.agents.clear();
        self.history.clear();
        self.parse_errors = 0;
        self.version += 1;
    }

    pub fn ingest_line(&mut self, line: &str) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return;
        }
        match serde_json::from_str::<TelemetryEvent>(trimmed) {
            Ok(TelemetryEvent::SessionTrace(record)) => {
                self.ingest_trace(&record);
                self.version += 1;
            }
            Ok(_) => {}
            Err(_) => {
                self.parse_errors += 1;
                self.version += 1;
            }
        }
    }

    fn ingest_trace(&mut self, record: &SessionTraceRecord) {
        // Agent lifecycle analytics may target an explicit agent id distinct
        // from the emitting session; everything else attributes to the session.
        if let Some((namespace, action)) = analytics_identity(record) {
            if namespace == telemetry::AGENT_NAMESPACE {
                self.ingest_agent_lifecycle(record, &action);
                return;
            }
            if namespace == telemetry::API_NAMESPACE && action == telemetry::MESSAGE_USAGE_ACTION {
                self.ingest_message_usage(record);
                return;
            }
            return;
        }

        let agent = self.agent_entry(record.session_id.clone(), record.timestamp_ms);
        match record.name.as_str() {
            "turn_started" => {
                agent.status = AgentStatus::Running;
                agent.turns += 1;
                agent.last_action = "turn started".to_string();
            }
            "turn_completed" => {
                agent.status = AgentStatus::Completed;
                agent.last_action = "turn completed".to_string();
            }
            "turn_failed" => {
                agent.status = AgentStatus::Failed;
                agent.last_action = attribute_string(&record.attributes, "error")
                    .unwrap_or_else(|| "turn failed".to_string());
            }
            "http_request_started" => {
                agent.requests += 1;
                agent.status = AgentStatus::Running;
                agent.last_action = "calling model".to_string();
            }
            "http_request_failed" => {
                agent.last_action = attribute_string(&record.attributes, "error")
                    .unwrap_or_else(|| "request failed".to_string());
            }
            name if name.starts_with("lane.") => {
                apply_lane_status(agent, name);
            }
            _ => {}
        }
    }

    fn ingest_agent_lifecycle(&mut self, record: &SessionTraceRecord, action: &str) {
        let agent_id = attribute_string(&record.attributes, "agent_id")
            .unwrap_or_else(|| record.session_id.clone());
        let agent = self.agent_entry(agent_id, record.timestamp_ms);
        match action {
            telemetry::AGENT_STARTED_ACTION => {
                agent.status = AgentStatus::Running;
                agent.last_action = "started".to_string();
                if let Some(label) = attribute_string(&record.attributes, "label") {
                    agent.label = label;
                }
            }
            telemetry::AGENT_FINISHED_ACTION => {
                agent.status = AgentStatus::Completed;
                agent.last_action = "finished".to_string();
            }
            telemetry::AGENT_FAILED_ACTION => {
                agent.status = AgentStatus::Failed;
                agent.last_action = attribute_string(&record.attributes, "error")
                    .unwrap_or_else(|| "failed".to_string());
            }
            _ => {}
        }
    }

    fn ingest_message_usage(&mut self, record: &SessionTraceRecord) {
        let usage = TokenTotals {
            input_tokens: attribute_u64(&record.attributes, "input_tokens"),
            output_tokens: attribute_u64(&record.attributes, "output_tokens"),
            cache_creation_input_tokens: attribute_u64(
                &record.attributes,
                "cache_creation_input_tokens",
            ),
            cache_read_input_tokens: attribute_u64(&record.attributes, "cache_read_input_tokens"),
        };
        // Prefer the full-precision numeric cost; fall back to parsing the
        // formatted "$0.0450" string for events from older claw builds.
        let cost = record
            .attributes
            .get("estimated_cost_usd_value")
            .and_then(Value::as_f64)
            .or_else(|| {
                attribute_string(&record.attributes, "estimated_cost_usd")
                    .and_then(|raw| raw.trim_start_matches('$').parse::<f64>().ok())
            })
            .unwrap_or(0.0);
        let model = attribute_string(&record.attributes, "model");
        let timestamp_ms = record.timestamp_ms;

        let agent = self.agent_entry(record.session_id.clone(), timestamp_ms);
        agent.tokens.add(usage);
        agent.total_tokens = agent.tokens.total_tokens();
        agent.estimated_cost_usd += cost;
        agent.last_action = "model responded".to_string();
        if model.is_some() {
            agent.model = model;
        }

        self.history.push_back(UsageSample {
            timestamp_ms,
            input_tokens: usage.input_tokens
                + usage.cache_creation_input_tokens
                + usage.cache_read_input_tokens,
            output_tokens: usage.output_tokens,
        });
        while self.history.len() > HISTORY_CAPACITY {
            self.history.pop_front();
        }
    }

    fn agent_entry(&mut self, id: String, timestamp_ms: u64) -> &mut AgentState {
        let agent = self
            .agents
            .entry(id.clone())
            .or_insert_with(|| AgentState::new(id, timestamp_ms));
        if timestamp_ms > agent.last_event_ms {
            agent.last_event_ms = timestamp_ms;
        }
        if agent.first_seen_ms == 0 {
            agent.first_seen_ms = timestamp_ms;
        }
        agent
    }

    #[must_use]
    pub fn snapshot(&self, now_ms: u64) -> Snapshot {
        let mut totals = TokenTotals::default();
        let mut cost = 0.0;
        let mut turns = 0;
        let mut requests = 0;
        let mut running = 0;
        let mut completed = 0;
        let mut failed = 0;

        let mut agents: Vec<AgentState> = self.agents.values().cloned().collect();
        for agent in &mut agents {
            totals.add(agent.tokens);
            cost += agent.estimated_cost_usd;
            turns += agent.turns;
            requests += agent.requests;
            agent.stale = agent.status == AgentStatus::Running
                && now_ms.saturating_sub(agent.last_event_ms) > STALE_AFTER_MS;
            match agent.status {
                AgentStatus::Running => running += 1,
                AgentStatus::Completed => completed += 1,
                AgentStatus::Failed => failed += 1,
                AgentStatus::Idle | AgentStatus::Blocked => {}
            }
        }
        agents.sort_by_key(|agent| agent.first_seen_ms);

        Snapshot {
            version: self.version,
            generated_ms: now_ms,
            total_tokens: totals.total_tokens(),
            totals,
            estimated_cost_usd: cost,
            turns,
            requests,
            running,
            completed,
            failed,
            agents,
            history: self.history.iter().copied().collect(),
            parse_errors: self.parse_errors,
        }
    }
}

/// Returns `(namespace, action)` when the trace is a mirrored analytics event.
fn analytics_identity(record: &SessionTraceRecord) -> Option<(String, String)> {
    if record.name != "analytics" {
        return None;
    }
    let namespace = attribute_string(&record.attributes, "namespace")?;
    let action = attribute_string(&record.attributes, "action")?;
    Some((namespace, action))
}

fn attribute_string(attributes: &Map<String, Value>, key: &str) -> Option<String> {
    attributes
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn attribute_u64(attributes: &Map<String, Value>, key: &str) -> u64 {
    attributes.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn apply_lane_status(agent: &mut AgentState, name: &str) {
    let status = match name {
        "lane.started" | "lane.ready" | "lane.green" => AgentStatus::Running,
        "lane.blocked" | "lane.red" => AgentStatus::Blocked,
        "lane.finished" | "lane.merged" | "lane.reconciled" | "lane.closed" | "lane.superseded" => {
            AgentStatus::Completed
        }
        "lane.failed" => AgentStatus::Failed,
        _ => return,
    };
    agent.status = status;
    agent.last_action = name.to_string();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn trace_line(session_id: &str, sequence: u64, name: &str, attributes: Value) -> String {
        json!({
            "type": "session_trace",
            "session_id": session_id,
            "sequence": sequence,
            "name": name,
            "timestamp_ms": 1_000 + sequence,
            "attributes": attributes,
        })
        .to_string()
    }

    fn usage_line(session_id: &str, sequence: u64, input: u64, output: u64) -> String {
        trace_line(
            session_id,
            sequence,
            "analytics",
            json!({
                "namespace": "api",
                "action": "message_usage",
                "model": "claude-sonnet-4-6",
                "input_tokens": input,
                "output_tokens": output,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0,
                "total_tokens": input + output,
                "estimated_cost_usd": "$0.1000",
            }),
        )
    }

    #[test]
    fn accumulates_usage_per_session_and_totals() {
        let mut aggregator = Aggregator::new();
        aggregator.ingest_line(&usage_line("session-a", 0, 100, 20));
        aggregator.ingest_line(&usage_line("session-a", 1, 50, 10));
        aggregator.ingest_line(&usage_line("session-b", 0, 30, 5));

        let snapshot = aggregator.snapshot(10_000);
        assert_eq!(snapshot.totals.input_tokens, 180);
        assert_eq!(snapshot.totals.output_tokens, 35);
        assert_eq!(snapshot.agents.len(), 2);
        let agent_a = snapshot
            .agents
            .iter()
            .find(|agent| agent.id == "session-a")
            .expect("session-a agent");
        assert_eq!(agent_a.tokens.input_tokens, 150);
        assert_eq!(agent_a.tokens.output_tokens, 30);
        assert_eq!(agent_a.model.as_deref(), Some("claude-sonnet-4-6"));
        assert!((snapshot.estimated_cost_usd - 0.3).abs() < 1e-9);
        assert_eq!(snapshot.history.len(), 3);
    }

    #[test]
    fn turn_lifecycle_drives_agent_status() {
        let mut aggregator = Aggregator::new();
        aggregator.ingest_line(&trace_line("session-a", 0, "turn_started", json!({})));
        let snapshot = aggregator.snapshot(2_000);
        assert_eq!(snapshot.agents[0].status, AgentStatus::Running);
        assert_eq!(snapshot.running, 1);

        aggregator.ingest_line(&trace_line("session-a", 1, "turn_completed", json!({})));
        let snapshot = aggregator.snapshot(3_000);
        assert_eq!(snapshot.agents[0].status, AgentStatus::Completed);
        assert_eq!(snapshot.completed, 1);
        assert_eq!(snapshot.agents[0].turns, 1);
    }

    #[test]
    fn agent_lifecycle_creates_labeled_card() {
        let mut aggregator = Aggregator::new();
        aggregator.ingest_line(&trace_line(
            "orchestrator",
            0,
            "analytics",
            json!({
                "namespace": "agent",
                "action": "started",
                "agent_id": "worker-1",
                "label": "refactor tests",
            }),
        ));
        aggregator.ingest_line(&trace_line(
            "orchestrator",
            1,
            "analytics",
            json!({"namespace": "agent", "action": "finished", "agent_id": "worker-1"}),
        ));

        let snapshot = aggregator.snapshot(5_000);
        let worker = snapshot
            .agents
            .iter()
            .find(|agent| agent.id == "worker-1")
            .expect("worker agent");
        assert_eq!(worker.label, "refactor tests");
        assert_eq!(worker.status, AgentStatus::Completed);
    }

    #[test]
    fn running_agent_with_old_events_is_stale() {
        let mut aggregator = Aggregator::new();
        aggregator.ingest_line(&trace_line("session-a", 0, "turn_started", json!({})));
        let snapshot = aggregator.snapshot(1_000 + STALE_AFTER_MS + 1_000);
        assert!(snapshot.agents[0].stale);
    }

    #[test]
    fn malformed_lines_are_counted_not_fatal() {
        let mut aggregator = Aggregator::new();
        aggregator.ingest_line("this is not json");
        aggregator.ingest_line("");
        let snapshot = aggregator.snapshot(1_000);
        assert_eq!(snapshot.parse_errors, 1);
        assert!(snapshot.agents.is_empty());
    }

    #[test]
    fn lane_events_map_to_statuses() {
        let mut aggregator = Aggregator::new();
        aggregator.ingest_line(&trace_line("lane-1", 0, "lane.started", json!({})));
        assert_eq!(
            aggregator.snapshot(2_000).agents[0].status,
            AgentStatus::Running
        );
        aggregator.ingest_line(&trace_line("lane-1", 1, "lane.merged", json!({})));
        assert_eq!(
            aggregator.snapshot(3_000).agents[0].status,
            AgentStatus::Completed
        );
    }
}
