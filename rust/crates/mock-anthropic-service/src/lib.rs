use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use api::{InputContentBlock, MessageRequest, MessageResponse, OutputContentBlock, Usage};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{oneshot, Mutex};
use tokio::task::JoinHandle;

pub const SCENARIO_PREFIX: &str = "PARITY_SCENARIO:";
pub const DEFAULT_MODEL: &str = "claude-sonnet-4-6";

/// Scenario label recorded for requests answered by a content route.
pub const ROUTED_SCENARIO_PREFIX: &str = "route:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedRequest {
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub scenario: String,
    pub stream: bool,
    pub raw_body: String,
}

/// A canned answer keyed by a substring of the raw request body. First
/// registered match wins, so register the most specific patterns first.
#[derive(Debug, Clone)]
struct ContentRoute {
    pattern: String,
    response_text: String,
    /// When set, the first matching request (no tool_result yet) is
    /// answered with this tool call; the follow-up request that carries the
    /// tool result gets `response_text`. Lets a routed "agent" actually
    /// exercise the real tool runtime (e.g. `write_file`).
    tool_use: Option<RoutedToolUse>,
}

/// The single tool call a tool route emits before its final text.
#[derive(Debug, Clone)]
struct RoutedToolUse {
    tool_name: String,
    input: Value,
}

pub struct MockAnthropicService {
    base_url: String,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    routes: Arc<Mutex<Vec<ContentRoute>>>,
    shutdown: Option<oneshot::Sender<()>>,
    join_handle: JoinHandle<()>,
}

impl MockAnthropicService {
    pub async fn spawn() -> io::Result<Self> {
        Self::spawn_on("127.0.0.1:0").await
    }

    pub async fn spawn_on(bind_addr: &str) -> io::Result<Self> {
        let listener = TcpListener::bind(bind_addr).await?;
        let address = listener.local_addr()?;
        let requests = Arc::new(Mutex::new(Vec::new()));
        let routes: Arc<Mutex<Vec<ContentRoute>>> = Arc::new(Mutex::new(Vec::new()));
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let request_state = Arc::clone(&requests);
        let route_state = Arc::clone(&routes);

        let join_handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accepted = listener.accept() => {
                        let Ok((socket, _)) = accepted else {
                            break;
                        };
                        let request_state = Arc::clone(&request_state);
                        let route_state = Arc::clone(&route_state);
                        tokio::spawn(async move {
                            let _ = handle_connection(socket, request_state, route_state).await;
                        });
                    }
                }
            }
        });

        Ok(Self {
            base_url: format!("http://{address}"),
            requests,
            routes,
            shutdown: Some(shutdown_tx),
            join_handle,
        })
    }

    #[must_use]
    pub fn base_url(&self) -> String {
        self.base_url.clone()
    }

    pub async fn captured_requests(&self) -> Vec<CapturedRequest> {
        self.requests.lock().await.clone()
    }

    /// Registers a content route: any request whose raw body contains
    /// `pattern` is answered with `response_text` (as a plain assistant text
    /// message, streamed or not to match the request). Routes are checked in
    /// registration order and take precedence over the parity scenarios;
    /// requests matching no route keep the existing scenario behavior.
    pub async fn route(&self, pattern: impl Into<String>, response_text: impl Into<String>) {
        self.routes.lock().await.push(ContentRoute {
            pattern: pattern.into(),
            response_text: response_text.into(),
            tool_use: None,
        });
    }

    /// Registers a tool route: a matching request that carries no
    /// tool_result yet is answered with ONE `tool_use` block
    /// (`tool_name`/`input`, streamed or not to match the request); the
    /// follow-up request containing the tool result is answered with
    /// `response_text`. `/v1/messages/count_tokens` preflights always get
    /// the plain-text shape (they never execute tools). Same precedence and
    /// ordering rules as [`Self::route`].
    pub async fn route_tool_use(
        &self,
        pattern: impl Into<String>,
        tool_name: impl Into<String>,
        input: Value,
        response_text: impl Into<String>,
    ) {
        self.routes.lock().await.push(ContentRoute {
            pattern: pattern.into(),
            response_text: response_text.into(),
            tool_use: Some(RoutedToolUse {
                tool_name: tool_name.into(),
                input,
            }),
        });
    }
}

impl Drop for MockAnthropicService {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.join_handle.abort();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scenario {
    StreamingText,
    ReadFileRoundtrip,
    GrepChunkAssembly,
    WriteFileAllowed,
    WriteFileDenied,
    MultiToolTurnRoundtrip,
    BashStdoutRoundtrip,
    BashPermissionPromptApproved,
    BashPermissionPromptDenied,
    PluginToolRoundtrip,
    AutoCompactTriggered,
    TokenCostReporting,
}

impl Scenario {
    fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "streaming_text" => Some(Self::StreamingText),
            "read_file_roundtrip" => Some(Self::ReadFileRoundtrip),
            "grep_chunk_assembly" => Some(Self::GrepChunkAssembly),
            "write_file_allowed" => Some(Self::WriteFileAllowed),
            "write_file_denied" => Some(Self::WriteFileDenied),
            "multi_tool_turn_roundtrip" => Some(Self::MultiToolTurnRoundtrip),
            "bash_stdout_roundtrip" => Some(Self::BashStdoutRoundtrip),
            "bash_permission_prompt_approved" => Some(Self::BashPermissionPromptApproved),
            "bash_permission_prompt_denied" => Some(Self::BashPermissionPromptDenied),
            "plugin_tool_roundtrip" => Some(Self::PluginToolRoundtrip),
            "auto_compact_triggered" => Some(Self::AutoCompactTriggered),
            "token_cost_reporting" => Some(Self::TokenCostReporting),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::StreamingText => "streaming_text",
            Self::ReadFileRoundtrip => "read_file_roundtrip",
            Self::GrepChunkAssembly => "grep_chunk_assembly",
            Self::WriteFileAllowed => "write_file_allowed",
            Self::WriteFileDenied => "write_file_denied",
            Self::MultiToolTurnRoundtrip => "multi_tool_turn_roundtrip",
            Self::BashStdoutRoundtrip => "bash_stdout_roundtrip",
            Self::BashPermissionPromptApproved => "bash_permission_prompt_approved",
            Self::BashPermissionPromptDenied => "bash_permission_prompt_denied",
            Self::PluginToolRoundtrip => "plugin_tool_roundtrip",
            Self::AutoCompactTriggered => "auto_compact_triggered",
            Self::TokenCostReporting => "token_cost_reporting",
        }
    }
}

async fn handle_connection(
    mut socket: tokio::net::TcpStream,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    routes: Arc<Mutex<Vec<ContentRoute>>>,
) -> io::Result<()> {
    let (method, path, headers, raw_body) = read_http_request(&mut socket).await?;
    let request: MessageRequest = serde_json::from_str(&raw_body)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;

    // Content routes first: they answer by substring of the raw JSON body
    // (patterns should avoid quotes/newlines, which arrive JSON-escaped).
    let matched_route = routes
        .lock()
        .await
        .iter()
        .find(|route| raw_body.contains(&route.pattern))
        .cloned();
    if let Some(route) = matched_route {
        // Token-count preflights never execute tools: they always get the
        // plain-text shape even on a tool route.
        let is_preflight = path.ends_with("/count_tokens");
        requests.lock().await.push(CapturedRequest {
            method,
            path,
            headers,
            scenario: format!("{ROUTED_SCENARIO_PREFIX}{}", route.pattern),
            stream: request.stream,
            raw_body,
        });
        let response = match &route.tool_use {
            Some(tool_use) if !is_preflight && latest_tool_result(&request).is_none() => {
                build_routed_tool_response(&request, tool_use)
            }
            _ => build_routed_response(&request, &route.response_text),
        };
        socket.write_all(response.as_bytes()).await?;
        return Ok(());
    }

    let scenario = detect_scenario(&request)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing parity scenario"))?;

    requests.lock().await.push(CapturedRequest {
        method,
        path,
        headers,
        scenario: scenario.name().to_string(),
        stream: request.stream,
        raw_body,
    });

    let response = build_http_response(&request, scenario);
    socket.write_all(response.as_bytes()).await?;
    Ok(())
}

/// The tool_use half of a tool route, in the transport the caller asked
/// for. Ids are unique per response so parallel routed agents never collide.
fn build_routed_tool_response(request: &MessageRequest, tool_use: &RoutedToolUse) -> String {
    let tool_id = format!("toolu_route_{}", unique_message_id());
    if request.stream {
        let input_json = tool_use.input.to_string();
        http_response(
            "200 OK",
            "text/event-stream",
            &tool_use_sse(&tool_id, &tool_use.tool_name, &[input_json.as_str()]),
            &[("x-request-id", "req_content_route_tool")],
        )
    } else {
        let response = tool_message_response(
            &unique_message_id(),
            &tool_id,
            &tool_use.tool_name,
            tool_use.input.clone(),
        );
        http_response(
            "200 OK",
            "application/json",
            &serde_json::to_string(&response).expect("message response should serialize"),
            &[("request-id", "req_content_route_tool")],
        )
    }
}

/// A routed request is answered with a plain assistant text message, in the
/// transport the caller asked for (SSE stream or single JSON body).
fn build_routed_response(request: &MessageRequest, text: &str) -> String {
    if request.stream {
        http_response(
            "200 OK",
            "text/event-stream",
            &final_text_sse(text),
            &[("x-request-id", "req_content_route")],
        )
    } else {
        let response = text_message_response(&unique_message_id(), text);
        http_response(
            "200 OK",
            "application/json",
            &serde_json::to_string(&response).expect("message response should serialize"),
            &[("request-id", "req_content_route")],
        )
    }
}

async fn read_http_request(
    socket: &mut tokio::net::TcpStream,
) -> io::Result<(String, String, HashMap<String, String>, String)> {
    let mut buffer = Vec::new();
    let mut header_end = None;

    loop {
        let mut chunk = [0_u8; 1024];
        let read = socket.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(position) = find_header_end(&buffer) {
            header_end = Some(position);
            break;
        }
    }

    let header_end = header_end
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "missing http headers"))?;
    let (header_bytes, remaining) = buffer.split_at(header_end);
    let header_text = String::from_utf8(header_bytes.to_vec())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    let mut lines = header_text.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing request line"))?;
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing method"))?
        .to_string();
    let path = request_parts
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing path"))?
        .to_string();

    let mut headers = HashMap::new();
    let mut content_length = 0_usize;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line.split_once(':').ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "malformed http header line")
        })?;
        let value = value.trim().to_string();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.parse().map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid content-length: {error}"),
                )
            })?;
        }
        headers.insert(name.to_ascii_lowercase(), value);
    }

    let mut body = remaining[4..].to_vec();
    while body.len() < content_length {
        let mut chunk = vec![0_u8; content_length - body.len()];
        let read = socket.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }

    let body = String::from_utf8(body)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    Ok((method, path, headers, body))
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

fn detect_scenario(request: &MessageRequest) -> Option<Scenario> {
    request.messages.iter().rev().find_map(|message| {
        message.content.iter().rev().find_map(|block| match block {
            InputContentBlock::Text { text } => text
                .split_whitespace()
                .find_map(|token| token.strip_prefix(SCENARIO_PREFIX))
                .and_then(Scenario::parse),
            _ => None,
        })
    })
}

fn latest_tool_result(request: &MessageRequest) -> Option<(String, bool)> {
    request.messages.iter().rev().find_map(|message| {
        message.content.iter().rev().find_map(|block| match block {
            InputContentBlock::ToolResult {
                content, is_error, ..
            } => Some((flatten_tool_result_content(content), *is_error)),
            _ => None,
        })
    })
}

fn tool_results_by_name(request: &MessageRequest) -> HashMap<String, (String, bool)> {
    let mut tool_names_by_id = HashMap::new();
    for message in &request.messages {
        for block in &message.content {
            if let InputContentBlock::ToolUse { id, name, .. } = block {
                tool_names_by_id.insert(id.clone(), name.clone());
            }
        }
    }

    let mut results = HashMap::new();
    for message in request.messages.iter().rev() {
        for block in message.content.iter().rev() {
            if let InputContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } = block
            {
                let tool_name = tool_names_by_id
                    .get(tool_use_id)
                    .cloned()
                    .unwrap_or_else(|| tool_use_id.clone());
                results
                    .entry(tool_name)
                    .or_insert_with(|| (flatten_tool_result_content(content), *is_error));
            }
        }
    }
    results
}

fn flatten_tool_result_content(content: &[api::ToolResultContentBlock]) -> String {
    content
        .iter()
        .map(|block| match block {
            api::ToolResultContentBlock::Text { text } => text.clone(),
            api::ToolResultContentBlock::Json { value } => value.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[allow(clippy::too_many_lines)]
fn build_http_response(request: &MessageRequest, scenario: Scenario) -> String {
    let response = if request.stream {
        let body = build_stream_body(request, scenario);
        return http_response(
            "200 OK",
            "text/event-stream",
            &body,
            &[("x-request-id", request_id_for(scenario))],
        );
    } else {
        build_message_response(request, scenario)
    };

    http_response(
        "200 OK",
        "application/json",
        &serde_json::to_string(&response).expect("message response should serialize"),
        &[("request-id", request_id_for(scenario))],
    )
}

#[allow(clippy::too_many_lines)]
fn build_stream_body(request: &MessageRequest, scenario: Scenario) -> String {
    match scenario {
        Scenario::StreamingText => streaming_text_sse(),
        Scenario::ReadFileRoundtrip => match latest_tool_result(request) {
            Some((tool_output, _)) => final_text_sse(&format!(
                "read_file roundtrip complete: {}",
                extract_read_content(&tool_output)
            )),
            None => tool_use_sse(
                "toolu_read_fixture",
                "read_file",
                &[r#"{"path":"fixture.txt"}"#],
            ),
        },
        Scenario::GrepChunkAssembly => match latest_tool_result(request) {
            Some((tool_output, _)) => final_text_sse(&format!(
                "grep_search matched {} occurrences",
                extract_num_matches(&tool_output)
            )),
            None => tool_use_sse(
                "toolu_grep_fixture",
                "grep_search",
                &[
                    "{\"pattern\":\"par",
                    "ity\",\"path\":\"fixture.txt\"",
                    ",\"output_mode\":\"count\"}",
                ],
            ),
        },
        Scenario::WriteFileAllowed => match latest_tool_result(request) {
            Some((tool_output, _)) => final_text_sse(&format!(
                "write_file succeeded: {}",
                extract_file_path(&tool_output)
            )),
            None => tool_use_sse(
                "toolu_write_allowed",
                "write_file",
                &[r#"{"path":"generated/output.txt","content":"created by mock service\n"}"#],
            ),
        },
        Scenario::WriteFileDenied => match latest_tool_result(request) {
            Some((tool_output, _)) => {
                final_text_sse(&format!("write_file denied as expected: {tool_output}"))
            }
            None => tool_use_sse(
                "toolu_write_denied",
                "write_file",
                &[r#"{"path":"generated/denied.txt","content":"should not exist\n"}"#],
            ),
        },
        Scenario::MultiToolTurnRoundtrip => {
            let tool_results = tool_results_by_name(request);
            match (
                tool_results.get("read_file"),
                tool_results.get("grep_search"),
            ) {
                (Some((read_output, _)), Some((grep_output, _))) => final_text_sse(&format!(
                    "multi-tool roundtrip complete: {} / {} occurrences",
                    extract_read_content(read_output),
                    extract_num_matches(grep_output)
                )),
                _ => tool_uses_sse(&[
                    ToolUseSse {
                        tool_id: "toolu_multi_read",
                        tool_name: "read_file",
                        partial_json_chunks: &[r#"{"path":"fixture.txt"}"#],
                    },
                    ToolUseSse {
                        tool_id: "toolu_multi_grep",
                        tool_name: "grep_search",
                        partial_json_chunks: &[
                            "{\"pattern\":\"par",
                            "ity\",\"path\":\"fixture.txt\"",
                            ",\"output_mode\":\"count\"}",
                        ],
                    },
                ]),
            }
        }
        Scenario::BashStdoutRoundtrip => match latest_tool_result(request) {
            Some((tool_output, _)) => final_text_sse(&format!(
                "bash completed: {}",
                extract_bash_stdout(&tool_output)
            )),
            None => tool_use_sse(
                "toolu_bash_stdout",
                "bash",
                &[r#"{"command":"printf 'alpha from bash'","timeout":1000}"#],
            ),
        },
        Scenario::BashPermissionPromptApproved => match latest_tool_result(request) {
            Some((tool_output, is_error)) => {
                if is_error {
                    final_text_sse(&format!("bash approval unexpectedly failed: {tool_output}"))
                } else {
                    final_text_sse(&format!(
                        "bash approved and executed: {}",
                        extract_bash_stdout(&tool_output)
                    ))
                }
            }
            None => tool_use_sse(
                "toolu_bash_prompt_allow",
                "bash",
                &[r#"{"command":"printf 'approved via prompt'","timeout":1000}"#],
            ),
        },
        Scenario::BashPermissionPromptDenied => match latest_tool_result(request) {
            Some((tool_output, _)) => {
                final_text_sse(&format!("bash denied as expected: {tool_output}"))
            }
            None => tool_use_sse(
                "toolu_bash_prompt_deny",
                "bash",
                &[r#"{"command":"printf 'should not run'","timeout":1000}"#],
            ),
        },
        Scenario::PluginToolRoundtrip => match latest_tool_result(request) {
            Some((tool_output, _)) => final_text_sse(&format!(
                "plugin tool completed: {}",
                extract_plugin_message(&tool_output)
            )),
            None => tool_use_sse(
                "toolu_plugin_echo",
                "plugin_echo",
                &[r#"{"message":"hello from plugin parity"}"#],
            ),
        },
        Scenario::AutoCompactTriggered => {
            final_text_sse_with_usage("auto compact parity complete.", 50_000, 200)
        }
        Scenario::TokenCostReporting => {
            final_text_sse_with_usage("token cost reporting parity complete.", 1_000, 500)
        }
    }
}

#[allow(clippy::too_many_lines)]
fn build_message_response(request: &MessageRequest, scenario: Scenario) -> MessageResponse {
    match scenario {
        Scenario::StreamingText => text_message_response(
            "msg_streaming_text",
            "Mock streaming says hello from the parity harness.",
        ),
        Scenario::ReadFileRoundtrip => match latest_tool_result(request) {
            Some((tool_output, _)) => text_message_response(
                "msg_read_file_final",
                &format!(
                    "read_file roundtrip complete: {}",
                    extract_read_content(&tool_output)
                ),
            ),
            None => tool_message_response(
                "msg_read_file_tool",
                "toolu_read_fixture",
                "read_file",
                json!({"path": "fixture.txt"}),
            ),
        },
        Scenario::GrepChunkAssembly => match latest_tool_result(request) {
            Some((tool_output, _)) => text_message_response(
                "msg_grep_final",
                &format!(
                    "grep_search matched {} occurrences",
                    extract_num_matches(&tool_output)
                ),
            ),
            None => tool_message_response(
                "msg_grep_tool",
                "toolu_grep_fixture",
                "grep_search",
                json!({"pattern": "parity", "path": "fixture.txt", "output_mode": "count"}),
            ),
        },
        Scenario::WriteFileAllowed => match latest_tool_result(request) {
            Some((tool_output, _)) => text_message_response(
                "msg_write_allowed_final",
                &format!("write_file succeeded: {}", extract_file_path(&tool_output)),
            ),
            None => tool_message_response(
                "msg_write_allowed_tool",
                "toolu_write_allowed",
                "write_file",
                json!({"path": "generated/output.txt", "content": "created by mock service\n"}),
            ),
        },
        Scenario::WriteFileDenied => match latest_tool_result(request) {
            Some((tool_output, _)) => text_message_response(
                "msg_write_denied_final",
                &format!("write_file denied as expected: {tool_output}"),
            ),
            None => tool_message_response(
                "msg_write_denied_tool",
                "toolu_write_denied",
                "write_file",
                json!({"path": "generated/denied.txt", "content": "should not exist\n"}),
            ),
        },
        Scenario::MultiToolTurnRoundtrip => {
            let tool_results = tool_results_by_name(request);
            match (
                tool_results.get("read_file"),
                tool_results.get("grep_search"),
            ) {
                (Some((read_output, _)), Some((grep_output, _))) => text_message_response(
                    "msg_multi_tool_final",
                    &format!(
                        "multi-tool roundtrip complete: {} / {} occurrences",
                        extract_read_content(read_output),
                        extract_num_matches(grep_output)
                    ),
                ),
                _ => tool_message_response_many(
                    "msg_multi_tool_start",
                    &[
                        ToolUseMessage {
                            tool_id: "toolu_multi_read",
                            tool_name: "read_file",
                            input: json!({"path": "fixture.txt"}),
                        },
                        ToolUseMessage {
                            tool_id: "toolu_multi_grep",
                            tool_name: "grep_search",
                            input: json!({"pattern": "parity", "path": "fixture.txt", "output_mode": "count"}),
                        },
                    ],
                ),
            }
        }
        Scenario::BashStdoutRoundtrip => match latest_tool_result(request) {
            Some((tool_output, _)) => text_message_response(
                "msg_bash_stdout_final",
                &format!("bash completed: {}", extract_bash_stdout(&tool_output)),
            ),
            None => tool_message_response(
                "msg_bash_stdout_tool",
                "toolu_bash_stdout",
                "bash",
                json!({"command": "printf 'alpha from bash'", "timeout": 1000}),
            ),
        },
        Scenario::BashPermissionPromptApproved => match latest_tool_result(request) {
            Some((tool_output, is_error)) => {
                if is_error {
                    text_message_response(
                        "msg_bash_prompt_allow_error",
                        &format!("bash approval unexpectedly failed: {tool_output}"),
                    )
                } else {
                    text_message_response(
                        "msg_bash_prompt_allow_final",
                        &format!(
                            "bash approved and executed: {}",
                            extract_bash_stdout(&tool_output)
                        ),
                    )
                }
            }
            None => tool_message_response(
                "msg_bash_prompt_allow_tool",
                "toolu_bash_prompt_allow",
                "bash",
                json!({"command": "printf 'approved via prompt'", "timeout": 1000}),
            ),
        },
        Scenario::BashPermissionPromptDenied => match latest_tool_result(request) {
            Some((tool_output, _)) => text_message_response(
                "msg_bash_prompt_deny_final",
                &format!("bash denied as expected: {tool_output}"),
            ),
            None => tool_message_response(
                "msg_bash_prompt_deny_tool",
                "toolu_bash_prompt_deny",
                "bash",
                json!({"command": "printf 'should not run'", "timeout": 1000}),
            ),
        },
        Scenario::PluginToolRoundtrip => match latest_tool_result(request) {
            Some((tool_output, _)) => text_message_response(
                "msg_plugin_tool_final",
                &format!(
                    "plugin tool completed: {}",
                    extract_plugin_message(&tool_output)
                ),
            ),
            None => tool_message_response(
                "msg_plugin_tool_start",
                "toolu_plugin_echo",
                "plugin_echo",
                json!({"message": "hello from plugin parity"}),
            ),
        },
        Scenario::AutoCompactTriggered => text_message_response_with_usage(
            "msg_auto_compact_triggered",
            "auto compact parity complete.",
            50_000,
            200,
        ),
        Scenario::TokenCostReporting => text_message_response_with_usage(
            "msg_token_cost_reporting",
            "token cost reporting parity complete.",
            1_000,
            500,
        ),
    }
}

fn request_id_for(scenario: Scenario) -> &'static str {
    match scenario {
        Scenario::StreamingText => "req_streaming_text",
        Scenario::ReadFileRoundtrip => "req_read_file_roundtrip",
        Scenario::GrepChunkAssembly => "req_grep_chunk_assembly",
        Scenario::WriteFileAllowed => "req_write_file_allowed",
        Scenario::WriteFileDenied => "req_write_file_denied",
        Scenario::MultiToolTurnRoundtrip => "req_multi_tool_turn_roundtrip",
        Scenario::BashStdoutRoundtrip => "req_bash_stdout_roundtrip",
        Scenario::BashPermissionPromptApproved => "req_bash_permission_prompt_approved",
        Scenario::BashPermissionPromptDenied => "req_bash_permission_prompt_denied",
        Scenario::PluginToolRoundtrip => "req_plugin_tool_roundtrip",
        Scenario::AutoCompactTriggered => "req_auto_compact_triggered",
        Scenario::TokenCostReporting => "req_token_cost_reporting",
    }
}

fn http_response(status: &str, content_type: &str, body: &str, headers: &[(&str, &str)]) -> String {
    let mut extra_headers = String::new();
    for (name, value) in headers {
        use std::fmt::Write as _;
        write!(&mut extra_headers, "{name}: {value}\r\n").expect("header write should succeed");
    }
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\n{extra_headers}content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn text_message_response(id: &str, text: &str) -> MessageResponse {
    MessageResponse {
        id: id.to_string(),
        kind: "message".to_string(),
        role: "assistant".to_string(),
        content: vec![OutputContentBlock::Text {
            text: text.to_string(),
        }],
        model: DEFAULT_MODEL.to_string(),
        stop_reason: Some("end_turn".to_string()),
        stop_sequence: None,
        usage: Usage {
            input_tokens: 10,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            output_tokens: 6,
        },
        request_id: None,
    }
}

fn text_message_response_with_usage(
    id: &str,
    text: &str,
    input_tokens: u32,
    output_tokens: u32,
) -> MessageResponse {
    MessageResponse {
        id: id.to_string(),
        kind: "message".to_string(),
        role: "assistant".to_string(),
        content: vec![OutputContentBlock::Text {
            text: text.to_string(),
        }],
        model: DEFAULT_MODEL.to_string(),
        stop_reason: Some("end_turn".to_string()),
        stop_sequence: None,
        usage: Usage {
            input_tokens,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            output_tokens,
        },
        request_id: None,
    }
}

fn tool_message_response(
    id: &str,
    tool_id: &str,
    tool_name: &str,
    input: Value,
) -> MessageResponse {
    tool_message_response_many(
        id,
        &[ToolUseMessage {
            tool_id,
            tool_name,
            input,
        }],
    )
}

struct ToolUseMessage<'a> {
    tool_id: &'a str,
    tool_name: &'a str,
    input: Value,
}

fn tool_message_response_many(id: &str, tool_uses: &[ToolUseMessage<'_>]) -> MessageResponse {
    MessageResponse {
        id: id.to_string(),
        kind: "message".to_string(),
        role: "assistant".to_string(),
        content: tool_uses
            .iter()
            .map(|tool_use| OutputContentBlock::ToolUse {
                id: tool_use.tool_id.to_string(),
                name: tool_use.tool_name.to_string(),
                input: tool_use.input.clone(),
            })
            .collect(),
        model: DEFAULT_MODEL.to_string(),
        stop_reason: Some("tool_use".to_string()),
        stop_sequence: None,
        usage: Usage {
            input_tokens: 10,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            output_tokens: 3,
        },
        request_id: None,
    }
}

fn streaming_text_sse() -> String {
    let mut body = String::new();
    append_sse(
        &mut body,
        "message_start",
        json!({
            "type": "message_start",
            "message": {
                "id": "msg_streaming_text",
                "type": "message",
                "role": "assistant",
                "content": [],
                "model": DEFAULT_MODEL,
                "stop_reason": null,
                "stop_sequence": null,
                "usage": usage_json(11, 0)
            }
        }),
    );
    append_sse(
        &mut body,
        "content_block_start",
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "text", "text": ""}
        }),
    );
    append_sse(
        &mut body,
        "content_block_delta",
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": "Mock streaming "}
        }),
    );
    append_sse(
        &mut body,
        "content_block_delta",
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": "says hello from the parity harness."}
        }),
    );
    append_sse(
        &mut body,
        "content_block_stop",
        json!({
            "type": "content_block_stop",
            "index": 0
        }),
    );
    append_sse(
        &mut body,
        "message_delta",
        json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn", "stop_sequence": null},
            "usage": usage_json(11, 8)
        }),
    );
    append_sse(&mut body, "message_stop", json!({"type": "message_stop"}));
    body
}

fn tool_use_sse(tool_id: &str, tool_name: &str, partial_json_chunks: &[&str]) -> String {
    tool_uses_sse(&[ToolUseSse {
        tool_id,
        tool_name,
        partial_json_chunks,
    }])
}

struct ToolUseSse<'a> {
    tool_id: &'a str,
    tool_name: &'a str,
    partial_json_chunks: &'a [&'a str],
}

fn tool_uses_sse(tool_uses: &[ToolUseSse<'_>]) -> String {
    let mut body = String::new();
    let message_id = tool_uses.first().map_or_else(
        || "msg_tool_use".to_string(),
        |tool_use| format!("msg_{}", tool_use.tool_id),
    );
    append_sse(
        &mut body,
        "message_start",
        json!({
            "type": "message_start",
            "message": {
                "id": message_id,
                "type": "message",
                "role": "assistant",
                "content": [],
                "model": DEFAULT_MODEL,
                "stop_reason": null,
                "stop_sequence": null,
                "usage": usage_json(12, 0)
            }
        }),
    );
    for (index, tool_use) in tool_uses.iter().enumerate() {
        append_sse(
            &mut body,
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": index,
                "content_block": {
                    "type": "tool_use",
                    "id": tool_use.tool_id,
                    "name": tool_use.tool_name,
                    "input": {}
                }
            }),
        );
        for chunk in tool_use.partial_json_chunks {
            append_sse(
                &mut body,
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": index,
                    "delta": {"type": "input_json_delta", "partial_json": chunk}
                }),
            );
        }
        append_sse(
            &mut body,
            "content_block_stop",
            json!({
                "type": "content_block_stop",
                "index": index
            }),
        );
    }
    append_sse(
        &mut body,
        "message_delta",
        json!({
            "type": "message_delta",
            "delta": {"stop_reason": "tool_use", "stop_sequence": null},
            "usage": usage_json(12, 4)
        }),
    );
    append_sse(&mut body, "message_stop", json!({"type": "message_stop"}));
    body
}

fn final_text_sse(text: &str) -> String {
    let mut body = String::new();
    append_sse(
        &mut body,
        "message_start",
        json!({
            "type": "message_start",
            "message": {
                "id": unique_message_id(),
                "type": "message",
                "role": "assistant",
                "content": [],
                "model": DEFAULT_MODEL,
                "stop_reason": null,
                "stop_sequence": null,
                "usage": usage_json(14, 0)
            }
        }),
    );
    append_sse(
        &mut body,
        "content_block_start",
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "text", "text": ""}
        }),
    );
    append_sse(
        &mut body,
        "content_block_delta",
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": text}
        }),
    );
    append_sse(
        &mut body,
        "content_block_stop",
        json!({
            "type": "content_block_stop",
            "index": 0
        }),
    );
    append_sse(
        &mut body,
        "message_delta",
        json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn", "stop_sequence": null},
            "usage": usage_json(14, 7)
        }),
    );
    append_sse(&mut body, "message_stop", json!({"type": "message_stop"}));
    body
}

fn final_text_sse_with_usage(text: &str, input_tokens: u32, output_tokens: u32) -> String {
    let mut body = String::new();
    append_sse(
        &mut body,
        "message_start",
        json!({
            "type": "message_start",
            "message": {
                "id": unique_message_id(),
                "type": "message",
                "role": "assistant",
                "content": [],
                "model": DEFAULT_MODEL,
                "stop_reason": null,
                "stop_sequence": null,
                "usage": {
                    "input_tokens": input_tokens,
                    "cache_creation_input_tokens": 0,
                    "cache_read_input_tokens": 0,
                    "output_tokens": 0
                }
            }
        }),
    );
    append_sse(
        &mut body,
        "content_block_start",
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "text", "text": ""}
        }),
    );
    append_sse(
        &mut body,
        "content_block_delta",
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": text}
        }),
    );
    append_sse(
        &mut body,
        "content_block_stop",
        json!({
            "type": "content_block_stop",
            "index": 0
        }),
    );
    append_sse(
        &mut body,
        "message_delta",
        json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn", "stop_sequence": null},
            "usage": {
                "input_tokens": input_tokens,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0,
                "output_tokens": output_tokens
            }
        }),
    );
    append_sse(&mut body, "message_stop", json!({"type": "message_stop"}));
    body
}

#[allow(clippy::needless_pass_by_value)]
fn append_sse(buffer: &mut String, event: &str, payload: Value) {
    use std::fmt::Write as _;
    writeln!(buffer, "event: {event}").expect("event write should succeed");
    writeln!(buffer, "data: {payload}").expect("payload write should succeed");
    buffer.push('\n');
}

fn usage_json(input_tokens: u32, output_tokens: u32) -> Value {
    json!({
        "input_tokens": input_tokens,
        "cache_creation_input_tokens": 0,
        "cache_read_input_tokens": 0,
        "output_tokens": output_tokens
    })
}

fn unique_message_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    format!("msg_{nanos}")
}

fn extract_read_content(tool_output: &str) -> String {
    serde_json::from_str::<Value>(tool_output)
        .ok()
        .and_then(|value| {
            value
                .get("file")
                .and_then(|file| file.get("content"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| tool_output.trim().to_string())
}

#[allow(clippy::cast_possible_truncation)]
fn extract_num_matches(tool_output: &str) -> usize {
    serde_json::from_str::<Value>(tool_output)
        .ok()
        .and_then(|value| value.get("numMatches").and_then(Value::as_u64))
        .unwrap_or(0) as usize
}

fn extract_file_path(tool_output: &str) -> String {
    serde_json::from_str::<Value>(tool_output)
        .ok()
        .and_then(|value| {
            value
                .get("filePath")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| tool_output.trim().to_string())
}

fn extract_bash_stdout(tool_output: &str) -> String {
    serde_json::from_str::<Value>(tool_output)
        .ok()
        .and_then(|value| {
            value
                .get("stdout")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| tool_output.trim().to_string())
}

fn extract_plugin_message(tool_output: &str) -> String {
    serde_json::from_str::<Value>(tool_output)
        .ok()
        .and_then(|value| {
            value
                .get("input")
                .and_then(|input| input.get("message"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| tool_output.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_body(prompt: &str, stream: bool) -> String {
        let mut body = json!({
            "model": DEFAULT_MODEL,
            "max_tokens": 512,
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": prompt}],
            }],
        });
        if stream {
            body["stream"] = json!(true);
        }
        body.to_string()
    }

    async fn post_messages(base_url: &str, body: &str) -> String {
        let address = base_url
            .strip_prefix("http://")
            .expect("mock base url is http");
        let mut socket = tokio::net::TcpStream::connect(address)
            .await
            .expect("connect to mock");
        let request = format!(
            "POST /v1/messages HTTP/1.1\r\nhost: {address}\r\ncontent-type: application/json\r\n\
             content-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        socket
            .write_all(request.as_bytes())
            .await
            .expect("send request");
        let mut response = Vec::new();
        socket
            .read_to_end(&mut response)
            .await
            .expect("read response");
        String::from_utf8(response).expect("utf8 response")
    }

    #[tokio::test]
    async fn content_route_answers_matching_request_and_captures_it() {
        let server = MockAnthropicService::spawn().await.expect("spawn mock");
        server
            .route("Produce the complete product plan", "plan enlatado listo")
            .await;

        let body = request_body("User prompt: Produce the complete product plan now", false);
        let response = post_messages(&server.base_url(), &body).await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.contains("plan enlatado listo"), "{response}");

        let captured = server.captured_requests().await;
        assert_eq!(captured.len(), 1);
        assert_eq!(
            captured[0].scenario,
            format!("{ROUTED_SCENARIO_PREFIX}Produce the complete product plan")
        );
        assert!(!captured[0].stream);
    }

    #[tokio::test]
    async fn content_route_streams_sse_when_the_request_streams() {
        let server = MockAnthropicService::spawn().await.expect("spawn mock");
        server.route("design document", "documento de diseño").await;

        let body = request_body("Deliver your design document", true);
        let response = post_messages(&server.base_url(), &body).await;
        assert!(response.contains("text/event-stream"), "{response}");
        assert!(response.contains("documento de diseño"), "{response}");
        assert!(response.contains("message_stop"), "{response}");
    }

    /// A follow-up body: the assistant's tool_use turn plus the user's
    /// tool_result, as the runtime would send after executing the tool.
    fn tool_result_body(prompt: &str, tool_id: &str, tool_name: &str, result: &str) -> String {
        json!({
            "model": DEFAULT_MODEL,
            "max_tokens": 512,
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": prompt}]},
                {"role": "assistant", "content": [{
                    "type": "tool_use", "id": tool_id, "name": tool_name,
                    "input": {"path": "src/x.css", "content": "x"}
                }]},
                {"role": "user", "content": [{
                    "type": "tool_result", "tool_use_id": tool_id,
                    "content": [{"type": "text", "text": result}]
                }]}
            ],
        })
        .to_string()
    }

    #[tokio::test]
    async fn tool_route_emits_the_tool_call_then_the_final_text() {
        let server = MockAnthropicService::spawn().await.expect("spawn mock");
        server
            .route_tool_use(
                "implementa la tarea",
                "write_file",
                json!({"path": "src/x.css", "content": "x"}),
                "archivo entregado",
            )
            .await;

        // First request (no tool_result yet): a write_file tool_use.
        let first = post_messages(
            &server.base_url(),
            &request_body("implementa la tarea T1", false),
        )
        .await;
        assert!(first.contains("tool_use"), "{first}");
        assert!(first.contains("write_file"), "{first}");
        assert!(first.contains("src/x.css"), "{first}");
        assert!(!first.contains("archivo entregado"), "{first}");

        // Follow-up carrying the tool result: the final text, no more tools.
        let second = post_messages(
            &server.base_url(),
            &tool_result_body(
                "implementa la tarea T1",
                "toolu_route_1",
                "write_file",
                r#"{"filePath":"src/x.css"}"#,
            ),
        )
        .await;
        assert!(second.contains("archivo entregado"), "{second}");
        assert!(second.contains("end_turn"), "{second}");

        // Streaming first request gets the SSE transport.
        let streamed = post_messages(
            &server.base_url(),
            &request_body("implementa la tarea otra vez", true),
        )
        .await;
        assert!(streamed.contains("text/event-stream"), "{streamed}");
        assert!(streamed.contains("write_file"), "{streamed}");
        assert!(streamed.contains("input_json_delta"), "{streamed}");

        let captured = server.captured_requests().await;
        assert_eq!(captured.len(), 3);
        assert!(captured
            .iter()
            .all(|request| request.scenario.starts_with(ROUTED_SCENARIO_PREFIX)));
    }

    #[tokio::test]
    async fn first_registered_route_wins_and_scenarios_still_work() {
        let server = MockAnthropicService::spawn().await.expect("spawn mock");
        server.route("plan", "primera ruta").await;
        server.route("plan detallado", "segunda ruta").await;

        let routed = post_messages(
            &server.base_url(),
            &request_body("dame el plan detallado", false),
        )
        .await;
        assert!(routed.contains("primera ruta"), "{routed}");
        assert!(!routed.contains("segunda ruta"), "{routed}");

        // A request matching no route falls back to the parity scenarios.
        let scenario = post_messages(
            &server.base_url(),
            &request_body(&format!("{SCENARIO_PREFIX}streaming_text"), false),
        )
        .await;
        assert!(scenario.contains("parity harness"), "{scenario}");
        let captured = server.captured_requests().await;
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[1].scenario, "streaming_text");
    }
}
