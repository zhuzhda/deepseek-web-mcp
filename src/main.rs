use std::sync::Arc;
use std::time::Duration;

use rmcp::{
    ErrorData, ServerHandler, ServiceExt, handler::server::wrapper::Parameters, schemars, tool,
    tool_handler, tool_router, transport::stdio,
};
use serde::{Deserialize, Serialize};

const BASE: &str = "https://api.deepseek.com";
/// `query` and `ori_question` are limited to 128 Unicode characters by the API.
const MAX_TEXT_CHARS: usize = 128;
/// Minimum and maximum number of search results.
const MAX_RESULTS: u16 = 50;
/// `url` is limited to 8192 characters by the API.
const MAX_URL_CHARS: usize = 8192;
/// Response bodies are buffered in memory, so cap them (ureq's default is only 10 MiB).
const MAX_BODY_BYTES: u64 = 64 * 1024 * 1024;
/// Longest slice of a non-JSON error body echoed back to the client.
const ERROR_PREVIEW_CHARS: usize = 500;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

fn default_count() -> u16 {
    10
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
struct SearchParams {
    /// Search query (1-128 chars). Operators: site:, -site:, -term, "phrase", lang:, after:, before:.
    query: String,
    /// Number of results (1-50).
    #[serde(default = "default_count")]
    #[schemars(range(min = 1, max = 50))]
    count: u16,
    /// Include full page content.
    #[serde(default)]
    need_content: bool,
    /// Original user question that triggered the search (1-128 chars).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ori_question: Option<String>,
    /// Optional IP address used to localize search results.
    // Not serialized when unset: the API accepts `null` for `ori_question` but not
    // for `localization`, so unset values are omitted from the request body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    localization: Option<String>,
}

impl SearchParams {
    /// Trim, drop empty optional values and reject anything the API would reject.
    fn normalize(mut self) -> Result<Self, ErrorData> {
        self.query = self.query.trim().to_owned();
        let query_len = char_len(&self.query);
        if query_len == 0 {
            return Err(invalid_arg("query must not be empty"));
        }
        if query_len > MAX_TEXT_CHARS {
            return Err(invalid_arg(format!(
                "query must be at most {MAX_TEXT_CHARS} characters (got {query_len})"
            )));
        }
        if !(1..=MAX_RESULTS).contains(&self.count) {
            return Err(invalid_arg(format!(
                "count must be between 1 and {MAX_RESULTS} (got {})",
                self.count
            )));
        }
        self.ori_question = normalize_optional(self.ori_question, "ori_question")?;
        self.localization = normalize_optional(self.localization, "localization")?;
        Ok(self)
    }
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
struct FetchParams {
    /// Page URL (http/https).
    url: String,
}

impl FetchParams {
    fn normalize(mut self) -> Result<Self, ErrorData> {
        self.url = self.url.trim().to_owned();
        validate_url(&self.url)?;
        Ok(self)
    }
}

#[derive(Clone)]
struct DeepSeekWeb {
    key: Arc<str>,
    agent: ureq::Agent,
}

#[tool_router]
impl DeepSeekWeb {
    #[tool(
        description = "Search the web via DeepSeek. Returns title, url, snippet, summary and (optionally) content for each result.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true)
    )]
    async fn search(&self, Parameters(p): Parameters<SearchParams>) -> Result<String, ErrorData> {
        let p = p.normalize()?;
        self.post("/v1/tools/search", &p).await
    }

    #[tool(
        description = "Fetch a single web page and return its content as Markdown.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true)
    )]
    async fn fetch(&self, Parameters(p): Parameters<FetchParams>) -> Result<String, ErrorData> {
        let p = p.normalize()?;
        self.post("/v1/tools/fetch", &p).await
    }
}

#[tool_handler(
    name = "deepseek-web-mcp",
    instructions = "Live web search and single-page fetching via the DeepSeek Web Search API"
)]
impl ServerHandler for DeepSeekWeb {}

impl DeepSeekWeb {
    async fn post(&self, path: &str, body: &impl Serialize) -> Result<String, ErrorData> {
        let body =
            serde_json::to_string(body).map_err(|e| internal(format!("invalid body: {e}")))?;
        let url = format!("{BASE}{path}");
        let auth = format!("Bearer {}", self.key);
        let agent = self.agent.clone();
        tokio::task::spawn_blocking(move || {
            let mut resp = agent
                .post(&url)
                .header("Authorization", &auth)
                .header("Content-Type", "application/json")
                .send(body)
                .map_err(|e| internal(format!("request to {url} failed: {e}")))?;
            let status = resp.status().as_u16();
            let text = resp
                .body_mut()
                .with_config()
                .limit(MAX_BODY_BYTES)
                .lossy_utf8(true)
                .read_to_string()
                .map_err(|e| {
                    internal(format!(
                        "reading DeepSeek response failed (HTTP {status}): {e}"
                    ))
                })?;
            if (200..300).contains(&status) {
                Ok(text)
            } else {
                Err(status_error(status, &text))
            }
        })
        .await
        .map_err(|e| internal(format!("request task failed: {e}")))?
    }
}

/// Translate a non-2xx DeepSeek response into an MCP error, keeping the
/// JSON-RPC code semantically meaningful for the client and the model.
fn status_error(status: u16, body: &str) -> ErrorData {
    let message = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .and_then(|m| m.as_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| preview(body));
    let message = format!("DeepSeek API {status}: {message}");
    match status {
        // Malformed or unsatisfiable request: retrying verbatim will not help.
        400 | 413 => invalid_arg(message),
        // The page could not be retrieved; another URL might work.
        404 => invalid_arg(format!("{message} (page unavailable or blocked)")),
        // Transient server-side conditions.
        408 | 429 => internal(format!(
            "{message} (rate limited or timed out, retry later)"
        )),
        // Configuration / account problems.
        401..=403 => internal(format!(
            "{message} (check DEEPSEEK_API_KEY and account balance)"
        )),
        status if status >= 500 => internal(format!("{message} (upstream error, retry later)")),
        _ => internal(message),
    }
}

fn invalid_arg(message: impl Into<String>) -> ErrorData {
    ErrorData::invalid_params(message.into(), None)
}

fn internal(message: impl Into<String>) -> ErrorData {
    ErrorData::internal_error(message.into(), None)
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// Trim an optional free-text field, turning it into `None` when blank and
/// rejecting values the API would reject.
fn normalize_optional(value: Option<String>, field: &str) -> Result<Option<String>, ErrorData> {
    match value.map(|v| v.trim().to_owned()) {
        None => Ok(None),
        Some(v) if v.is_empty() => Ok(None),
        Some(v) if char_len(&v) > MAX_TEXT_CHARS => Err(invalid_arg(format!(
            "{field} must be at most {MAX_TEXT_CHARS} characters (got {})",
            char_len(&v)
        ))),
        Some(v) => Ok(Some(v)),
    }
}

fn validate_url(url: &str) -> Result<(), ErrorData> {
    let len = char_len(url);
    if len == 0 {
        return Err(invalid_arg("url must not be empty"));
    }
    if len > MAX_URL_CHARS {
        return Err(invalid_arg(format!(
            "url must be at most {MAX_URL_CHARS} characters (got {len})"
        )));
    }
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or_else(|| invalid_arg("url must start with http:// or https://"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() {
        return Err(invalid_arg("url must include a host"));
    }
    if authority.contains('@') {
        return Err(invalid_arg("url must not contain a username or password"));
    }
    Ok(())
}

/// Short, single-line preview of a non-JSON body (HTML error pages etc.).
fn preview(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        return "<empty response body>".to_owned();
    }
    if char_len(text) <= ERROR_PREVIEW_CHARS {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(ERROR_PREVIEW_CHARS).collect();
    out.push('…');
    out
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let key = std::env::var("DEEPSEEK_API_KEY")
        .ok()
        .map(|key| key.trim().to_owned())
        .filter(|key| !key.is_empty())
        .ok_or("DEEPSEEK_API_KEY is not set (pass it through the MCP server env config)")?;

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        .http_status_as_error(false)
        .build()
        .into();

    let service = DeepSeekWeb {
        key: Arc::from(key),
        agent,
    }
    .serve(stdio())
    .await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn search(json: &str) -> SearchParams {
        serde_json::from_str(json).expect("valid params")
    }

    #[test]
    fn search_defaults_match_api_defaults() {
        let p = search(r#"{"query":"DeepSeek"}"#).normalize().unwrap();
        assert_eq!(p.count, 10);
        assert!(!p.need_content);
        assert_eq!(p.ori_question, None);
        assert_eq!(p.localization, None);
    }

    #[test]
    fn search_rejects_empty_and_blank_query() {
        for json in [r#"{"query":""}"#, r#"{"query":"   "}"#] {
            let err = search(json).normalize().unwrap_err();
            assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
            assert!(err.message.contains("query must not be empty"), "{err:?}");
        }
    }

    #[test]
    fn search_rejects_overlong_query_but_counts_chars_not_bytes() {
        let too_long = "я".repeat(MAX_TEXT_CHARS + 1);
        let err = search(&serde_json::json!({ "query": too_long }).to_string())
            .normalize()
            .unwrap_err();
        assert!(err.message.contains("at most 128"), "{err:?}");

        // 128 multi-byte characters are fine: the limit is in chars, not bytes.
        let max_ok = "я".repeat(MAX_TEXT_CHARS);
        assert!(
            search(&serde_json::json!({ "query": max_ok }).to_string())
                .normalize()
                .is_ok()
        );
    }

    #[test]
    fn search_rejects_out_of_range_count() {
        for count in [0, 51, 1000] {
            let err = search(&serde_json::json!({ "query": "q", "count": count }).to_string())
                .normalize()
                .unwrap_err();
            assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
            assert!(err.message.contains("count must be between 1 and 50"));
        }
        assert!(search(r#"{"query":"q","count":50}"#).normalize().is_ok());
    }

    #[test]
    fn request_body_omits_unset_optional_fields() {
        // The API rejects JSON `null` for `localization`, so unset optionals
        // must be dropped from the body instead of being serialized as null.
        let body =
            serde_json::to_string(&search(r#"{"query":" q "}"#).normalize().unwrap()).unwrap();
        assert_eq!(body, r#"{"query":"q","count":10,"need_content":false}"#);

        let body = serde_json::to_string(
            &search(r#"{"query":"q","localization":"1.2.3.4","ori_question":"why"}"#)
                .normalize()
                .unwrap(),
        )
        .unwrap();
        assert!(body.contains(r#""localization":"1.2.3.4""#), "{body}");
        assert!(body.contains(r#""ori_question":"why""#), "{body}");
    }

    #[test]
    fn search_normalizes_optional_fields() {
        let p = search(r#"{"query":"  q  ","ori_question":"  ","localization":"  "}"#)
            .normalize()
            .unwrap();
        assert_eq!(p.query, "q");
        assert_eq!(p.ori_question, None);
        assert_eq!(p.localization, None);
    }

    #[test]
    fn fetch_accepts_plain_urls() {
        for url in [
            "https://example.com",
            "https://example.com/a?b=c#d",
            "http://127.0.0.1:8080/x",
            "  https://example.com/pad  ",
        ] {
            let p: FetchParams = serde_json::from_str(&format!(r#"{{"url":"{url}"}}"#)).unwrap();
            assert!(p.normalize().is_ok(), "{url}");
        }
    }

    #[test]
    fn fetch_rejects_bad_urls() {
        for (url, hint) in [
            ("", "must not be empty"),
            ("example.com", "must start with http"),
            ("ftp://example.com", "must start with http"),
            ("https://", "must include a host"),
            ("https:///path", "must include a host"),
            ("https://user:pw@example.com", "username or password"),
        ] {
            let p: FetchParams =
                serde_json::from_str(&serde_json::json!({ "url": url }).to_string()).unwrap();
            let err = p.normalize().unwrap_err();
            assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
            assert!(err.message.contains(hint), "{url}: {err:?}");
        }
    }

    #[test]
    fn fetch_rejects_overlong_url() {
        let url = format!("https://example.com/{}", "a".repeat(MAX_URL_CHARS));
        let p: FetchParams =
            serde_json::from_str(&serde_json::json!({ "url": url }).to_string()).unwrap();
        assert!(p.normalize().unwrap_err().message.contains("at most 8192"));
    }

    #[test]
    fn status_errors_map_to_meaningful_codes() {
        let body = r#"{"error":{"message":"'count' must be between 1 and 50","type":"invalid_request_error","param":null,"code":"invalid_request_error"}}"#;
        let e = status_error(400, body);
        assert_eq!(e.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(e.message.contains("'count' must be between 1 and 50"));

        assert_eq!(
            status_error(404, "not found").code,
            rmcp::model::ErrorCode::INVALID_PARAMS
        );
        for status in [401, 402, 403, 408, 429, 500, 504, 418] {
            assert_eq!(
                status_error(status, "").code,
                rmcp::model::ErrorCode::INTERNAL_ERROR,
                "{status}"
            );
        }
        assert!(status_error(401, "").message.contains("DEEPSEEK_API_KEY"));
        assert!(status_error(429, "").message.contains("retry later"));
    }

    #[test]
    fn preview_truncates_and_flags_empty_bodies() {
        assert_eq!(preview("  \n "), "<empty response body>");
        assert_eq!(preview("  hi  "), "hi");
        let long = "x".repeat(ERROR_PREVIEW_CHARS + 100);
        let p = preview(&long);
        assert_eq!(char_len(&p), ERROR_PREVIEW_CHARS + 1);
        assert!(p.ends_with('…'));
    }
}
