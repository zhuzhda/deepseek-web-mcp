use std::sync::Arc;
use std::time::Duration;

use rmcp::{
    handler::server::wrapper::Parameters,
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
    ErrorData, ServerHandler, ServiceExt,
};
use serde::{Deserialize, Serialize};

const BASE: &str = "https://api.deepseek.com";

fn default_count() -> u8 {
    10
}

#[derive(Deserialize, Serialize, schemars::JsonSchema)]
struct SearchParams {
    /// Search query (1-128 chars). Operators: site:, -site:, -term, "phrase", lang:, after:, before:.
    query: String,
    /// Number of results (1-50).
    #[serde(default = "default_count")]
    #[schemars(range(min = 1, max = 50))]
    count: u8,
    /// Include full page content.
    #[serde(default)]
    need_content: bool,
    /// Original user question.
    ori_question: Option<String>,
}

#[derive(Deserialize, Serialize, schemars::JsonSchema)]
struct FetchParams {
    /// Page URL (http/https).
    url: String,
}

#[derive(Clone)]
struct DeepSeekWeb {
    key: Arc<str>,
    agent: ureq::Agent,
}

#[tool_router]
impl DeepSeekWeb {
    #[tool(description = "Search the web via DeepSeek. Returns title, url, snippet, summary and (optionally) content for each result.", annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true))]
    async fn search(&self, Parameters(p): Parameters<SearchParams>) -> Result<String, ErrorData> {
        self.post("/v1/tools/search", &p).await
    }

    #[tool(description = "Fetch a single web page and return its content as Markdown.", annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true))]
    async fn fetch(&self, Parameters(p): Parameters<FetchParams>) -> Result<String, ErrorData> {
        self.post("/v1/tools/fetch", &p).await
    }
}

#[tool_handler(
    name = "deepseek-web-mcp",
    version = "0.1.1",
    instructions = "Live web search and single-page fetching via the DeepSeek Web Search API"
)]
impl ServerHandler for DeepSeekWeb {}

impl DeepSeekWeb {
    async fn post(&self, path: &str, body: &impl Serialize) -> Result<String, ErrorData> {
        let body = serde_json::to_string(body)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let key = Arc::clone(&self.key);
        let agent = self.agent.clone();
        let url = format!("{BASE}{path}");
        tokio::task::spawn_blocking(move || {
            let auth = format!("Bearer {key}");
            let mut resp = agent
                .post(&url)
                .header("Authorization", &auth)
                .header("Content-Type", "application/json")
                .send(body)
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
            let status = resp.status().as_u16();
            let text = resp
                .body_mut()
                .read_to_string()
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
            if (200..300).contains(&status) {
                Ok(text)
            } else {
                let msg = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| {
                        v.pointer("/error/message")
                            .and_then(|m| m.as_str())
                            .map(str::to_owned)
                    })
                    .unwrap_or(text);
                Err(ErrorData::internal_error(
                    format!("DeepSeek API {status}: {msg}"),
                    None,
                ))
            }
        })
        .await
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let key = std::env::var("DEEPSEEK_API_KEY").map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "DEEPSEEK_API_KEY is not set",
        )
    })?;

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .http_status_as_error(false)
        .build()
        .into();

    let service = DeepSeekWeb { key: Arc::from(key), agent }
        .serve(stdio())
        .await?;
    service.waiting().await?;
    Ok(())
}
