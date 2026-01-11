//! Anthropic Claude API client implementation.
//!
//! Implements the `LlmClient` trait for calling Anthropic's Messages API.

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::env;
use std::time::Duration;

use crate::agentic::conversation::Message;
use crate::agentic::llm::{LlmClient, LlmResponse};
use crate::agentic::tools::{Tool, ToolCall};
use crate::error::{Error, Result};

/// Anthropic API version header value.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Default base URL for Anthropic API.
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// Cost per million input tokens for Claude Sonnet.
const SONNET_INPUT_COST_PER_M: f64 = 3.0;

/// Cost per million output tokens for Claude Sonnet.
const SONNET_OUTPUT_COST_PER_M: f64 = 15.0;

/// Anthropic API client.
pub struct AnthropicClient {
    client: Client,
    api_key: String,
    base_url: String,
}

impl AnthropicClient {
    /// Create a new Anthropic client.
    ///
    /// Reads API key from `ANTHROPIC_API_KEY` environment variable if not provided.
    pub fn new(api_key: Option<String>, base_url: Option<String>) -> Result<Self> {
        let api_key = api_key
            .or_else(|| env::var("ANTHROPIC_API_KEY").ok())
            .ok_or_else(|| Error::Config("ANTHROPIC_API_KEY not set".to_string()))?;

        let base_url = base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string());

        let client = Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| Error::Api(format!("Failed to create HTTP client: {}", e)))?;

        Ok(Self {
            client,
            api_key,
            base_url,
        })
    }

    /// Convert internal messages to Anthropic format.
    fn convert_messages(&self, messages: &[Message]) -> (Option<String>, Vec<AnthropicMessage>) {
        let mut system_prompt = None;
        let mut anthropic_messages = Vec::new();

        for msg in messages {
            match msg.role {
                crate::agentic::conversation::MessageRole::System => {
                    // Anthropic uses a separate system field, not a message
                    system_prompt = Some(msg.content.clone());
                }
                crate::agentic::conversation::MessageRole::User => {
                    anthropic_messages.push(AnthropicMessage {
                        role: "user".to_string(),
                        content: MessageContent::Text(msg.content.clone()),
                    });
                }
                crate::agentic::conversation::MessageRole::Assistant => {
                    if msg.tool_calls.is_empty() {
                        anthropic_messages.push(AnthropicMessage {
                            role: "assistant".to_string(),
                            content: MessageContent::Text(msg.content.clone()),
                        });
                    } else {
                        // Assistant message with tool calls
                        let mut blocks = Vec::new();
                        if !msg.content.is_empty() {
                            blocks.push(ContentBlock::Text {
                                text: msg.content.clone(),
                            });
                        }
                        for tc in &msg.tool_calls {
                            blocks.push(ContentBlock::ToolUse {
                                id: tc.id.clone(),
                                name: tc.name.clone(),
                                input: tc.arguments.clone(),
                            });
                        }
                        anthropic_messages.push(AnthropicMessage {
                            role: "assistant".to_string(),
                            content: MessageContent::Blocks(blocks),
                        });
                    }
                }
                crate::agentic::conversation::MessageRole::Tool => {
                    // Tool results go in a user message
                    let tool_result = ContentBlock::ToolResult {
                        tool_use_id: msg.tool_call_id.clone().unwrap_or_default(),
                        content: msg.content.clone(),
                    };
                    anthropic_messages.push(AnthropicMessage {
                        role: "user".to_string(),
                        content: MessageContent::Blocks(vec![tool_result]),
                    });
                }
            }
        }

        (system_prompt, anthropic_messages)
    }

    /// Convert internal tools to Anthropic format.
    fn convert_tools(&self, tools: &[Tool]) -> Vec<AnthropicTool> {
        tools
            .iter()
            .map(|t| AnthropicTool {
                name: t.name.clone(),
                description: t.description.clone(),
                input_schema: t.parameters.clone(),
            })
            .collect()
    }

    /// Calculate cost based on token usage.
    fn calculate_cost(&self, input_tokens: u64, output_tokens: u64) -> f64 {
        let input_cost = (input_tokens as f64 / 1_000_000.0) * SONNET_INPUT_COST_PER_M;
        let output_cost = (output_tokens as f64 / 1_000_000.0) * SONNET_OUTPUT_COST_PER_M;
        input_cost + output_cost
    }
}

#[async_trait]
impl LlmClient for AnthropicClient {
    async fn complete(&self, model: &str, messages: &[Message], tools: &[Tool]) -> Result<LlmResponse> {
        let (system, anthropic_messages) = self.convert_messages(messages);
        let anthropic_tools = self.convert_tools(tools);

        let request = AnthropicRequest {
            model: model.to_string(),
            max_tokens: 4096,
            messages: anthropic_messages,
            system,
            tools: if anthropic_tools.is_empty() { None } else { Some(anthropic_tools) },
        };

        // Retry logic with exponential backoff
        let mut last_error = None;
        for attempt in 0..3 {
            if attempt > 0 {
                let delay = Duration::from_millis(1000 * 2u64.pow(attempt as u32));
                tokio::time::sleep(delay).await;
            }

            let response = self
                .client
                .post(format!("{}/v1/messages", self.base_url))
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .header("content-type", "application/json")
                .json(&request)
                .send()
                .await;

            match response {
                Ok(resp) => {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();

                    if status.is_success() {
                        let api_response: AnthropicResponse = serde_json::from_str(&body)
                            .map_err(|e| Error::Api(format!("Failed to parse response: {} - {}", e, body)))?;

                        return Ok(self.convert_response(api_response));
                    } else if status.as_u16() == 429 {
                        // Rate limited, retry
                        last_error = Some(Error::Api(format!("Rate limited (attempt {}): {}", attempt + 1, body)));
                        continue;
                    } else if status.is_server_error() {
                        // Server error, retry
                        last_error = Some(Error::Api(format!(
                            "Server error {} (attempt {}): {}",
                            status,
                            attempt + 1,
                            body
                        )));
                        continue;
                    } else {
                        // Client error, don't retry
                        return Err(Error::Api(format!("API error {}: {}", status, body)));
                    }
                }
                Err(e) => {
                    last_error = Some(Error::Api(format!("Request failed (attempt {}): {}", attempt + 1, e)));
                    continue;
                }
            }
        }

        Err(last_error.unwrap_or_else(|| Error::Api("All retry attempts failed".to_string())))
    }
}

impl AnthropicClient {
    /// Convert Anthropic API response to internal format.
    fn convert_response(&self, response: AnthropicResponse) -> LlmResponse {
        let mut content = String::new();
        let mut tool_calls = Vec::new();

        for block in response.content {
            match block {
                ContentBlock::Text { text } => {
                    content.push_str(&text);
                }
                ContentBlock::ToolUse { id, name, input } => {
                    tool_calls.push(ToolCall {
                        id,
                        name,
                        arguments: input,
                    });
                }
                ContentBlock::ToolResult { .. } => {
                    // Tool results shouldn't appear in responses
                }
            }
        }

        let input_tokens = response.usage.input_tokens;
        let output_tokens = response.usage.output_tokens;

        LlmResponse {
            content,
            tool_calls,
            stop_reason: Some(response.stop_reason),
            tokens_used: input_tokens + output_tokens,
            cost: self.calculate_cost(input_tokens, output_tokens),
        }
    }
}

// Anthropic API types

#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<AnthropicTool>>,
}

#[derive(Debug, Serialize)]
struct AnthropicMessage {
    role: String,
    content: MessageContent,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse { id: String, name: String, input: Value },
    #[serde(rename = "tool_result")]
    ToolResult { tool_use_id: String, content: String },
}

#[derive(Debug, Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: Value,
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    content: Vec<ContentBlock>,
    stop_reason: String,
    usage: Usage,
}

#[derive(Debug, Deserialize)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agentic::conversation::MessageRole;

    #[test]
    fn test_convert_messages_system() {
        let client = AnthropicClient {
            client: Client::new(),
            api_key: "test".to_string(),
            base_url: "http://test".to_string(),
        };

        let messages = vec![Message {
            role: MessageRole::System,
            content: "You are helpful".to_string(),
            tool_calls: vec![],
            tool_call_id: None,
        }];

        let (system, msgs) = client.convert_messages(&messages);
        assert_eq!(system, Some("You are helpful".to_string()));
        assert!(msgs.is_empty());
    }

    #[test]
    fn test_convert_messages_user_assistant() {
        let client = AnthropicClient {
            client: Client::new(),
            api_key: "test".to_string(),
            base_url: "http://test".to_string(),
        };

        let messages = vec![
            Message {
                role: MessageRole::User,
                content: "Hello".to_string(),
                tool_calls: vec![],
                tool_call_id: None,
            },
            Message {
                role: MessageRole::Assistant,
                content: "Hi there".to_string(),
                tool_calls: vec![],
                tool_call_id: None,
            },
        ];

        let (system, msgs) = client.convert_messages(&messages);
        assert!(system.is_none());
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[1].role, "assistant");
    }

    #[test]
    fn test_convert_messages_with_tool_calls() {
        let client = AnthropicClient {
            client: Client::new(),
            api_key: "test".to_string(),
            base_url: "http://test".to_string(),
        };

        let messages = vec![Message {
            role: MessageRole::Assistant,
            content: "Let me read that file".to_string(),
            tool_calls: vec![ToolCall {
                id: "call_1".to_string(),
                name: "read_file".to_string(),
                arguments: serde_json::json!({"path": "test.txt"}),
            }],
            tool_call_id: None,
        }];

        let (_, msgs) = client.convert_messages(&messages);
        assert_eq!(msgs.len(), 1);

        // Verify it's a blocks content
        match &msgs[0].content {
            MessageContent::Blocks(blocks) => {
                assert_eq!(blocks.len(), 2); // text + tool_use
            }
            _ => panic!("Expected blocks content"),
        }
    }

    #[test]
    fn test_convert_messages_tool_result() {
        let client = AnthropicClient {
            client: Client::new(),
            api_key: "test".to_string(),
            base_url: "http://test".to_string(),
        };

        let messages = vec![Message {
            role: MessageRole::Tool,
            content: "file contents".to_string(),
            tool_calls: vec![],
            tool_call_id: Some("call_1".to_string()),
        }];

        let (_, msgs) = client.convert_messages(&messages);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "user"); // Tool results go in user messages
    }

    #[test]
    fn test_convert_tools() {
        let client = AnthropicClient {
            client: Client::new(),
            api_key: "test".to_string(),
            base_url: "http://test".to_string(),
        };

        let tools = vec![Tool {
            name: "read_file".to_string(),
            description: "Read a file".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }),
        }];

        let anthropic_tools = client.convert_tools(&tools);
        assert_eq!(anthropic_tools.len(), 1);
        assert_eq!(anthropic_tools[0].name, "read_file");
    }

    #[test]
    fn test_convert_all_default_tools() {
        use crate::agentic::tools::ToolExecutor;
        use std::path::PathBuf;

        let client = AnthropicClient {
            client: Client::new(),
            api_key: "test".to_string(),
            base_url: "http://test".to_string(),
        };

        let executor = ToolExecutor::new(PathBuf::from("/tmp"));
        let tools = executor.available_tools();
        let anthropic_tools = client.convert_tools(tools);

        // Should have 14 tools including web_search
        assert_eq!(anthropic_tools.len(), 14);

        // Verify web_search is present
        let names: Vec<&str> = anthropic_tools.iter().map(|t| t.name.as_str()).collect();
        assert!(
            names.contains(&"web_search"),
            "web_search not found in tools: {:?}",
            names
        );
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"run_command"));
    }

    #[test]
    fn test_calculate_cost() {
        let client = AnthropicClient {
            client: Client::new(),
            api_key: "test".to_string(),
            base_url: "http://test".to_string(),
        };

        // 1M input tokens = $3, 1M output tokens = $15
        let cost = client.calculate_cost(1_000_000, 1_000_000);
        assert!((cost - 18.0).abs() < 0.001);

        // 1000 input + 1000 output
        let cost = client.calculate_cost(1000, 1000);
        assert!((cost - 0.018).abs() < 0.001);
    }

    #[test]
    fn test_convert_response() {
        let client = AnthropicClient {
            client: Client::new(),
            api_key: "test".to_string(),
            base_url: "http://test".to_string(),
        };

        let response = AnthropicResponse {
            content: vec![ContentBlock::Text {
                text: "Hello!".to_string(),
            }],
            stop_reason: "end_turn".to_string(),
            usage: Usage {
                input_tokens: 100,
                output_tokens: 50,
            },
        };

        let llm_response = client.convert_response(response);
        assert_eq!(llm_response.content, "Hello!");
        assert!(llm_response.tool_calls.is_empty());
        assert_eq!(llm_response.stop_reason, Some("end_turn".to_string()));
        assert_eq!(llm_response.tokens_used, 150);
    }

    #[test]
    fn test_convert_response_with_tool_use() {
        let client = AnthropicClient {
            client: Client::new(),
            api_key: "test".to_string(),
            base_url: "http://test".to_string(),
        };

        let response = AnthropicResponse {
            content: vec![
                ContentBlock::Text {
                    text: "Let me read that".to_string(),
                },
                ContentBlock::ToolUse {
                    id: "call_123".to_string(),
                    name: "read_file".to_string(),
                    input: serde_json::json!({"path": "test.txt"}),
                },
            ],
            stop_reason: "tool_use".to_string(),
            usage: Usage {
                input_tokens: 100,
                output_tokens: 50,
            },
        };

        let llm_response = client.convert_response(response);
        assert_eq!(llm_response.content, "Let me read that");
        assert_eq!(llm_response.tool_calls.len(), 1);
        assert_eq!(llm_response.tool_calls[0].id, "call_123");
        assert_eq!(llm_response.tool_calls[0].name, "read_file");
        assert_eq!(llm_response.stop_reason, Some("tool_use".to_string()));
    }
}
