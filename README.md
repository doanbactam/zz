# ZeroZero

> Experimental project — a CLI coding agent written in 100% Rust.

`zz` is a command-line agent (similar to `codex` / `claude`) built to
experiment with agent loop architecture, TUI, sandboxing, and tool systems
in pure Rust.

Not a production product. The code is for learning and experimentation.

## Try it

```bash
git clone https://github.com/doanbactam/zz
cd zz
cargo build --release
# Binary: target/release/zz

# Set API key (default provider: xAI/Grok)
export XAI_API_KEY="xai-..."

# Run TUI
zz

# Or headless mode
zz exec "Fix the typo in src/main.rs"
```

## Features

- **TUI** (ratatui): chat streaming, diff view, slash commands, syntax highlight
- **Headless** (`zz exec`): JSONL output for CI/scripting
- **Multi-agent**: run multiple agents in parallel
- **Sandbox**: Landlock + seccomp (Linux), approval gate for dangerous commands
- **Tools**: read/write/edit file, bash, grep, glob, web search/fetch, git
- **Providers**: xAI, OpenAI, Anthropic, Gemini, Ollama (local)
- **Skills**: load `SKILL.md` from `.zerozero/skills/`, slash commands
- **Session**: SQLite history, rewind to checkpoint

## Architecture

```
zz (binary)
├── crates/cli        — CLI (clap), dispatch TUI/exec
├── crates/tui        — ratatui TUI
├── crates/exec       — headless JSONL
├── crates/core       — agent loop, session, compaction
├── crates/llm        — Provider trait (OpenAI, Anthropic, xAI, Gemini, Ollama)
├── crates/tools      — Tool trait (read, write, edit, bash, grep, glob, web)
├── crates/sandbox    — Landlock + seccomp, approval gates
├── crates/session    — SQLite persistence
├── crates/compaction — context window compaction
├── crates/multi-agent — parallel orchestration
├── crates/skills     — SKILL.md loader
├── crates/mcp        — MCP client/server
└── crates/plugins    — stdio JSON-RPC plugins
```

## Development

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

## License

MIT
