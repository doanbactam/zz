//! MCP server mode — expose ZeroZero's `ToolRegistry` over JSON-RPC 2.0.
//!
//! `McpServer` reads newline-delimited JSON-RPC 2.0 requests from an
//! async reader (stdio by default, or a TCP stream) and writes responses
//! to an async writer. It implements the minimal MCP surface required for
//! tool interoperability:
//!
//! - `initialize`            → protocol/capabilities/server info
//! - `notifications/initialized` (notification, no response)
//! - `tools/list`            → list of tools (name, description, input_schema)
//! - `tools/call`            → invoke a tool and return its text result
//!
//! ## Sibling contract
//!
//! The JSON shapes emitted here MUST match what the in-repo MCP **client**
//! (`crates/mcp/src/lib.rs`, `McpClient`) parses:
//!
//! - `tools/list` → `result.tools[]` with fields `name`, `description`,
//!   **`input_schema`** (snake_case — `McpTool` has no `serde rename`, so it
//!   expects `input_schema`, not the spec's camelCase `inputSchema`).
//! - `tools/call` → `result.content[]` of `{type:"text", text}` blocks and a
//!   boolean `result.isError`.
//!
//! This is a deliberate, documented deviation from the official MCP spec's
//! camelCase `inputSchema`, chosen so a `zz` server is consumable by a `zz`
//! client.

use std::sync::Arc;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use zerozero_sandbox::SandboxPolicy;

use crate::ToolRegistry;

/// MCP protocol version advertised in `initialize` — must match the
/// `PROTOCOL_VERSION` constant in `crates/mcp/src/lib.rs` (the client).
const PROTOCOL_VERSION: &str = "2024-11-05";

/// Server name reported in `initialize`.
const SERVER_NAME: &str = "zerozero";

/// An MCP server wrapping a `ToolRegistry`.
pub struct McpServer {
    registry: ToolRegistry,
}

impl McpServer {
    /// Create a server from a tool registry.
    pub const fn new(registry: ToolRegistry) -> Self {
        Self { registry }
    }

    /// Run on stdio: read from `tokio::io::stdin()`, write to
    /// `tokio::io::stdout()`. Blocks until stdin reaches EOF.
    pub async fn run(self) -> anyhow::Result<()> {
        let stdin = tokio::io::stdin();
        let stdout = tokio::io::stdout();
        self.run_on(BufReader::new(stdin), stdout).await
    }

    /// Run on an arbitrary async reader/writer (used for TCP, or tests).
    ///
    /// Reads one JSON-RPC request per line. Malformed lines are skipped
    /// (logged to stderr) and the loop continues — it never panics on bad
    /// input and always terminates on EOF (termination guarantee).
    pub async fn run_on<R, W>(self, reader: R, mut writer: W) -> anyhow::Result<()>
    where
        R: AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let mut lines = reader.lines();
        while let Some(line) = lines.next_line().await? {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let msg: serde_json::Value = match serde_json::from_str(trimmed) {
                Ok(m) => m,
                Err(e) => {
                    // skip malformed input, do not panic, keep looping.
                    eprintln!("mcp_server: skipping malformed JSON line: {e}");
                    continue;
                }
            };
            if let Some(response) = self.handle(msg).await {
                let mut out = serde_json::to_string(&response)?;
                out.push('\n');
                writer.write_all(out.as_bytes()).await?;
                writer.flush().await?;
            }
        }
        Ok(())
    }

    /// Optional TCP transport (best-effort, localhost-only, single client).
    ///
    /// Binds `127.0.0.1:port`, accepts the first connection, and serves it.
    /// This is the optional transport parity path; stdio is the supported
    /// default.
    pub async fn run_tcp(self, port: u16) -> anyhow::Result<()> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
        let (stream, _addr) = listener.accept().await?;
        let (reader, writer) = stream.into_split();
        self.run_on(BufReader::new(reader), writer).await
    }

    /// Handle a single parsed JSON-RPC message.
    ///
    /// Returns `Some(response)` to be written back, or `None` when the
    /// message is a notification (no `id`) and requires no response.
    async fn handle(&self, msg: serde_json::Value) -> Option<serde_json::Value> {
        let id = msg.get("id").cloned();
        let is_notification = id.is_none();
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");

        let result: Result<serde_json::Value, (i64, String)> = match method {
            "initialize" => Ok(serde_json::json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": SERVER_NAME,
                    "version": env!("CARGO_PKG_VERSION"),
                },
            })),
            "tools/list" => {
                let tools = self.registry.tools_snapshot();
                Ok(serde_json::json!({ "tools": tools }))
            }
            "tools/call" => {
                let params = msg.get("params");
                let name = params.and_then(|p| p.get("name")).and_then(|n| n.as_str());
                let args = params
                    .and_then(|p| p.get("arguments"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);

                match name {
                    None => Err((-32602, "missing tool name".to_string())),
                    Some(name) => match self.registry.get(name) {
                        None => Err((-32602, format!("unknown tool: {name}"))),
                        Some(tool) => {
                            match tool.execute(&args).await {
                                Ok(output) => Ok(serde_json::json!({
                                    "content": [{ "type": "text", "text": output }],
                                    "isError": false,
                                })),
                                // PRD AC5: tool failure is reported, not a
                                // protocol error — the server stays alive.
                                Err(e) => Ok(serde_json::json!({
                                    "content": [{ "type": "text", "text": e.to_string() }],
                                    "isError": true,
                                })),
                            }
                        }
                    },
                }
            }
            // Unknown method: if it's a notification, ignore it (no response).
            _ => {
                if is_notification {
                    return None;
                }
                Err((-32601, format!("method not found: {method}")))
            }
        };

        if is_notification {
            // Notifications (e.g. notifications/initialized) get no response.
            return None;
        }

        let id = id.unwrap_or(serde_json::Value::Null);
        let response = match result {
            Ok(res) => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": res,
            }),
            Err((code, message)) => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": code, "message": message },
            }),
        };
        Some(response)
    }
}

/// Build a `McpServer` pre-loaded with the standard tool registry
/// (full-access sandbox, isolated network namespace) — the default surface
/// exposed by `zz mcp serve`.
pub fn standard_server() -> McpServer {
    let registry = ToolRegistry::standard(Arc::new(SandboxPolicy::FullAccess));
    McpServer::new(registry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{BufReader as TokioBufReader, DuplexStream};

    /// Write a JSON value as a single newline-delimited frame.
    async fn send(tx: &mut DuplexStream, v: &serde_json::Value) {
        let mut s = serde_json::to_string(v).expect("serialize request");
        s.push('\n');
        tx.write_all(s.as_bytes()).await.expect("write request");
        tx.flush().await.expect("flush request");
    }

    /// Read one newline-delimited JSON response frame.
    async fn recv(rx: &mut DuplexStream) -> serde_json::Value {
        let mut reader = TokioBufReader::new(&mut *rx);
        let mut buf = String::new();
        let n = reader.read_line(&mut buf).await.expect("read response");
        assert!(n > 0, "expected a response line, got EOF");
        serde_json::from_str(buf.trim()).expect("parse response JSON")
    }

    fn make_server() -> McpServer {
        let reg = ToolRegistry::standard(Arc::new(SandboxPolicy::FullAccess));
        McpServer::new(reg)
    }

    #[tokio::test]
    async fn test_initialize() {
        let server = make_server();
        let (mut cli_req, srv_req) = tokio::io::duplex(4096);
        let (srv_resp, mut cli_resp) = tokio::io::duplex(4096);

        let task =
            tokio::spawn(
                async move { server.run_on(TokioBufReader::new(srv_req), srv_resp).await },
            );

        // initialize request.
        send(
            &mut cli_req,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {"name": "test", "version": "0"}
                }
            }),
        )
        .await;

        // notifications/initialized — must produce NO response line.
        send(
            &mut cli_req,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }),
        )
        .await;

        let resp = recv(&mut cli_resp).await;
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert!(resp["result"]["protocolVersion"].is_string(), "{resp}");
        assert!(resp["result"]["serverInfo"]["name"].is_string(), "{resp}");
        assert!(
            resp["result"]["serverInfo"]["version"].is_string(),
            "{resp}"
        );
        assert!(
            resp["result"]["capabilities"]["tools"].is_object(),
            "{resp}"
        );

        drop(cli_req);
        let _ = task.await;
    }

    #[tokio::test]
    async fn test_tools_list() {
        let server = make_server();
        let (mut cli_req, srv_req) = tokio::io::duplex(4096);
        let (srv_resp, mut cli_resp) = tokio::io::duplex(4096);

        let task =
            tokio::spawn(
                async move { server.run_on(TokioBufReader::new(srv_req), srv_resp).await },
            );

        send(
            &mut cli_req,
            &serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        )
        .await;

        let resp = recv(&mut cli_resp).await;
        assert_eq!(resp["id"], 2);
        let tools = &resp["result"]["tools"];
        assert!(tools.is_array(), "tools must be an array: {resp}");
        let tools = tools.as_array().expect("tools array");
        assert!(!tools.is_empty(), "expected at least one tool");

        // Each tool must carry name/description/input_schema (snake_case,
        // matching the in-repo MCP client's McpTool parse —).
        for t in tools {
            assert!(t["name"].is_string(), "tool missing name: {t}");
            assert!(
                t["description"].is_string(),
                "tool missing description: {t}"
            );
            assert!(
                t["input_schema"].is_object(),
                "tool must expose input_schema (snake_case): {t}"
            );
        }
        // read_file must be present (we exercise it in another test).
        assert!(
            tools.iter().any(|t| t["name"] == "read_file"),
            "read_file tool must be listed"
        );

        drop(cli_req);
        let _ = task.await;
    }

    #[tokio::test]
    async fn test_tools_call_read_file() {
        let server = make_server();
        let (mut cli_req, srv_req) = tokio::io::duplex(4096);
        let (srv_resp, mut cli_resp) = tokio::io::duplex(4096);

        let task =
            tokio::spawn(
                async move { server.run_on(TokioBufReader::new(srv_req), srv_resp).await },
            );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hello.txt");
        std::fs::write(&path, "FILECONTENT123").unwrap();

        send(
            &mut cli_req,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "read_file",
                    "arguments": { "path": path.to_str().unwrap() }
                }
            }),
        )
        .await;

        let resp = recv(&mut cli_resp).await;
        assert_eq!(resp["id"], 3);
        assert_eq!(resp["result"]["isError"], false);
        let content = &resp["result"]["content"];
        assert!(content.is_array(), "content must be array: {resp}");
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "FILECONTENT123\n");

        drop(cli_req);
        let _ = task.await;
    }

    #[tokio::test]
    async fn test_tools_call_unknown_tool() {
        let server = make_server();
        let (mut cli_req, srv_req) = tokio::io::duplex(4096);
        let (srv_resp, mut cli_resp) = tokio::io::duplex(4096);

        let task =
            tokio::spawn(
                async move { server.run_on(TokioBufReader::new(srv_req), srv_resp).await },
            );

        send(
            &mut cli_req,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": { "name": "does_not_exist", "arguments": {} }
            }),
        )
        .await;

        let resp = recv(&mut cli_resp).await;
        assert_eq!(resp["id"], 4);
        assert!(resp["error"].is_object(), "unknown tool must error: {resp}");
        assert_eq!(resp["error"]["code"], -32602);

        drop(cli_req);
        let _ = task.await;
    }

    #[tokio::test]
    async fn test_tools_call_error_iserror() {
        let server = make_server();
        let (mut cli_req, srv_req) = tokio::io::duplex(4096);
        let (srv_resp, mut cli_resp) = tokio::io::duplex(4096);

        let task =
            tokio::spawn(
                async move { server.run_on(TokioBufReader::new(srv_req), srv_resp).await },
            );

        // read_file on a missing path → tool returns Err → isError:true,
        // but the SERVER must stay alive and answer the next request.
        send(
            &mut cli_req,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": 5,
                "method": "tools/call",
                "params": { "name": "read_file", "arguments": { "path": "/no/such/file/xyz" } }
            }),
        )
        .await;

        let resp = recv(&mut cli_resp).await;
        assert_eq!(resp["id"], 5);
        assert_eq!(resp["result"]["isError"], true);
        assert!(resp["result"]["content"][0]["text"].is_string());

        // Server still alive: a follow-up tools/list must succeed.
        send(
            &mut cli_req,
            &serde_json::json!({"jsonrpc":"2.0","id":6,"method":"tools/list"}),
        )
        .await;
        let resp2 = recv(&mut cli_resp).await;
        assert_eq!(resp2["id"], 6);
        assert!(resp2["result"]["tools"].is_array());

        drop(cli_req);
        let _ = task.await;
    }

    #[tokio::test]
    async fn test_malformed_line_skipped() {
        let server = make_server();
        let (mut cli_req, srv_req) = tokio::io::duplex(4096);
        let (srv_resp, mut cli_resp) = tokio::io::duplex(4096);

        let task =
            tokio::spawn(
                async move { server.run_on(TokioBufReader::new(srv_req), srv_resp).await },
            );

        // Garbage line first — must be skipped, server keeps serving.
        cli_req.write_all(b"this is not json{{{\n").await.unwrap();
        cli_req.flush().await.unwrap();

        send(
            &mut cli_req,
            &serde_json::json!({"jsonrpc":"2.0","id":7,"method":"tools/list"}),
        )
        .await;

        let resp = recv(&mut cli_resp).await;
        assert_eq!(resp["id"], 7);
        assert!(resp["result"]["tools"].is_array());

        drop(cli_req);
        let _ = task.await;
    }
}
