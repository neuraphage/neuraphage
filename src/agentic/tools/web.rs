//! Web tool: fetch URL content.

use serde_json::Value;

use super::{Tool, ToolResult};
use crate::error::Result;

/// WebFetch tool - fetches content from URLs.
pub struct WebFetchTool;

impl WebFetchTool {
    pub fn definition() -> Tool {
        Tool {
            name: "web_fetch".to_string(),
            description: "Fetch content from a URL. Returns the page content as text.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The URL to fetch"
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "description": "Timeout in milliseconds (default: 30000)"
                    }
                },
                "required": ["url"]
            }),
        }
    }

    pub async fn execute(args: &Value) -> Result<ToolResult> {
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| crate::error::Error::Validation("Missing 'url' argument".to_string()))?;

        let timeout_ms = args.get("timeout_ms").and_then(|v| v.as_i64()).unwrap_or(30_000) as u64;

        // Validate URL
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Ok(ToolResult::error("URL must start with http:// or https://"));
        }

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(timeout_ms))
            .user_agent("Neuraphage/0.1")
            .build()
            .map_err(|e| crate::error::Error::Network(format!("Failed to create client: {}", e)))?;

        let response = client.get(url).send().await;

        match response {
            Ok(resp) => {
                let status = resp.status();
                if !status.is_success() {
                    return Ok(ToolResult::error(format!(
                        "HTTP error: {} {}",
                        status.as_u16(),
                        status.canonical_reason().unwrap_or("Unknown")
                    )));
                }

                match resp.text().await {
                    Ok(body) => {
                        // Truncate very large responses
                        let max_len = 100_000;
                        let content = if body.len() > max_len {
                            format!(
                                "{}...\n\n[Truncated: {} characters total]",
                                &body[..max_len],
                                body.len()
                            )
                        } else {
                            body
                        };
                        Ok(ToolResult::success(content))
                    }
                    Err(e) => Ok(ToolResult::error(format!("Failed to read response body: {}", e))),
                }
            }
            Err(e) => {
                if e.is_timeout() {
                    Ok(ToolResult::error(format!("Request timed out after {}ms", timeout_ms)))
                } else if e.is_connect() {
                    Ok(ToolResult::error(format!("Connection failed: {}", e)))
                } else {
                    Ok(ToolResult::error(format!("Request failed: {}", e)))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_web_fetch_invalid_url() {
        let result = WebFetchTool::execute(&serde_json::json!({"url": "not-a-url"}))
            .await
            .unwrap();
        assert!(result.output.contains("must start with http"));
    }

    #[tokio::test]
    async fn test_web_fetch_missing_url() {
        let result = WebFetchTool::execute(&serde_json::json!({})).await;
        assert!(result.is_err());
    }
}
