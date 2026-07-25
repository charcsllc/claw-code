use std::collections::BTreeMap;

use runtime::{pricing_for_model, TokenUsage, UsageCostEstimate};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MessageRequest {
    pub model: String,
    pub max_tokens: u32,
    pub messages: Vec<InputMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stream: bool,
    /// OpenAI-compatible tuning parameters. Optional — omitted from payload when None.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
    /// Reasoning effort level for OpenAI-compatible reasoning models (e.g. `o4-mini`).
    /// Accepted values: `"low"`, `"medium"`, `"high"`. Omitted when `None`.
    /// Silently ignored by backends that do not support it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Provider-specific OpenAI-compatible request body parameters. These are
    /// copied into the final JSON payload after core fields are populated so
    /// users can opt into gateway features such as `web_search_options`,
    /// `parallel_tool_calls`, or custom local-server switches without waiting
    /// for first-class typed fields. Core protocol keys are protected and cannot
    /// be overridden through this map.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra_body: BTreeMap<String, Value>,
}

impl MessageRequest {
    #[must_use]
    pub fn with_streaming(mut self) -> Self {
        self.stream = true;
        self
    }
}

/// A base64-encoded image attached to a user message.
///
/// This is the transport-agnostic carrier used by callers (runtime adapter,
/// sub-agent tool) to hand images to the API layer. Each provider protocol
/// renders it into its own wire shape:
/// - Anthropic: `{"type":"image","source":{"type":"base64","media_type":…,"data":…}}`
/// - OpenAI-compat: `{"type":"image_url","image_url":{"url":"data:<media_type>;base64,<data>"}}`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageAttachment {
    /// MIME type of the encoded image, e.g. `image/png` or `image/jpeg`.
    pub media_type: String,
    /// Raw base64 payload (no `data:` URL prefix).
    pub base64_data: String,
}

/// Wire-format source of an [`InputContentBlock::Image`], matching the
/// Anthropic Messages API shape: `{"type":"base64","media_type":…,"data":…}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageSource {
    /// Source encoding discriminator; always `"base64"` for attachments.
    #[serde(rename = "type")]
    pub source_type: String,
    pub media_type: String,
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputMessage {
    pub role: String,
    pub content: Vec<InputContentBlock>,
}

impl InputMessage {
    #[must_use]
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: vec![InputContentBlock::Text { text: text.into() }],
        }
    }

    /// Builds a user message carrying `images` followed by a trailing text
    /// block. Image blocks are placed before the text block, mirroring the
    /// ordering the Anthropic vision docs recommend for image+question turns.
    #[must_use]
    pub fn user_text_with_images(text: impl Into<String>, images: Vec<ImageAttachment>) -> Self {
        let mut content: Vec<InputContentBlock> =
            images.into_iter().map(InputContentBlock::from).collect();
        content.push(InputContentBlock::Text { text: text.into() });
        Self {
            role: "user".to_string(),
            content,
        }
    }

    #[must_use]
    pub fn user_tool_result(
        tool_use_id: impl Into<String>,
        content: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self {
            role: "user".to_string(),
            content: vec![InputContentBlock::ToolResult {
                tool_use_id: tool_use_id.into(),
                content: vec![ToolResultContentBlock::Text {
                    text: content.into(),
                }],
                is_error,
            }],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputContentBlock {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: Vec<ToolResultContentBlock>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
    /// A base64 image content block. The serde representation matches the
    /// Anthropic Messages wire format
    /// (`{"type":"image","source":{"type":"base64",…}}`); the OpenAI-compat
    /// translator rewrites it into an `image_url` data-URL part.
    Image {
        source: ImageSource,
    },
}

impl From<ImageAttachment> for InputContentBlock {
    fn from(image: ImageAttachment) -> Self {
        Self::Image {
            source: ImageSource {
                source_type: "base64".to_string(),
                media_type: image.media_type,
                data: image.base64_data,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolResultContentBlock {
    Text { text: String },
    Json { value: Value },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolChoice {
    Auto,
    Any,
    Tool { name: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageResponse {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub role: String,
    pub content: Vec<OutputContentBlock>,
    pub model: String,
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub stop_sequence: Option<String>,
    #[serde(default)]
    pub usage: Usage,
    #[serde(default)]
    pub request_id: Option<String>,
}

impl MessageResponse {
    #[must_use]
    pub fn total_tokens(&self) -> u32 {
        self.usage.total_tokens()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    Thinking {
        #[serde(default)]
        thinking: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    RedactedThinking {
        data: Value,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u32,
    #[serde(default)]
    pub cache_creation_input_tokens: u32,
    #[serde(default)]
    pub cache_read_input_tokens: u32,
    #[serde(default)]
    pub output_tokens: u32,
}

impl Usage {
    #[must_use]
    pub const fn total_tokens(&self) -> u32 {
        self.input_tokens
            + self.output_tokens
            + self.cache_creation_input_tokens
            + self.cache_read_input_tokens
    }

    #[must_use]
    pub const fn token_usage(&self) -> TokenUsage {
        TokenUsage {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_creation_input_tokens: self.cache_creation_input_tokens,
            cache_read_input_tokens: self.cache_read_input_tokens,
        }
    }

    #[must_use]
    pub fn estimated_cost_usd(&self, model: &str) -> UsageCostEstimate {
        let usage = self.token_usage();
        pricing_for_model(model).map_or_else(
            || usage.estimate_cost_usd(),
            |pricing| usage.estimate_cost_usd_with_pricing(pricing),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageStartEvent {
    pub message: MessageResponse,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageDeltaEvent {
    pub delta: MessageDelta,
    #[serde(default)]
    pub usage: Usage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageDelta {
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub stop_sequence: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentBlockStartEvent {
    pub index: u32,
    pub content_block: OutputContentBlock,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentBlockDeltaEvent {
    pub index: u32,
    pub delta: ContentBlockDelta,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlockDelta {
    TextDelta { text: String },
    InputJsonDelta { partial_json: String },
    ThinkingDelta { thinking: String },
    SignatureDelta { signature: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentBlockStopEvent {
    pub index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageStopEvent {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    MessageStart(MessageStartEvent),
    MessageDelta(MessageDeltaEvent),
    ContentBlockStart(ContentBlockStartEvent),
    ContentBlockDelta(ContentBlockDeltaEvent),
    ContentBlockStop(ContentBlockStopEvent),
    MessageStop(MessageStopEvent),
}

#[cfg(test)]
mod tests {
    use runtime::format_usd;
    use serde_json::json;

    use super::{ImageAttachment, InputContentBlock, InputMessage, MessageResponse, Usage};

    #[test]
    fn usage_total_tokens_includes_cache_tokens() {
        let usage = Usage {
            input_tokens: 10,
            cache_creation_input_tokens: 2,
            cache_read_input_tokens: 3,
            output_tokens: 4,
        };

        assert_eq!(usage.total_tokens(), 19);
        assert_eq!(usage.token_usage().total_tokens(), 19);
    }

    #[test]
    fn message_response_estimates_cost_from_model_usage() {
        let response = MessageResponse {
            id: "msg_cost".to_string(),
            kind: "message".to_string(),
            role: "assistant".to_string(),
            content: Vec::new(),
            model: "claude-sonnet-4-20250514".to_string(),
            stop_reason: Some("end_turn".to_string()),
            stop_sequence: None,
            usage: Usage {
                input_tokens: 1_000_000,
                cache_creation_input_tokens: 100_000,
                cache_read_input_tokens: 200_000,
                output_tokens: 500_000,
            },
            request_id: None,
        };

        let cost = response.usage.estimated_cost_usd(&response.model);
        assert_eq!(format_usd(cost.total_cost_usd()), "$54.6750");
        assert_eq!(response.total_tokens(), 1_800_000);
    }

    #[test]
    fn user_text_with_images_serializes_anthropic_image_blocks_before_text() {
        // given
        let message = InputMessage::user_text_with_images(
            "what is in this screenshot?",
            vec![
                ImageAttachment {
                    media_type: "image/png".to_string(),
                    base64_data: "aGVsbG8=".to_string(),
                },
                ImageAttachment {
                    media_type: "image/jpeg".to_string(),
                    base64_data: "d29ybGQ=".to_string(),
                },
            ],
        );

        // when: the Anthropic protocol serializes InputMessage via serde
        let serialized = serde_json::to_value(&message).unwrap();

        // then
        assert_eq!(
            serialized,
            json!({
                "role": "user",
                "content": [
                    {
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": "image/png",
                            "data": "aGVsbG8="
                        }
                    },
                    {
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": "image/jpeg",
                            "data": "d29ybGQ="
                        }
                    },
                    { "type": "text", "text": "what is in this screenshot?" }
                ]
            })
        );
    }

    #[test]
    fn image_input_content_block_round_trips_through_serde() {
        let block = InputContentBlock::from(ImageAttachment {
            media_type: "image/webp".to_string(),
            base64_data: "Zm9v".to_string(),
        });

        let value = serde_json::to_value(&block).unwrap();
        let restored: InputContentBlock = serde_json::from_value(value).unwrap();

        assert_eq!(restored, block);
    }

    #[test]
    fn user_text_with_images_without_images_matches_user_text() {
        assert_eq!(
            InputMessage::user_text_with_images("hola", Vec::new()),
            InputMessage::user_text("hola")
        );
    }

    #[test]
    fn input_content_block_thinking_serializes_with_snake_case_type() {
        // given
        let block = InputContentBlock::Thinking {
            thinking: "pondering".to_string(),
            signature: Some("sig_123".to_string()),
        };

        // when
        let serialized = serde_json::to_value(&block).unwrap();
        let deserialized: InputContentBlock = serde_json::from_value(json!({
            "type": "thinking",
            "thinking": "pondering",
            "signature": "sig_123"
        }))
        .unwrap();

        // then
        assert_eq!(
            serialized,
            json!({
                "type": "thinking",
                "thinking": "pondering",
                "signature": "sig_123"
            })
        );
        assert_eq!(deserialized, block);
    }
}
