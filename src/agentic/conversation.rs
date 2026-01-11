//! Conversation storage for the agentic loop.
//!
//! Conversations are stored as JSON files for persistence and debugging.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::agentic::tools::ToolCall;
use crate::error::Result;

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

/// A conversation history.
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
}
