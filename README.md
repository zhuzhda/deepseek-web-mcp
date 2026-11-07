# DeepSeekWebSearchMCP

A lightweight Rust MCP server exposing the [DeepSeek Search & Fetch API](https://api-docs.deepseek.com/guides/search).
It speaks MCP over stdio and ships as a single small static binary.

## Tools

### `search`

Search the web via DeepSeek's in-house search engine. Returns the raw JSON
response (`{"results": [...]}`), where each result has `title`, `url`,
`snippet`, `summary`, `content`, `site_name`, `site_icon`, `date`,
`authority` and `rerank_score`.

| Argument | Type | Required | Default | Description |
|---|---|---|---|---|
| `query` | string | yes | – | 1–128 characters. Supports operators: `site:`, `-site:`, `-term`, `-"phrase"`, `"phrase"`, `lang:`, `after:`, `before:`. |
| `count` | integer | no | `10` | Number of results, 1–50. |
| `need_content` | boolean | no | `false` | Also return full page content in `content`. |
| `ori_question` | string | no | – | The user's original question (1–128 chars) to improve query understanding. |
| `localization` | string | no | – | IP address used to localize search results. |

### `fetch`

Fetch a single page and return it as Markdown (`{"title": ..., "content": ...}`).

| Argument | Type | Required | Description |
|---|---|---|---|
| `url` | string | yes | `http`/`https` URL with a host, at most 8192 chars, no embedded username/password. |

Both tools are read-only and forward the API response verbatim. Arguments are
validated before the request is made, so malformed input is rejected locally
(`InvalidParams`) instead of costing an API call.

## Installation

```bash
cargo install deepseek-web-mcp
```

Binary: `~/.cargo/bin/deepseek-web-mcp`.

From a checkout:

```bash
cargo install --path .
```

Requires Rust 1.88 or newer (edition 2024, the minimum supported by `rmcp`).

## API Key

The key is read from the `DEEPSEEK_API_KEY` environment variable (surrounding
whitespace and newlines are stripped). The server exits immediately with an
error if the variable is missing or empty.

## Registering with an MCP client

Add the server to your client config as a stdio command:

```json
{
  "mcpServers": {
    "deepseek-web": {
      "command": "deepseek-web-mcp",
      "env": { "DEEPSEEK_API_KEY": "sk-..." }
    }
  }
}
```

The binary is on `PATH` after installation (`~/.cargo/bin`). If your client does not
inherit `PATH`, use `~/.cargo/bin/deepseek-web-mcp` instead.

## Errors

API failures are reported as JSON-RPC errors with a meaningful code:

| Condition | JSON-RPC code |
|---|---|
| Invalid arguments (`400`, `413`) | `-32602` Invalid params |
| Page could not be retrieved (`404`) | `-32602` Invalid params |
| Bad/absent API key, insufficient balance (`401`–`403`) | `-32603` Internal error |
| Rate limit or timeout (`408`, `429`) | `-32603` Internal error (retry later) |
| Upstream failure (`5xx`) | `-32603` Internal error (retry later) |

Error messages come from the DeepSeek `error.message` field; non-JSON bodies are
truncated to a short preview. Requests use a 60 s timeout and responses are
capped at 64 MiB.

## Cost

DeepSeek bills Search and Fetch per successful request (currently
$0.75 per 1,000 requests each), not per result. Failed requests and empty
results are free. Each account is rate limited to 100 QPS for search and
25 QPS for fetch.

## Development

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## License

MIT
