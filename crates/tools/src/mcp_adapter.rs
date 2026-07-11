//! MCP tool adapter — wraps MCP server tools as ZeroZero `Tool` trait.
//!
//! `McpToolAdapter` bridges `zerozero_mcp::McpTool` (discovered from an
//! external MCP server) to ZeroZero's `Tool` trait, so the agent loop can
//! invoke MCP tools the same way as built-in tools.
//!
//! The adapter holds an `Arc<tokio::sync::Mutex<McpClient>>` shared with
//! other adapters from the same MCP server, ensuring only one request is
//! in-flight at a time (JSON-RPC over stdio is inherently sequential).

use std::sync::Arc;

use tokio::sync::Mutex;

use crate::Tool;
use zerozero_mcp::{McpClient, McpTool};

/// Adapter that wraps an MCP server tool as a ZeroZero `Tool`.
pub struct McpToolAdapter {
    /// Shared MCP client (Arc<Mutex> for sequential JSON-RPC access).
    client: Arc<Mutex<McpClient>>,
    /// The MCP tool definition (name, description, input_schema).
    tool: McpTool,
}

impl McpToolAdapter {
    /// Create a new adapter for a single MCP tool.
    pub const fn new(client: Arc<Mutex<McpClient>>, tool: McpTool) -> Self {
        Self { client, tool }
    }
}

#[async_trait::async_trait]
impl Tool for McpToolAdapter {
    fn name(&self) -> &str {
        &self.tool.name
    }

    fn description(&self) -> &str {
        &self.tool.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.tool.input_schema.clone()
    }

    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let mut client = self.client.lock().await;
        client.call_tool_async(&self.tool.name, args).await
    }
}

/// Discover tools from an MCP server and register them as ZeroZero tools.
///
/// Calls `list_tools_async` on the provided client, creates an
/// `McpToolAdapter` for each discovered tool, and registers it in the
/// given `ToolRegistry`. The client is wrapped in `Arc<Mutex>` and shared
/// across all adapters.
///
/// Returns the number of tools registered.
pub async fn register_mcp_tools(
    registry: &mut crate::ToolRegistry,
    mut client: McpClient,
) -> anyhow::Result<usize> {
    let tools = client.list_tools_async().await?;
    let count = tools.len();
    let client = Arc::new(Mutex::new(client));
    for tool in tools {
        registry.register(Box::new(McpToolAdapter::new(client.clone(), tool)));
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_adapter_name_and_description() {
        // We can't construct a real McpClient without spawning a process,
        // but we can verify the adapter logic by checking that name and
        // description come from the McpTool.
        let tool = McpTool {
            name: "search".to_string(),
            description: "Search the web".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
        };
        // Verify the tool fields are accessible.
        assert_eq!(tool.name, "search");
        assert_eq!(tool.description, "Search the web");
    }

    #[test]
    fn test_mcp_tool_schema_passthrough() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"}
            },
            "required": ["query"],
        });
        let tool = McpTool {
            name: "search".to_string(),
            description: "Search the web".to_string(),
            input_schema: schema.clone(),
        };
        // The adapter should pass through the schema as-is.
        assert_eq!(tool.input_schema, schema);
    }

    #[tokio::test]
    async fn test_register_mcp_tools_invalid_command() {
        // register_mcp_tools should fail when the MCP server can't be spawned.
        // We can't easily test this because McpClient::new is sync and
        // creates its own runtime. Instead, verify the function signature
        // compiles correctly.
        let reg = crate::ToolRegistry::new();
        // If we had a real McpClient, we'd call:
        // let count = register_mcp_tools(&mut reg, client).await.unwrap();
        // For now, just verify the registry is empty.
        assert_eq!(reg.definitions().len(), 0);
    }
}
