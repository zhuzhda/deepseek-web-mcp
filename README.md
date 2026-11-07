# DeepSeekWebSearchMCP

A lightweight Rust MCP server exposing the [DeepSeek Search & Fetch API](https://api-docs.deepseek.com/guides/search).
It speaks MCP over stdio and ships as a single small native binary (~2.7 MB) with no runtime.

## Tools

### `search`

Search the web via DeepSeek's in-house search engine.

| Argument | Type | Required | Default | Description |
|---|---|---|---|---|
| `query` | string | yes | – | 1–128 characters. Supports operators: `site:`, `-site:`, `-term`, `-"phrase"`, `"phrase"`, `lang:`, `after:`, `before:`. |
| `count` | integer | no | `10` | Number of results, 1–50. |
| `need_content` | boolean | no | `false` | Also return full page content in `content`. |
| `ori_question` | string | no | – | The user's original question (1–128 chars) to improve query understanding. |
| `localization` | string | no | – | IP address used to localize search results. |

Response: `{"results": [...]}` where each item has `title`, `url`, `snippet`,
`summary`, `content`, `site_name`, `site_icon`, `date`, `authority`, `rerank_score`.

### `fetch`

Fetch a single page and return it as Markdown: `{"title": ..., "content": ...}`.

| Argument | Type | Required | Description |
|---|---|---|---|
| `url` | string | yes | `http`/`https` URL with a host, at most 8192 chars, no embedded username/password. |

### `structuredContent`

Both tools return the API payload twice: as `structuredContent` (JSON, for clients
that consume structured tool output) and as a text block with the same JSON (for
backward compatibility with text-only clients).

Arguments are validated before the request is made, so malformed input is rejected
locally (`InvalidParams`) instead of costing an API call.

## Installation

### Prebuilt binary (`cargo binstall`)

```bash
cargo binstall deepseek-web-mcp
```

### Install script (Linux/macOS, no Rust required)

```bash
curl -fsSL https://raw.githubusercontent.com/zhuzhda/deepseek-web-mcp/main/install.sh | sh
```

The script detects the platform, downloads the matching archive from the latest
GitHub release, verifies its `sha256`, and installs to `${DEEPSEEK_INSTALL_DIR:-$HOME/.local/bin}`.
Use `VERSION=0.2.0` to pin a release.

### From crates.io (needs Rust 1.89+)

```bash
cargo install deepseek-web-mcp
```

Binary: `~/.cargo/bin/deepseek-web-mcp`. From a checkout: `cargo install --path .`.

### Windows

Download `deepseek-web-mcp-x86_64-pc-windows-msvc.zip` from the
[latest release](https://github.com/zhuzhda/deepseek-web-mcp/releases/latest),
unpack it, and make sure `deepseek-web-mcp.exe` is on `PATH` (or reference it by full path):

```powershell
& "$HOME\.local\bin\deepseek-web-mcp.exe" --version
```

The Windows build uses a static CRT, so no Visual C++ Redistributable is required.
`cargo binstall deepseek-web-mcp` picks the `.zip` automatically.

Known Windows caveats: an unsigned `.exe` may trigger SmartScreen (the download
warning appears only for browser downloads, not for `curl`/PowerShell/`binstall`)
and some antivirus engines flag unsigned binaries. Code signing is not set up.

## Configuration

| Variable | Default | Description |
|---|---|---|
| `DEEPSEEK_API_KEY` | – (required) | DeepSeek API key. Surrounding whitespace is stripped. |
| `DEEPSEEK_BASE_URL` | `https://api.deepseek.com` | API base URL; must be `http(s)://` with a host. Trailing `/` is ignored. |
| `DEEPSEEK_TIMEOUT_SECS` | `60` | Per-request timeout in seconds, 1–600. |
| `DEEPSEEK_MAX_BODY_MB` | `64` | Maximum response body size in MiB, 1–256. |

Invalid values abort startup with a clear message. The server exits immediately
when `DEEPSEEK_API_KEY` is missing or empty.

## CLI

```
deepseek-web-mcp [OPTIONS]

  -h, --help       Short help with an MCP config example
  -V, --version    Print the version
      --check      Verify the API key with one live search request
```

`--check` performs **one paid API request** (~$0.00075) and prints the outcome:

```
$ deepseek-web-mcp --check
Checking DEEPSEEK_API_KEY against https://api.deepseek.com (one paid API call, ~$0.00075)...
OK: key is valid, search returned 1 result(s).
  first: ... — https://...
```

Unknown or conflicting options exit with status `2`.

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

The binary is on `PATH` after installation (`~/.cargo/bin` for `cargo install`,
`~/.local/bin` for the install script). If your client does not inherit `PATH`,
use the absolute path instead.

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
truncated to a short preview.

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

Releases are produced by `.github/workflows/release.yml` on a `v*` tag:
`x86_64`/`aarch64` Linux (musl), `x86_64`/`aarch64` macOS and `x86_64` Windows,
each with a `sha256` checksum.

Known limitation: cancelling a tool call in the client does not abort the
in-flight HTTP request, because the blocking `ureq` call runs on a worker thread.

## License

MIT
