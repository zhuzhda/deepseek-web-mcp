# DeepSeekWebSearchMCP

A lightweight Rust MCP server for the [DeepSeek Web Search API](https://api-docs.deepseek.com/guides/search).

## Tools

- `search` — search for web pages by query.
- `fetch` — fetch a single page and return it as Markdown.

## Installation

```bash
cargo install deepseek-web-mcp
```

Binary: `~/.cargo/bin/deepseek-web-mcp`.

## API Key

The key is read from the `DEEPSEEK_API_KEY` environment variable.

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

## License

MIT
