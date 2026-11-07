//! Minimal MCP server exposing the DeepSeek Search & Fetch APIs over stdio.

use std::sync::Arc;
use std::time::Duration;

use rmcp::{
    ErrorData, ServerHandler, ServiceExt, handler::server::wrapper::Parameters,
    model::CallToolResult, schemars, tool, tool_handler, tool_router, transport::stdio,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const DEFAULT_BASE_URL: &str = "https://api.deepseek.com";
const DEFAULT_TIMEOUT_SECS: u64 = 60;
const TIMEOUT_SECS_RANGE: (u64, u64) = (1, 600);
const DEFAULT_MAX_BODY_MB: u64 = 64;
const MAX_BODY_MB_RANGE: (u64, u64) = (1, 256);
const BYTES_PER_MB: u64 = 1024 * 1024;

const DEFAULT_COUNT: u16 = 10;
/// Maximum number of search results accepted by the API.
const MAX_RESULTS: u16 = 50;
/// `query` and `ori_question` are limited to 128 Unicode characters by the API.
const MAX_TEXT_CHARS: usize = 128;
/// `url` is limited to 8192 characters by the API.
const MAX_URL_CHARS: usize = 8192;
/// Longest slice of a non-JSON error body echoed back to the client.
const ERROR_PREVIEW_CHARS: usize = 500;

fn default_count() -> u16 {
    DEFAULT_COUNT
}

/// Runtime configuration, read from the environment.
#[derive(Debug, Clone)]
struct Config {
    api_key: String,
    base_url: String,
    timeout: Duration,
    max_body: u64,
}

impl Config {
    fn from_env() -> Result<Self, String> {
        let api_key = std::env::var("DEEPSEEK_API_KEY")
            .ok()
            .map(|key| key.trim().to_owned())
            .filter(|key| !key.is_empty())
            .ok_or_else(|| {
                "DEEPSEEK_API_KEY is not set (pass it through the MCP server env config)".to_owned()
            })?;
        Ok(Self {
            api_key,
            base_url: parse_base_url(std::env::var("DEEPSEEK_BASE_URL").ok())?,
            timeout: Duration::from_secs(parse_bounded(
                "DEEPSEEK_TIMEOUT_SECS",
                std::env::var("DEEPSEEK_TIMEOUT_SECS").ok(),
                DEFAULT_TIMEOUT_SECS,
                TIMEOUT_SECS_RANGE,
            )?),
            max_body: parse_bounded(
                "DEEPSEEK_MAX_BODY_MB",
                std::env::var("DEEPSEEK_MAX_BODY_MB").ok(),
                DEFAULT_MAX_BODY_MB,
                MAX_BODY_MB_RANGE,
            )? * BYTES_PER_MB,
        })
    }
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
    // The API accepts `null` for `ori_question` but not for `localization`, so
    // unset optionals are omitted from the request body entirely.
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

    /// Minimal, valid request used by `--check`.
    fn probe() -> Self {
        Self {
            query: "DeepSeek".to_owned(),
            count: 1,
            need_content: false,
            ori_question: None,
            localization: None,
        }
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

/// HTTP client shared by the tools and by `--check`.
struct Client {
    api_key: Arc<str>,
    base_url: Arc<str>,
    max_body: u64,
    agent: ureq::Agent,
}

impl Client {
    fn new(config: &Config) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(config.timeout))
            .http_status_as_error(false)
            .build()
            .into();
        Self {
            api_key: Arc::from(config.api_key.as_str()),
            base_url: Arc::from(config.base_url.as_str()),
            max_body: config.max_body,
            agent,
        }
    }

    fn request(&self, path: &str, body: &impl Serialize) -> Result<Value, ErrorData> {
        let body =
            serde_json::to_string(body).map_err(|e| internal(format!("invalid body: {e}")))?;
        let url = format!("{}{path}", self.base_url);
        let auth = format!("Bearer {}", self.api_key);
        let mut resp = self
            .agent
            .post(&url)
            .header("Authorization", &auth)
            .header("Content-Type", "application/json")
            .send(body)
            .map_err(|e| internal(format!("request to {url} failed: {e}")))?;
        let status = resp.status().as_u16();
        let text = resp
            .body_mut()
            .with_config()
            .limit(self.max_body)
            .lossy_utf8(true)
            .read_to_string()
            .map_err(|e| {
                internal(format!(
                    "reading DeepSeek response failed (HTTP {status}): {e}"
                ))
            })?;
        if (200..300).contains(&status) {
            serde_json::from_str(&text)
                .map_err(|e| internal(format!("invalid JSON from DeepSeek API: {e}")))
        } else {
            Err(status_error(status, &text))
        }
    }
}

#[derive(Clone)]
struct DeepSeekWeb {
    client: Arc<Client>,
}

#[tool_router]
impl DeepSeekWeb {
    #[tool(
        description = "Search the web via DeepSeek. Returns title, url, snippet, summary and (optionally) content for each result.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true)
    )]
    async fn search(
        &self,
        Parameters(p): Parameters<SearchParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let p = p.normalize()?;
        let value = self.post("/v1/tools/search", p).await?;
        Ok(CallToolResult::structured(value))
    }

    #[tool(
        description = "Fetch a single web page and return its content as Markdown.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true)
    )]
    async fn fetch(
        &self,
        Parameters(p): Parameters<FetchParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let p = p.normalize()?;
        let value = self.post("/v1/tools/fetch", p).await?;
        Ok(CallToolResult::structured(value))
    }
}

#[tool_handler(
    name = "deepseek-web-mcp",
    instructions = "Live web search and single-page fetching via the DeepSeek Web Search API"
)]
impl ServerHandler for DeepSeekWeb {}

impl DeepSeekWeb {
    /// Run the blocking HTTP request off the async runtime.
    async fn post(
        &self,
        path: &'static str,
        body: impl Serialize + Send + 'static,
    ) -> Result<Value, ErrorData> {
        let client = Arc::clone(&self.client);
        tokio::task::spawn_blocking(move || client.request(path, &body))
            .await
            .map_err(|e| internal(format!("request task failed: {e}")))?
    }
}

/// Translate a non-2xx DeepSeek response into an MCP error, keeping the
/// JSON-RPC code semantically meaningful for the client and the model.
fn status_error(status: u16, body: &str) -> ErrorData {
    let message = serde_json::from_str::<Value>(body)
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

/// Authority (`host[:port]`) of an `http`/`https` URL.
fn url_authority(url: &str) -> Option<&str> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    (!authority.is_empty()).then_some(authority)
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
    let authority = url_authority(url)
        .ok_or_else(|| invalid_arg("url must start with http:// or https:// and include a host"))?;
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

fn parse_base_url(raw: Option<String>) -> Result<String, String> {
    let Some(raw) = raw else {
        return Ok(DEFAULT_BASE_URL.to_owned());
    };
    let url = raw.trim().trim_end_matches('/');
    if url.is_empty() {
        return Ok(DEFAULT_BASE_URL.to_owned());
    }
    if url_authority(url).is_none() {
        return Err(format!(
            "DEEPSEEK_BASE_URL must be an http(s) URL with a host (got {url:?})"
        ));
    }
    Ok(url.to_owned())
}

fn parse_bounded(
    name: &str,
    raw: Option<String>,
    default: u64,
    (min, max): (u64, u64),
) -> Result<u64, String> {
    let Some(raw) = raw else {
        return Ok(default);
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(default);
    }
    let value: u64 = raw
        .parse()
        .map_err(|_| format!("{name} must be an integer between {min} and {max} (got {raw:?})"))?;
    if !(min..=max).contains(&value) {
        return Err(format!(
            "{name} must be between {min} and {max} (got {value})"
        ));
    }
    Ok(value)
}

/// What the process was asked to do on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Serve,
    Help,
    Version,
    Check,
}

impl Command {
    fn as_str(self) -> &'static str {
        match self {
            Self::Serve => "(none)",
            Self::Help => "--help",
            Self::Version => "--version",
            Self::Check => "--check",
        }
    }
}

fn parse_args<I>(args: I) -> Result<Command, String>
where
    I: IntoIterator<Item = String>,
{
    let mut command = Command::Serve;
    for arg in args {
        let next = match arg.as_str() {
            "-h" | "--help" => Command::Help,
            "-V" | "--version" => Command::Version,
            "--check" => Command::Check,
            other if other.starts_with('-') => return Err(format!("unknown option: {other}")),
            other => return Err(format!("unexpected argument: {other}")),
        };
        if command != Command::Serve && command != next {
            return Err(format!(
                "conflicting options: {} and {}",
                command.as_str(),
                next.as_str()
            ));
        }
        command = next;
    }
    Ok(command)
}

fn usage() -> String {
    format!(
        "\
deepseek-web-mcp {version}
MCP server exposing the DeepSeek Search & Fetch APIs over stdio.

USAGE:
    deepseek-web-mcp [OPTIONS]

OPTIONS:
    -h, --help       Print this help and exit
    -V, --version    Print the version and exit
        --check      Verify DEEPSEEK_API_KEY with one live search request, then exit.
                     NOTE: this performs one paid API call (~$0.00075 per request).

ENVIRONMENT:
    DEEPSEEK_API_KEY       required: DeepSeek API key
    DEEPSEEK_BASE_URL      default: {base_url}
    DEEPSEEK_TIMEOUT_SECS  default: {timeout} (range {tmin}-{tmax})
    DEEPSEEK_MAX_BODY_MB   default: {body} (range {bmin}-{bmax})

MCP CLIENT CONFIG (stdio transport):
    {{
      \"mcpServers\": {{
        \"deepseek-web\": {{
          \"command\": \"deepseek-web-mcp\",
          \"env\": {{ \"DEEPSEEK_API_KEY\": \"sk-...\" }}
        }}
      }}
    }}

TOOLS:
    search   Search the web; returns structured results (title, url, snippet, summary, content).
    fetch    Fetch a single page and return it as Markdown.
",
        version = env!("CARGO_PKG_VERSION"),
        base_url = DEFAULT_BASE_URL,
        timeout = DEFAULT_TIMEOUT_SECS,
        tmin = TIMEOUT_SECS_RANGE.0,
        tmax = TIMEOUT_SECS_RANGE.1,
        body = DEFAULT_MAX_BODY_MB,
        bmin = MAX_BODY_MB_RANGE.0,
        bmax = MAX_BODY_MB_RANGE.1,
    )
}

/// `--check`: prove that the key and base URL work, with one real request.
fn check() -> Result<(), String> {
    let config = Config::from_env()?;
    println!(
        "Checking DEEPSEEK_API_KEY against {} (one paid API call, ~$0.00075)...",
        config.base_url
    );
    let client = Client::new(&config);
    let value = client
        .request("/v1/tools/search", &SearchParams::probe())
        .map_err(|e| format!("check failed: {}", e.message))?;
    let results = value
        .get("results")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    println!(
        "OK: key is valid, search returned {} result(s).",
        results.len()
    );
    if let Some(first) = results.first() {
        let title = first
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("<no title>");
        let url = first
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("<no url>");
        println!("  first: {title} — {url}");
    }
    Ok(())
}

fn serve() -> Result<(), String> {
    let config = Config::from_env()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .map_err(|e| format!("cannot start async runtime: {e}"))?;
    runtime.block_on(async move {
        let client = Arc::new(Client::new(&config));
        let service = DeepSeekWeb { client }
            .serve(stdio())
            .await
            .map_err(|e| e.to_string())?;
        service.waiting().await.map_err(|e| e.to_string())?;
        Ok(())
    })
}

fn main() -> std::process::ExitCode {
    let command = match parse_args(std::env::args().skip(1)) {
        Ok(command) => command,
        Err(err) => {
            eprintln!("error: {err}\n\n{}", usage());
            return std::process::ExitCode::from(2);
        }
    };
    let outcome = match command {
        Command::Help => {
            print!("{}", usage());
            Ok(())
        }
        Command::Version => {
            println!("deepseek-web-mcp {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Command::Check => check(),
        Command::Serve => serve(),
    };
    match outcome {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn search(json: &str) -> SearchParams {
        serde_json::from_str(json).expect("valid params")
    }

    fn args(raw: &[&str]) -> Result<Command, String> {
        parse_args(raw.iter().map(|s| (*s).to_owned()))
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
            ("https://", "must start with http"),
            ("https:///path", "must start with http"),
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

    #[test]
    fn structured_results_carry_both_json_and_text() {
        let value = serde_json::json!({ "results": [{ "title": "t" }] });
        let result = CallToolResult::structured(value.clone());
        let wire = serde_json::to_value(&result).unwrap();
        assert_eq!(wire["structuredContent"], value);
        assert_eq!(wire["content"][0]["type"], "text");
        assert_eq!(wire["content"][0]["text"], value.to_string());
        assert_eq!(wire["isError"], false);
    }

    #[test]
    fn args_default_to_serving() {
        assert_eq!(args(&[]).unwrap(), Command::Serve);
    }

    #[test]
    fn args_parse_every_flag_and_its_short_form() {
        assert_eq!(args(&["--help"]).unwrap(), Command::Help);
        assert_eq!(args(&["-h"]).unwrap(), Command::Help);
        assert_eq!(args(&["--version"]).unwrap(), Command::Version);
        assert_eq!(args(&["-V"]).unwrap(), Command::Version);
        assert_eq!(args(&["--check"]).unwrap(), Command::Check);
    }

    #[test]
    fn args_reject_unknown_and_conflicting_flags() {
        let err = args(&["--nope"]).unwrap_err();
        assert!(err.contains("unknown option"), "{err}");
        let err = args(&["serve"]).unwrap_err();
        assert!(err.contains("unexpected argument"), "{err}");
        let err = args(&["--check", "--help"]).unwrap_err();
        assert!(err.contains("conflicting options"), "{err}");
    }

    #[test]
    fn usage_documents_flags_and_env() {
        let text = usage();
        for needle in [
            "--help",
            "--version",
            "--check",
            "paid API call",
            "DEEPSEEK_API_KEY",
            "DEEPSEEK_BASE_URL",
            "DEEPSEEK_TIMEOUT_SECS",
            "DEEPSEEK_MAX_BODY_MB",
            env!("CARGO_PKG_VERSION"),
        ] {
            assert!(text.contains(needle), "missing {needle}");
        }
    }

    #[test]
    fn base_url_defaults_validates_and_trims() {
        assert_eq!(parse_base_url(None).unwrap(), DEFAULT_BASE_URL);
        assert_eq!(parse_base_url(Some("  ".into())).unwrap(), DEFAULT_BASE_URL);
        assert_eq!(
            parse_base_url(Some("http://127.0.0.1:9/".into())).unwrap(),
            "http://127.0.0.1:9"
        );
        for bad in ["api.deepseek.com", "ftp://x", "https://"] {
            assert!(parse_base_url(Some(bad.into())).is_err(), "{bad}");
        }
    }

    #[test]
    fn bounded_env_values_are_validated() {
        let name = "DEEPSEEK_TIMEOUT_SECS";
        assert_eq!(parse_bounded(name, None, 60, (1, 600)).unwrap(), 60);
        assert_eq!(
            parse_bounded(name, Some(" 5 ".into()), 60, (1, 600)).unwrap(),
            5
        );
        assert_eq!(
            parse_bounded(name, Some("".into()), 60, (1, 600)).unwrap(),
            60
        );
        for bad in ["0", "601", "-1", "abc", "1.5"] {
            let err = parse_bounded(name, Some(bad.into()), 60, (1, 600)).unwrap_err();
            assert!(err.contains(name), "{bad}: {err}");
        }
    }

    #[test]
    fn probe_request_is_valid_and_cheapest_possible() {
        let probe = SearchParams::probe();
        assert_eq!(probe.count, 1);
        assert!(!probe.need_content);
        assert!(probe.normalize().is_ok());
    }
}
