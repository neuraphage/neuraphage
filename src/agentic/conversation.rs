//! Conversation storage for the agentic loop.
//!
//! Conversations are stored as JSON files for persistence and debugging.
//! Includes execution state for crash recovery.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::agentic::tools::ToolCall;
use crate::error::Result;

/// Checkpoint marker for crash recovery.
///
/// Indicates which phase of execution the loop was in when it last saved state.
/// This allows recovery logic to determine how to safely resume.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum CheckpointMarker {
    /// About to send LLM request.
    RequestSent { request_id: String },
    /// Response received, about to process.
    ResponseReceived { response_hash: String },
    /// Tool execution started.
    ToolStarted {
        tool_name: String,
        tool_use_id: String,
        params_hash: String,
    },
    /// Tool execution completed.
    ToolCompleted {
        tool_name: String,
        tool_use_id: String,
        result_hash: String,
    },
    /// Waiting for user input.
    WaitingForUser { prompt_hash: String },
}

/// Execution state stored with conversation for recovery.
///
/// This struct captures the runtime state of the agentic loop so it can be
/// restored after a crash. It's serialized alongside the conversation messages.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConversationExecutionState {
    /// Current iteration count.
    pub iteration: u32,
    /// Total tokens used.
    pub tokens_used: u64,
    /// Total cost incurred.
    pub cost: f64,
    /// Last checkpoint marker.
    pub checkpoint: Option<CheckpointMarker>,
    /// When state was last saved.
    pub saved_at: Option<DateTime<Utc>>,
}

/// Role of a message in the conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    /// System message (instructions, context).
    System,
    /// User message.
    User,
    /// Assistant (model) message.
    Assistant,
    /// Tool result.
    Tool,
}

/// A single message in the conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// Role of the message sender.
    pub role: MessageRole,
    /// Content of the message.
    pub content: String,
    /// Tool calls made in this message (for assistant messages).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Tool call ID this message is responding to (for tool messages).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// A conversation history with execution metadata.
#[derive(Debug, Serialize, Deserialize)]
pub struct Conversation {
    /// Path to the conversation file.
    #[serde(skip)]
    path: PathBuf,
    /// When the conversation was created.
    pub created_at: DateTime<Utc>,
    /// When the conversation was last updated.
    pub updated_at: DateTime<Utc>,
    /// The messages in the conversation.
    pub messages: Vec<Message>,
    /// Execution state for recovery (backward compatible with old format).
    #[serde(default)]
    pub execution_state: ConversationExecutionState,
}

impl Conversation {
    /// Create a new conversation.
    pub fn new(path: &Path) -> Result<Self> {
        let now = Utc::now();
        Ok(Self {
            path: path.to_path_buf(),
            created_at: now,
            updated_at: now,
            messages: Vec::new(),
            execution_state: ConversationExecutionState::default(),
        })
    }

    /// Load an existing conversation from disk.
    pub fn load(path: &Path) -> Result<Self> {
        let conversation_file = Self::conversation_file(path);

        if !conversation_file.exists() {
            return Self::new(path);
        }

        let content = fs::read_to_string(&conversation_file)?;
        let mut conversation: Self = serde_json::from_str(&content)?;
        conversation.path = path.to_path_buf();
        Ok(conversation)
    }

    /// Save the conversation to disk.
    pub fn save(&self) -> Result<()> {
        let conversation_file = Self::conversation_file(&self.path);

        // Ensure directory exists
        if let Some(parent) = conversation_file.parent() {
            fs::create_dir_all(parent)?;
        }

        let content = serde_json::to_string_pretty(self)?;
        fs::write(&conversation_file, content)?;
        Ok(())
    }

    /// Add a message to the conversation.
    pub fn add_message(&mut self, message: Message) -> Result<()> {
        self.messages.push(message);
        self.updated_at = Utc::now();
        self.save()
    }

    /// Get the messages in the conversation.
    pub fn messages(&self) -> impl Iterator<Item = &Message> {
        self.messages.iter()
    }

    /// Get the last message.
    pub fn last_message(&self) -> Option<&Message> {
        self.messages.last()
    }

    /// Get the number of messages.
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// Check if the conversation is empty.
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Clear all messages.
    pub fn clear(&mut self) -> Result<()> {
        self.messages.clear();
        self.updated_at = Utc::now();
        self.save()
    }

    /// Get conversation file path.
    fn conversation_file(base_path: &Path) -> PathBuf {
        base_path.join("conversation.json")
    }

    /// Get the last N messages (for context window management).
    pub fn last_n_messages(&self, n: usize) -> Vec<&Message> {
        let start = self.messages.len().saturating_sub(n);
        self.messages[start..].iter().collect()
    }

    /// Estimate token count for all messages.
    pub fn estimate_tokens(&self) -> usize {
        // Rough estimate: 4 characters per token
        self.messages.iter().map(|m| m.content.len() / 4).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_conversation_new() {
        let temp = TempDir::new().unwrap();
        let conv = Conversation::new(temp.path()).unwrap();
        assert!(conv.is_empty());
    }

    #[test]
    fn test_conversation_add_message() {
        let temp = TempDir::new().unwrap();
        let mut conv = Conversation::new(temp.path()).unwrap();

        conv.add_message(Message {
            role: MessageRole::User,
            content: "Hello".to_string(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        })
        .unwrap();

        assert_eq!(conv.len(), 1);
        assert_eq!(conv.last_message().unwrap().content, "Hello");
    }

    #[test]
    fn test_conversation_save_load() {
        let temp = TempDir::new().unwrap();
        let mut conv = Conversation::new(temp.path()).unwrap();

        conv.add_message(Message {
            role: MessageRole::User,
            content: "Test message".to_string(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        })
        .unwrap();

        // Load should find the saved conversation
        let loaded = Conversation::load(temp.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded.messages[0].content, "Test message");
    }

    #[test]
    fn test_conversation_with_tool_calls() {
        let temp = TempDir::new().unwrap();
        let mut conv = Conversation::new(temp.path()).unwrap();

        let tool_call = ToolCall {
            id: "call_123".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({"path": "/test.txt"}),
        };

        conv.add_message(Message {
            role: MessageRole::Assistant,
            content: "Let me read that file.".to_string(),
            tool_calls: vec![tool_call],
            tool_call_id: None,
        })
        .unwrap();

        conv.add_message(Message {
            role: MessageRole::Tool,
            content: "File contents here".to_string(),
            tool_calls: Vec::new(),
            tool_call_id: Some("call_123".to_string()),
        })
        .unwrap();

        assert_eq!(conv.len(), 2);

        // Verify serialization roundtrip
        let loaded = Conversation::load(temp.path()).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.messages[0].tool_calls.len(), 1);
        assert_eq!(loaded.messages[1].tool_call_id, Some("call_123".to_string()));
    }

    #[test]
    fn test_last_n_messages() {
        let temp = TempDir::new().unwrap();
        let mut conv = Conversation::new(temp.path()).unwrap();

        for i in 0..10 {
            conv.add_message(Message {
                role: MessageRole::User,
                content: format!("Message {}", i),
                tool_calls: Vec::new(),
                tool_call_id: None,
            })
            .unwrap();
        }

        let last_3 = conv.last_n_messages(3);
        assert_eq!(last_3.len(), 3);
        assert_eq!(last_3[0].content, "Message 7");
        assert_eq!(last_3[2].content, "Message 9");
    }

    #[test]
    fn test_estimate_tokens() {
        let temp = TempDir::new().unwrap();
        let mut conv = Conversation::new(temp.path()).unwrap();

        conv.add_message(Message {
            role: MessageRole::User,
            content: "Hello World!".to_string(), // 12 chars = ~3 tokens
            tool_calls: Vec::new(),
            tool_call_id: None,
        })
        .unwrap();

        let estimate = conv.estimate_tokens();
        assert!((2..=4).contains(&estimate));
    }

    #[test]
    fn test_conversation_roundtrip_with_execution_state() {
        let temp = TempDir::new().unwrap();
        let mut conv = Conversation::new(temp.path()).unwrap();

        // Add a message
        conv.add_message(Message {
            role: MessageRole::User,
            content: "Test".to_string(),
            tool_calls: vec![],
            tool_call_id: None,
        })
        .unwrap();

        // Set execution state
        conv.execution_state = ConversationExecutionState {
            iteration: 5,
            tokens_used: 12345,
            cost: 0.567,
            checkpoint: Some(CheckpointMarker::ToolStarted {
                tool_name: "read_file".to_string(),
                tool_use_id: "tu_123".to_string(),
                params_hash: "abc123".to_string(),
            }),
            saved_at: Some(Utc::now()),
        };
        conv.save().unwrap();

        // Load and verify
        let loaded = Conversation::load(temp.path()).unwrap();
        assert_eq!(loaded.execution_state.iteration, 5);
        assert_eq!(loaded.execution_state.tokens_used, 12345);
        assert!((loaded.execution_state.cost - 0.567).abs() < 0.001);
        assert!(matches!(
            loaded.execution_state.checkpoint,
            Some(CheckpointMarker::ToolStarted { .. })
        ));
    }

    #[test]
    fn test_conversation_load_without_execution_state() {
        // Test backward compatibility with old format
        let temp = TempDir::new().unwrap();
        let conversation_file = temp.path().join("conversation.json");

        // Write old-format JSON manually (without execution_state)
        let old_json = r#"{
            "created_at": "2026-01-11T00:00:00Z",
            "updated_at": "2026-01-11T00:00:00Z",
            "messages": []
        }"#;
        fs::write(&conversation_file, old_json).unwrap();

        let loaded = Conversation::load(temp.path()).unwrap();
        assert_eq!(loaded.execution_state.iteration, 0);
        assert_eq!(loaded.execution_state.tokens_used, 0);
        assert!((loaded.execution_state.cost - 0.0).abs() < 0.001);
        assert!(loaded.execution_state.checkpoint.is_none());
    }

    #[test]
    fn test_checkpoint_marker_variants() {
        let temp = TempDir::new().unwrap();

        // Test each checkpoint marker variant
        let markers = vec![
            CheckpointMarker::RequestSent {
                request_id: "req_123".to_string(),
            },
            CheckpointMarker::ResponseReceived {
                response_hash: "hash_abc".to_string(),
            },
            CheckpointMarker::ToolStarted {
                tool_name: "bash".to_string(),
                tool_use_id: "tu_456".to_string(),
                params_hash: "params_xyz".to_string(),
            },
            CheckpointMarker::ToolCompleted {
                tool_name: "edit".to_string(),
                tool_use_id: "tu_789".to_string(),
                result_hash: "result_abc".to_string(),
            },
            CheckpointMarker::WaitingForUser {
                prompt_hash: "prompt_123".to_string(),
            },
        ];

        for (i, marker) in markers.into_iter().enumerate() {
            let path = temp.path().join(format!("conv_{i}"));
            fs::create_dir_all(&path).unwrap();

            let mut conv = Conversation::new(&path).unwrap();
            conv.execution_state.checkpoint = Some(marker.clone());
            conv.save().unwrap();

            let loaded = Conversation::load(&path).unwrap();
            assert_eq!(loaded.execution_state.checkpoint, Some(marker));
        }
    }
}
