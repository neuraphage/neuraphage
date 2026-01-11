//! LLM client abstraction for the agentic loop.
//!
//! Provides traits and types for interacting with language models.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::agentic::conversation::Message;
use crate::agentic::tools::{Tool, ToolCall};
use crate::error::Result;

/// Configuration for the LLM client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    /// API key for authentication.
    pub api_key: String,
    /// Base URL for the API.
    pub base_url: String,
    /// Default model to use.
    pub default_model: String,
    /// Maximum tokens to generate.
    pub max_tokens: u32,
    /// Temperature for generation.
    pub temperature: f32,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            base_url: "https://api.anthropic.com".to_string(),
            default_model: "claude-sonnet-4-20250514".to_string(),
            max_tokens: 4096,
            temperature: 0.7,
        }
    }
}

/// Response from the LLM.
#[derive(Debug, Clone)]
pub struct LlmResponse {
    /// Text content of the response.
    pub content: String,
    /// Tool calls made by the model.
    pub tool_calls: Vec<ToolCall>,
    /// Reason the response stopped.
    pub stop_reason: Option<String>,
    /// Number of tokens used (input + output).
    pub tokens_used: u64,
    /// Estimated cost in USD.
    pub cost: f64,
}

impl LlmResponse {
    /// Create a new response with just content.
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            tool_calls: Vec::new(),
            stop_reason: Some("end_turn".to_string()),
            tokens_used: 0,
            cost: 0.0,
        }
    }

    /// Create a response with tool calls.
    pub fn with_tool_calls(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            content: content.into(),
            tool_calls,
            stop_reason: Some("tool_use".to_string()),
            tokens_used: 0,
            cost: 0.0,
        }
    }
}

/// Trait for LLM clients.
#[async_trait]
pub trait LlmClient: Send + Sync {
    /// Complete a conversation with the given messages and tools.
    async fn complete(&self, model: &str, messages: &[Message], tools: &[Tool]) -> Result<LlmResponse>;
}

/// Mock LLM client for testing.
#[cfg(test)]
pub struct MockLlmClient {
    /// Responses to return in order.
    pub responses: std::sync::Mutex<Vec<LlmResponse>>,
}

#[cfg(test)]
impl MockLlmClient {
    /// Create a new mock client with predefined responses.
    pub fn new(responses: Vec<LlmResponse>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses),
        }
    }

    /// Create a mock that always returns the same response.
    pub fn always(response: LlmResponse) -> Self {
        // Create a large number of copies for repeated calls
        Self::new(vec![response; 100])
    }
}

#[cfg(test)]
#[async_trait]
impl LlmClient for MockLlmClient {
    async fn complete(&self, _model: &str, _messages: &[Message], _tools: &[Tool]) -> Result<LlmResponse> {
        let mut responses = self.responses.lock().unwrap();
        if responses.is_empty() {
            Ok(LlmResponse::text("No more mock responses"))
        } else {
            Ok(responses.remove(0))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_llm_config_default() {
        let config = LlmConfig::default();
        assert_eq!(config.base_url, "https://api.anthropic.com");
        assert!(config.default_model.contains("claude"));
        assert_eq!(config.max_tokens, 4096);
    }

    #[test]
    fn test_llm_response_text() {
        let response = LlmResponse::text("Hello");
        assert_eq!(response.content, "Hello");
        assert!(response.tool_calls.is_empty());
        assert_eq!(response.stop_reason, Some("end_turn".to_string()));
    }

    #[test]
    fn test_llm_response_with_tool_calls() {
        let tool_calls = vec![ToolCall {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({"path": "test.txt"}),
        }];

        let response = LlmResponse::with_tool_calls("Reading file", tool_calls);
        assert_eq!(response.content, "Reading file");
        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.stop_reason, Some("tool_use".to_string()));
    }

    #[tokio::test]
    async fn test_mock_llm_client() {
        let responses = vec![LlmResponse::text("First"), LlmResponse::text("Second")];

        let client = MockLlmClient::new(responses);

        let response1 = client.complete("test", &[], &[]).await.unwrap();
        assert_eq!(response1.content, "First");

        let response2 = client.complete("test", &[], &[]).await.unwrap();
        assert_eq!(response2.content, "Second");

        let response3 = client.complete("test", &[], &[]).await.unwrap();
        assert_eq!(response3.content, "No more mock responses");
    }

    #[tokio::test]
    async fn test_mock_llm_client_always() {
        let client = MockLlmClient::always(LlmResponse::text("Always same"));

        for _ in 0..5 {
            let response = client.complete("test", &[], &[]).await.unwrap();
            assert_eq!(response.content, "Always same");
        }
    }
}
