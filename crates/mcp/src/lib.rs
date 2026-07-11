//! MCP (Model Context Protocol) client.
//!
//! Connects to external MCP servers via JSON-RPC 2.0 over stdio.
//! Spawns the server as a child process, performs the `initialize`
//! handshake, discovers tools via `tools/list`, and can invoke tools
//! via `tools/call`.

use std::process::Stdio;

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// A tool exposed by an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpTool {
    /// The tool name, used when calling `tools/call`.
    pub name: String,
    /// Human-readable description of the tool.
    pub description: String,
    /// JSON Schema describing the tool's input parameters.
    pub input_schema: serde_json::Value,
}

/// MCP protocol version advertised during the `initialize` handshake.
const PROTOCOL_VERSION: &str = "2024-11-05";

/// Client info sent in the `initialize` request.
const CLIENT_NAME: &str = "zerozero-mcp";

/// A JSON-RPC 2.0 client for an MCP server spawned over stdio.
pub struct McpClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpClient {
    /// Spawn the MCP server `command` with `args`, then perform the
    /// JSON-RPC `initialize` handshake.
    ///
    /// Returns an error if the process cannot be spawned or the
    /// handshake fails.
    pub fn new(command: &str, args: &[String]) -> Result<Self> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());

        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow!("failed to spawn MCP server `{command}`: {e}"))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("MCP server did not expose a stdin pipe"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("MCP server did not expose a stdout pipe"))?;
        let stdout = BufReader::new(stdout);

        let mut client = Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        };

        // Perform the initialize handshake. This is a blocking async
        // operation; we drive it on a fresh runtime so that `new`
        // remains a synchronous entry point.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(client.initialize())?;

        Ok(client)
    }

    /// Send the `initialize` request followed by the
    /// `notifications/initialized` notification.
    async fn initialize(&mut self) -> Result<()> {
        let params = serde_json::json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {
                "name": CLIENT_NAME,
                "version": env!("CARGO_PKG_VERSION"),
            },
        });

        let _response = self.send_request("initialize", Some(params)).await?;

        // Notify the server that initialization is complete. This is a
        // notification (no id, no response expected).
        self.send_notification("notifications/initialized", serde_json::json!({}))
            .await?;

        Ok(())
    }

    /// List the tools exposed by the connected MCP server.
    pub fn list_tools(&mut self) -> Result<Vec<McpTool>> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(self.list_tools_async())
    }

    /// Async version of `list_tools` — can be called from within a tokio runtime.
    pub async fn list_tools_async(&mut self) -> Result<Vec<McpTool>> {
        let response = self
            .send_request("tools/list", Some(serde_json::json!({})))
            .await?;

        let tools = response
            .get("tools")
            .cloned()
            .unwrap_or(serde_json::Value::Array(vec![]));

        let tools: Vec<McpTool> = serde_json::from_value(tools)
            .map_err(|e| anyhow!("failed to parse tools/list response: {e}"))?;

        Ok(tools)
    }

    /// Invoke a tool by name with the given JSON arguments.
    ///
    /// Returns the textual result of the tool call.
    pub fn call_tool(&mut self, name: &str, args: &serde_json::Value) -> Result<String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(self.call_tool_async(name, args))
    }

    /// Async version of `call_tool` — can be called from within a tokio runtime.
    pub async fn call_tool_async(
        &mut self,
        name: &str,
        args: &serde_json::Value,
    ) -> Result<String> {
        let params = serde_json::json!({
            "name": name,
            "arguments": args,
        });

        let response = self.send_request("tools/call", Some(params)).await?;

        // The result content is an array of content blocks. Concatenate
        // any text blocks into a single string.
        let content = response
            .get("content")
            .cloned()
            .unwrap_or(serde_json::Value::Array(vec![]));

        let blocks: Vec<serde_json::Value> = serde_json::from_value(content)
            .map_err(|e| anyhow!("failed to parse tools/call content: {e}"))?;

        let mut text = String::new();
        for block in blocks {
            if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(s) = block.get("text").and_then(|t| t.as_str()) {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(s);
                }
            }
        }

        Ok(text)
    }

    /// Send a `shutdown` request and kill the child process.
    pub fn close(&mut self) {
        // Best-effort shutdown request; ignore errors since the server
        // may already be gone.
        self.send_shutdown();
        // Kill the child process if it is still running.
        let _ = self.child.start_kill();
    }

    fn send_shutdown(&mut self) {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(_) => return,
        };
        let _ = rt.block_on(self.send_request("shutdown", None));
    }

    /// Build a JSON-RPC 2.0 request object with the next monotonically
    /// increasing id.
    fn build_request(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> serde_json::Value {
        let id = self.next_id;
        self.next_id += 1;

        let mut request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
        });
        if let Some(p) = params {
            request["params"] = p;
        }
        request
    }

    /// Send a JSON-RPC request and read the matching response.
    async fn send_request(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let request = self.build_request(method, params);
        let id = request
            .get("id")
            .cloned()
            .ok_or_else(|| anyhow!("internal error: request missing id"))?;

        let mut line = serde_json::to_string(&request)?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;

        self.read_response(id).await
    }

    /// Send a JSON-RPC notification (no id, no response expected).
    async fn send_notification(&mut self, method: &str, params: serde_json::Value) -> Result<()> {
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });

        let mut line = serde_json::to_string(&notification)?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    /// Read lines from stdout until a response matching `id` is found.
    /// Non-matching messages (notifications, unrelated responses) are
    /// skipped.
    async fn read_response(&mut self, id: serde_json::Value) -> Result<serde_json::Value> {
        loop {
            let mut buf = String::new();
            let n = self.stdout.read_line(&mut buf).await?;
            if n == 0 {
                bail!("MCP server closed stdout before responding to request {id}");
            }

            let trimmed = buf.trim();
            if trimmed.is_empty() {
                continue;
            }

            let message: serde_json::Value = serde_json::from_str(trimmed)
                .map_err(|e| anyhow!("failed to parse JSON-RPC message: {e}"))?;

            // Skip notifications (no id).
            if message.get("id").is_none() {
                continue;
            }

            if message.get("id") != Some(&id) {
                // Not the response we are waiting for; skip.
                continue;
            }

            if let Some(err) = message.get("error") {
                bail!("JSON-RPC error for request {id}: {err}");
            }

            let result = message
                .get("result")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            return Ok(result);
        }
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        // Ensure the child process is terminated when the client is
        // dropped. `start_kill` is synchronous and sends SIGKILL.
        let _ = self.child.start_kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_tool_serialization() {
        let tool = McpTool {
            name: "search".to_string(),
            description: "Search the web".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" }
                },
                "required": ["query"],
            }),
        };

        let json = serde_json::to_string(&tool).expect("serialize");
        let deserialized: McpTool = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(tool, deserialized);
    }

    #[test]
    fn test_jsonrpc_request_format() {
        // We cannot construct an McpClient without spawning a process,
        // so verify the request shape via a standalone helper that
        // mirrors `build_request`'s logic.
        let mut next_id: u64 = 1;

        let build = |next_id: &mut u64, method: &str, params: Option<serde_json::Value>| {
            let id = *next_id;
            *next_id += 1;
            let mut request = serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
            });
            if let Some(p) = params {
                request["params"] = p;
            }
            request
        };

        // Without params.
        let req = build(&mut next_id, "tools/list", None);
        assert_eq!(req["jsonrpc"], "2.0");
        assert_eq!(req["id"], 1);
        assert_eq!(req["method"], "tools/list");
        assert!(req.get("params").is_none());

        // With params.
        let req = build(
            &mut next_id,
            "tools/call",
            Some(serde_json::json!({ "name": "echo" })),
        );
        assert_eq!(req["jsonrpc"], "2.0");
        assert_eq!(req["id"], 2);
        assert_eq!(req["method"], "tools/call");
        assert_eq!(req["params"]["name"], "echo");

        // Id increments monotonically.
        let req = build(&mut next_id, "shutdown", None);
        assert_eq!(req["id"], 3);
    }

    #[test]
    fn test_mcp_client_new_with_invalid_command() {
        let result = McpClient::new("nonexistent_cmd_xyz", &[]);
        let err = match result {
            Ok(_) => panic!("expected an error for an invalid command"),
            Err(e) => e.to_string(),
        };

        assert!(
            err.contains("nonexistent_cmd_xyz"),
            "error should mention the failed command: {err}"
        );
    }
}
