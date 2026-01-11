//! LLM client abstraction for the agentic loop.
//!
//! Provides traits and types for interacting with language models.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::agentic::conversation::Message;
use crate::agentic::tools::{Tool, ToolCall};
use crate::error::Result;

/// A streaming chunk from the LLM.
#[derive(Debug, Clone)]
pub enum StreamChunk {
    /// Text content delta.
    TextDelta(String),
    /// Tool use started (id and name known).
    ToolUseStart { id: String, name: String },
    /// Tool use input JSON fragment.
    ToolUseDelta { id: String, json_delta: String },
    /// Tool use complete (full input available).
    ToolUseEnd { id: String },
    /// Message complete - final usage stats.
    MessageDone {
        stop_reason: String,
        input_tokens: u64,
        output_tokens: u64,
    },
    /// Error during streaming.
    Error(String),
}

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

    /// Stream a conversation completion, sending chunks as they arrive.
    ///
    /// Default implementation falls back to non-streaming `complete()`.
    async fn stream(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[Tool],
        chunk_tx: mpsc::Sender<StreamChunk>,
    ) -> Result<LlmResponse> {
        // Default: fall back to non-streaming
        let response = self.complete(model, messages, tools).await?;

        // Send the full content as a single delta for compatibility
        if !response.content.is_empty() {
            let _ = chunk_tx.send(StreamChunk::TextDelta(response.content.clone())).await;
        }

        // Send completion
        let _ = chunk_tx
            .send(StreamChunk::MessageDone {
                stop_reason: response.stop_reason.clone().unwrap_or_default(),
                input_tokens: 0,
                output_tokens: response.tokens_used,
            })
            .await;

        Ok(response)
    }
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

    #[tokio::test]
    async fn test_default_stream_impl() {
        // Test that the default stream() implementation works correctly
        let client = MockLlmClient::new(vec![LlmResponse::text("Hello, world!")]);
        let (tx, mut rx) = mpsc::channel::<StreamChunk>(10);

        let response = client.stream("test", &[], &[], tx).await.unwrap();

        // Response should contain the content
        assert_eq!(response.content, "Hello, world!");

        // Channel should have received TextDelta and MessageDone
        let mut received_text = String::new();
        let mut received_done = false;

        while let Ok(chunk) = rx.try_recv() {
            match chunk {
                StreamChunk::TextDelta(text) => received_text.push_str(&text),
                StreamChunk::MessageDone { .. } => received_done = true,
                _ => {}
            }
        }

        assert_eq!(received_text, "Hello, world!");
        assert!(received_done);
    }

    #[tokio::test]
    async fn test_default_stream_empty_response() {
        // Test that empty content doesn't send TextDelta
        let client = MockLlmClient::new(vec![LlmResponse::text("")]);
        let (tx, mut rx) = mpsc::channel::<StreamChunk>(10);

        let response = client.stream("test", &[], &[], tx).await.unwrap();
        assert_eq!(response.content, "");

        // Should only receive MessageDone (no TextDelta for empty content)
        let mut chunks = Vec::new();
        while let Ok(chunk) = rx.try_recv() {
            chunks.push(chunk);
        }

        assert_eq!(chunks.len(), 1);
        assert!(matches!(chunks[0], StreamChunk::MessageDone { .. }));
    }
}
