//! Plugin system for ZeroZero — external tool plugins via stdio JSON-RPC.
//!
//! A plugin is a command (script/binary) that:
//! 1. Receives a JSON request on stdin: {"method": "execute", "params": {...}}
//! 2. Returns a JSON response on stdout: {"result": "..."} or {"error": "..."}
//!
//! Plugins are registered in a config file and loaded as Tool implementations.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;

/// Plugin configuration — loaded from a config file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginConfig {
    pub name: String,
    pub description: String,
    pub command: String,   // e.g. "python3" or "./my-plugin.sh"
    pub args: Vec<String>, // e.g. ["plugin.py"]
    #[serde(default)]
    pub parameters_schema: serde_json::Value,
}

impl Default for PluginConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            command: String::new(),
            args: Vec::new(),
            parameters_schema: serde_json::json!({"type": "object", "properties": {}}),
        }
    }
}

/// Plugin config file format — a list of plugins.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginsFile {
    pub plugins: Vec<PluginConfig>,
}

/// A plugin tool — implements zerozero_tools::Tool by calling the external command.
pub struct PluginTool {
    config: PluginConfig,
}

impl PluginTool {
    pub const fn new(config: PluginConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl zerozero_tools::Tool for PluginTool {
    fn name(&self) -> &str {
        &self.config.name
    }

    fn description(&self) -> &str {
        &self.config.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.config.parameters_schema.clone()
    }

    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        // Spawn the command, send JSON request on stdin, read response from stdout.
        // Request format: {"method": "execute", "params": <args>}
        // Response format: {"result": "..."} or {"error": "..."}
        let request = serde_json::json!({
            "method": "execute",
            "params": args,
        });

        let mut child = tokio::process::Command::new(&self.config.command)
            .args(&self.config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| anyhow::anyhow!("failed to spawn plugin '{}': {e}", self.config.name))?;

        // Write request to stdin
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            let request_bytes = serde_json::to_vec(&request)?;
            stdin.write_all(&request_bytes).await?;
            stdin.shutdown().await?;
        }

        let output = child
            .wait_with_output()
            .await
            .map_err(|e| anyhow::anyhow!("plugin '{}' failed: {e}", self.config.name))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "plugin '{}' exited with {}: {stderr}",
                self.config.name,
                output.status
            );
        }

        // Parse response
        let stdout = String::from_utf8_lossy(&output.stdout);
        let response: serde_json::Value = serde_json::from_str(stdout.trim()).map_err(|e| {
            anyhow::anyhow!("plugin '{}' returned invalid JSON: {e}", self.config.name)
        })?;

        if let Some(error) = response.get("error").and_then(|v| v.as_str()) {
            anyhow::bail!("plugin '{}' error: {error}", self.config.name);
        }

        let result = response
            .get("result")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| stdout.trim());
        Ok(result.to_string())
    }
}

/// Load plugins from a config file (TOML or JSON).
/// Config file path: `.zerozero/plugins.json` or `.zerozero/plugins.toml`
pub fn load_plugins_config(config_path: &std::path::Path) -> anyhow::Result<PluginsFile> {
    let content = std::fs::read_to_string(config_path)
        .map_err(|e| anyhow::anyhow!("failed to read plugins config: {e}"))?;
    // Try JSON first, then TOML
    if config_path
        .extension()
        .map(|e| e == "json")
        .unwrap_or(false)
    {
        let file: PluginsFile = serde_json::from_str(&content)?;
        Ok(file)
    } else {
        // Parse simple TOML: [[plugins]] sections
        parse_toml_plugins(&content)
    }
}

/// Simple TOML parser for [[plugins]] sections.
/// Each section has: name, description, command, args (array), parameters_schema (inline JSON).
fn parse_toml_plugins(content: &str) -> anyhow::Result<PluginsFile> {
    // Parse line by line. Look for [[plugins]] sections.
    // Within each section, parse key = value pairs.
    // args is an array: ["arg1", "arg2"]
    // parameters_schema is a JSON string.
    // This is a simplified parser — not a full TOML parser.
    let mut plugins = Vec::new();
    let mut current: Option<PluginConfig> = None;
    let mut current_args: Vec<String> = Vec::new();
    let mut in_args_array = false;

    for line in content.lines() {
        let line = line.trim();
        if line == "[[plugins]]" {
            if let Some(p) = current.take() {
                plugins.push(p);
            }
            current = Some(PluginConfig::default());
            current_args.clear();
            in_args_array = false;
            continue;
        }
        if line.starts_with('[') {
            // other section
            if let Some(p) = current.take() {
                let mut p = p;
                p.args = current_args.clone();
                plugins.push(p);
            }
            in_args_array = false;
            continue;
        }
        let Some(cfg) = current.as_mut() else {
            continue;
        };
        if in_args_array {
            if line.ends_with(']') {
                in_args_array = false;
                let val = line.trim_end_matches(']').trim().trim_matches('"');
                if !val.is_empty() {
                    current_args.push(val.to_string());
                }
                cfg.args = current_args.clone();
                current_args.clear();
            } else {
                let val = line.trim().trim_matches(',').trim().trim_matches('"');
                if !val.is_empty() {
                    current_args.push(val.to_string());
                }
            }
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let value = value.trim();
            match key {
                "name" => cfg.name = value.trim_matches('"').to_string(),
                "description" => cfg.description = value.trim_matches('"').to_string(),
                "command" => cfg.command = value.trim_matches('"').to_string(),
                "args" => {
                    if value.starts_with('[') {
                        if value.ends_with(']') {
                            // single line array
                            let inner = value.trim_start_matches('[').trim_end_matches(']').trim();
                            if !inner.is_empty() {
                                for v in inner.split(',') {
                                    let v = v.trim().trim_matches('"');
                                    if !v.is_empty() {
                                        current_args.push(v.to_string());
                                    }
                                }
                                cfg.args = current_args.clone();
                                current_args.clear();
                            }
                        } else {
                            in_args_array = true;
                        }
                    }
                }
                "parameters_schema" => {
                    if let Ok(schema) =
                        serde_json::from_str::<serde_json::Value>(value.trim_matches('"'))
                    {
                        cfg.parameters_schema = schema;
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(mut p) = current {
        if !current_args.is_empty() {
            p.args = current_args;
        }
        plugins.push(p);
    }
    Ok(PluginsFile { plugins })
}

/// Register plugins from config into a ToolRegistry.
pub fn register_plugins(
    registry: &mut zerozero_tools::ToolRegistry,
    config: &PluginsFile,
) -> usize {
    let mut count = 0;
    for plugin in &config.plugins {
        registry.register(Box::new(PluginTool::new(plugin.clone())));
        count += 1;
    }
    count
}

/// Discover plugin config in standard locations.
/// Returns the first found path.
pub fn discover_config() -> Option<PathBuf> {
    let candidates = [
        PathBuf::from(".zerozero/plugins.json"),
        PathBuf::from(".zerozero/plugins.toml"),
    ];
    candidates.into_iter().find(|p| p.exists())
}

/// Auto-scan a directory for individual plugin config files.
/// Each file (JSON or TOML) represents one plugin. Returns all discovered configs.
/// If `base_dir` is None, uses current directory.
pub fn discover_plugins_dir(base_dir: Option<&std::path::Path>) -> Vec<PluginConfig> {
    let base = base_dir.unwrap_or_else(|| std::path::Path::new("."));
    let dir = base.join(".zerozero/plugins");
    let mut plugins = Vec::new();

    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return plugins,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext != "json" && ext != "toml" {
            continue;
        }
        match load_plugin_config_file(&path) {
            Ok(cfg) => plugins.push(cfg),
            Err(e) => {
                eprintln!("warning: failed to load plugin {}: {e}", path.display());
            }
        }
    }

    plugins
}

/// Load a single plugin config from a file (JSON or TOML).
fn load_plugin_config_file(path: &std::path::Path) -> anyhow::Result<PluginConfig> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;

    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ext == "json" {
        // Single plugin JSON: {"name": "...", "command": "...", ...}
        let cfg: PluginConfig = serde_json::from_str(&content)?;
        Ok(cfg)
    } else {
        // TOML: [[plugins]] section with one plugin
        let file = parse_toml_plugins(&content)?;
        file.plugins
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("no [[plugins]] section in {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zerozero_tools::Tool;

    #[test]
    fn test_parse_toml_plugins() {
        let toml = r#"
[[plugins]]
name = "echo"
description = "Echo plugin"
command = "echo"
args = ["hello"]

[[plugins]]
name = "date"
description = "Date plugin"
command = "date"
"#;
        let file = parse_toml_plugins(toml).unwrap();
        assert_eq!(file.plugins.len(), 2);
        assert_eq!(file.plugins[0].name, "echo");
        assert_eq!(file.plugins[0].command, "echo");
        assert_eq!(file.plugins[0].args, vec!["hello"]);
        assert_eq!(file.plugins[1].name, "date");
    }

    #[test]
    fn test_parse_json_plugins() {
        let json = r#"{"plugins":[{"name":"test","description":"Test","command":"echo","args":[],"parameters_schema":{"type":"object","properties":{}}}]}"#;
        let file: PluginsFile = serde_json::from_str(json).unwrap();
        assert_eq!(file.plugins.len(), 1);
        assert_eq!(file.plugins[0].name, "test");
    }

    #[tokio::test]
    async fn test_plugin_tool_execute() {
        // Use a simple shell command as plugin that reads JSON and returns result
        let config = PluginConfig {
            name: "echo-plugin".to_string(),
            description: "Echoes back".to_string(),
            command: "sh".to_string(),
            args: vec!["-c".to_string(), r#"echo '{"result":"ok"}'"#.to_string()],
            parameters_schema: serde_json::json!({"type":"object"}),
        };
        let tool = PluginTool::new(config);
        let result = tool.execute(&serde_json::json!({})).await.unwrap();
        assert_eq!(result, "ok");
    }

    #[test]
    fn test_load_plugins_config_nonexistent() {
        let result = load_plugins_config(std::path::Path::new("/nonexistent/plugins.toml"));
        assert!(result.is_err());
    }

    #[test]
    fn test_discover_plugins_dir_empty() {
        let tmp = std::env::temp_dir().join("zz_test_discover_empty");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join(".zerozero/plugins")).unwrap();

        let plugins = discover_plugins_dir(Some(&tmp));
        assert_eq!(plugins.len(), 0);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_discover_plugins_dir_with_json() {
        let tmp = std::env::temp_dir().join("zz_test_discover_json");
        let _ = std::fs::remove_dir_all(&tmp);
        let plugins_dir = tmp.join(".zerozero/plugins");
        std::fs::create_dir_all(&plugins_dir).unwrap();

        let cfg = serde_json::json!({
            "name": "test-discovered",
            "description": "Discovered plugin",
            "command": "echo",
            "args": ["hi"],
            "parameters_schema": {"type": "object", "properties": {}}
        });
        std::fs::write(plugins_dir.join("my-plugin.json"), cfg.to_string()).unwrap();

        let plugins = discover_plugins_dir(Some(&tmp));
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "test-discovered");
        assert_eq!(plugins[0].command, "echo");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_discover_plugins_dir_with_toml() {
        let tmp = std::env::temp_dir().join("zz_test_discover_toml");
        let _ = std::fs::remove_dir_all(&tmp);
        let plugins_dir = tmp.join(".zerozero/plugins");
        std::fs::create_dir_all(&plugins_dir).unwrap();

        let toml = r#"
[[plugins]]
name = "toml-plugin"
description = "From TOML"
command = "date"
"#;
        std::fs::write(plugins_dir.join("dated.toml"), toml).unwrap();

        let plugins = discover_plugins_dir(Some(&tmp));
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "toml-plugin");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_discover_plugins_dir_skips_non_config() {
        let tmp = std::env::temp_dir().join("zz_test_discover_skip");
        let _ = std::fs::remove_dir_all(&tmp);
        let plugins_dir = tmp.join(".zerozero/plugins");
        std::fs::create_dir_all(&plugins_dir).unwrap();

        std::fs::write(plugins_dir.join("readme.txt"), "not a plugin").unwrap();
        let cfg = serde_json::json!({
            "name": "valid",
            "description": "Valid",
            "command": "echo",
            "args": []
        });
        std::fs::write(plugins_dir.join("valid.json"), cfg.to_string()).unwrap();

        let plugins = discover_plugins_dir(Some(&tmp));
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "valid");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
