//! Web search tool: search the web using multiple backends.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Tool, ToolResult};
use crate::error::Result;

/// Search backend configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SearchBackend {
    /// Brave Search API (free tier: 2k queries/month)
    Brave,
    /// Kagi Search API (premium, $25/1k queries)
    Kagi,
    /// Self-hosted SearXNG instance (no rate limits)
    #[default]
    SearXNG,
}

/// Web search configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchConfig {
    /// Which backend to use.
    #[serde(default)]
    pub backend: SearchBackend,
    /// Path to Brave API key file.
    #[serde(default = "default_brave_key_path")]
    pub brave_key_path: PathBuf,
    /// Path to Kagi API key file.
    #[serde(default = "default_kagi_key_path")]
    pub kagi_key_path: PathBuf,
    /// SearXNG instance URL.
    #[serde(default = "default_searxng_url")]
    pub searxng_url: String,
    /// Maximum number of results to return.
    #[serde(default = "default_max_results")]
    pub max_results: usize,
}

fn default_brave_key_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("brave/tokens/brave-api-key")
}

fn default_kagi_key_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("kagi/tokens/kagi-api-key")
}

fn default_searxng_url() -> String {
    "http://localhost:8080".to_string()
}

fn default_max_results() -> usize {
    10
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            backend: SearchBackend::default(),
            brave_key_path: default_brave_key_path(),
            kagi_key_path: default_kagi_key_path(),
            searxng_url: default_searxng_url(),
            max_results: default_max_results(),
        }
    }
}

/// A single search result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// WebSearch tool - searches the web using configured backend.
pub struct WebSearchTool;

/// URL-encode a string for use in query parameters.
fn url_encode(s: &str) -> String {
    let mut result = String::new();
    for c in s.chars() {
        match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' => result.push(c),
            ' ' => result.push('+'),
            _ => {
                for byte in c.to_string().as_bytes() {
                    result.push_str(&format!("%{:02X}", byte));
                }
            }
        }
    }
    result
}

impl WebSearchTool {
    pub fn definition() -> Tool {
        Tool {
            name: "web_search".to_string(),
            description: "Search the web for information. Returns titles, URLs, and snippets.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query"
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Maximum number of results (default: 10)"
                    }
                },
                "required": ["query"]
            }),
        }
    }

    pub async fn execute(config: &SearchConfig, args: &Value) -> Result<ToolResult> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| crate::error::Error::Validation("Missing 'query' argument".to_string()))?;

        let max_results = args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(config.max_results);

        let results = match config.backend {
            SearchBackend::Brave => Self::search_brave(config, query, max_results).await,
            SearchBackend::Kagi => Self::search_kagi(config, query, max_results).await,
            SearchBackend::SearXNG => Self::search_searxng(config, query, max_results).await,
        };

        match results {
            Ok(results) => {
                if results.is_empty() {
                    return Ok(ToolResult::success("No results found."));
                }

                let mut output = format!("Found {} results:\n\n", results.len());
                for (i, result) in results.iter().enumerate() {
                    output.push_str(&format!(
                        "{}. {}\n   {}\n   {}\n\n",
                        i + 1,
                        result.title,
                        result.url,
                        result.snippet
                    ));
                }
                Ok(ToolResult::success(output))
            }
            Err(e) => Ok(ToolResult::error(format!("Search failed: {}", e))),
        }
    }

    async fn search_brave(
        config: &SearchConfig,
        query: &str,
        max_results: usize,
    ) -> std::result::Result<Vec<SearchResult>, String> {
        let api_key = std::fs::read_to_string(&config.brave_key_path)
            .map_err(|e| format!("Failed to read Brave API key from {:?}: {}", config.brave_key_path, e))?
            .trim()
            .to_string();

        let url = format!(
            "https://api.search.brave.com/res/v1/web/search?q={}&count={}",
            url_encode(query),
            max_results
        );

        let client = reqwest::Client::new();
        let response = client
            .get(&url)
            .header("X-Subscription-Token", api_key)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| format!("Brave API request failed: {}", e))?;

        if !response.status().is_success() {
            return Err(format!("Brave API error: {}", response.status()));
        }

        let json: Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse Brave response: {}", e))?;

        let mut results = Vec::new();
        if let Some(web) = json
            .get("web")
            .and_then(|w| w.get("results"))
            .and_then(|r| r.as_array())
        {
            for item in web.iter().take(max_results) {
                if let (Some(title), Some(url)) = (
                    item.get("title").and_then(|t| t.as_str()),
                    item.get("url").and_then(|u| u.as_str()),
                ) {
                    let snippet = item
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("")
                        .to_string();
                    results.push(SearchResult {
                        title: title.to_string(),
                        url: url.to_string(),
                        snippet,
                    });
                }
            }
        }

        Ok(results)
    }

    async fn search_kagi(
        config: &SearchConfig,
        query: &str,
        max_results: usize,
    ) -> std::result::Result<Vec<SearchResult>, String> {
        let api_key = std::fs::read_to_string(&config.kagi_key_path)
            .map_err(|e| format!("Failed to read Kagi API key from {:?}: {}", config.kagi_key_path, e))?
            .trim()
            .to_string();

        let url = format!(
            "https://kagi.com/api/v0/search?q={}&limit={}",
            url_encode(query),
            max_results
        );

        let client = reqwest::Client::new();
        let response = client
            .get(&url)
            .header("Authorization", format!("Bot {}", api_key))
            .send()
            .await
            .map_err(|e| format!("Kagi API request failed: {}", e))?;

        if !response.status().is_success() {
            return Err(format!("Kagi API error: {}", response.status()));
        }

        let json: Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse Kagi response: {}", e))?;

        let mut results = Vec::new();
        if let Some(data) = json.get("data").and_then(|d| d.as_array()) {
            for item in data.iter().take(max_results) {
                // Kagi returns different types of results, we want type 0 (web results)
                let result_type = item.get("t").and_then(|t| t.as_i64()).unwrap_or(-1);
                if result_type != 0 {
                    continue;
                }

                if let (Some(title), Some(url)) = (
                    item.get("title").and_then(|t| t.as_str()),
                    item.get("url").and_then(|u| u.as_str()),
                ) {
                    let snippet = item.get("snippet").and_then(|s| s.as_str()).unwrap_or("").to_string();
                    results.push(SearchResult {
                        title: title.to_string(),
                        url: url.to_string(),
                        snippet,
                    });
                }
            }
        }

        Ok(results)
    }

    async fn search_searxng(
        config: &SearchConfig,
        query: &str,
        max_results: usize,
    ) -> std::result::Result<Vec<SearchResult>, String> {
        let url = format!("{}/search?q={}&format=json", config.searxng_url, url_encode(query));

        let client = reqwest::Client::new();
        let response = client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("SearXNG request failed: {}", e))?;

        if !response.status().is_success() {
            return Err(format!("SearXNG error: {}", response.status()));
        }

        let json: Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse SearXNG response: {}", e))?;

        let mut results = Vec::new();
        if let Some(items) = json.get("results").and_then(|r| r.as_array()) {
            for item in items.iter().take(max_results) {
                if let (Some(title), Some(url)) = (
                    item.get("title").and_then(|t| t.as_str()),
                    item.get("url").and_then(|u| u.as_str()),
                ) {
                    let snippet = item.get("content").and_then(|c| c.as_str()).unwrap_or("").to_string();
                    results.push(SearchResult {
                        title: title.to_string(),
                        url: url.to_string(),
                        snippet,
                    });
                }
            }
        }

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_config_default() {
        let config = SearchConfig::default();
        assert!(matches!(config.backend, SearchBackend::SearXNG));
        assert_eq!(config.max_results, 10);
        assert_eq!(config.searxng_url, "http://localhost:8080");
    }

    #[test]
    fn test_search_backend_serde() {
        let brave: SearchBackend = serde_json::from_str("\"brave\"").unwrap();
        assert!(matches!(brave, SearchBackend::Brave));

        let kagi: SearchBackend = serde_json::from_str("\"kagi\"").unwrap();
        assert!(matches!(kagi, SearchBackend::Kagi));

        let searxng: SearchBackend = serde_json::from_str("\"sear_x_n_g\"").unwrap();
        assert!(matches!(searxng, SearchBackend::SearXNG));
    }

    #[test]
    fn test_default_paths() {
        let brave_path = default_brave_key_path();
        assert!(brave_path.to_string_lossy().contains("brave"));

        let kagi_path = default_kagi_key_path();
        assert!(kagi_path.to_string_lossy().contains("kagi"));
    }

    #[test]
    fn test_search_result_serde() {
        let result = SearchResult {
            title: "Test".to_string(),
            url: "https://example.com".to_string(),
            snippet: "A test result".to_string(),
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("Test"));
        assert!(json.contains("https://example.com"));
    }

    #[test]
    fn test_url_encode() {
        assert_eq!(url_encode("hello world"), "hello+world");
        assert_eq!(url_encode("rust programming"), "rust+programming");
        assert_eq!(url_encode("test"), "test");
        assert_eq!(url_encode("a&b=c"), "a%26b%3Dc");
    }

    /// Integration test - requires SearXNG running at localhost:8080
    #[tokio::test]
    #[ignore]
    async fn test_searxng_integration() {
        let config = SearchConfig::default();
        assert!(matches!(config.backend, SearchBackend::SearXNG));

        let result = WebSearchTool::execute(
            &config,
            &serde_json::json!({"query": "rust programming language", "max_results": 3}),
        )
        .await
        .unwrap();

        assert!(!result.is_failure);
        assert!(result.output.contains("rust") || result.output.contains("Rust"));
        println!("SearXNG results:\n{}", result.output);
    }
}
