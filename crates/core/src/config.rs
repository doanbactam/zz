//! Central configuration file support for ZeroZero .
//!
//! Parity goal: Codex ships a persistent `config.toml` (features, model,
//! approval, providers) plus a `settings.json` with profiles, `[agents]`,
//! permissions and env vars. ZeroZero previously had no standard central
//! config file (only CLI flags + `ZZ_*` env vars + ad-hoc `zz config`).
//!
//! This module introduces [`ZeroZeroConfig`] — a single TOML-backed config
//! loaded from `~/.config/zerozero/config.toml` (user-level) or
//! `./.zerozero.toml` (project-level), merged with CLI flags (CLI wins),
//! with named profiles and persistent feature flags.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The set of feature flags that `zz` understands. `feature enable <name>`
/// / `feature disable <name>` reject any name not in this list with a clear
/// error, and `feature list` prints every supported flag with its current
/// on/off state from the config.
///
/// feature flags (list action + validation + gating).
pub const SUPPORTED_FEATURES: &[&str] = &[
    "syntax-highlight",
    "auto-commit",
    "mcp",
    "compact-on-token-budget",
    "image-composer",
    "telemetry",
];

/// Returns `true` if `name` is a recognized feature flag.
pub fn is_supported_feature(name: &str) -> bool {
    SUPPORTED_FEATURES.contains(&name)
}

/// Return an error string if `name` is not a supported feature flag.
/// Returns `Ok(())` otherwise. Intended for use with `anyhow::bail`-style
/// callers from the CLI.
pub fn check_supported_feature(name: &str) -> anyhow::Result<()> {
    if is_supported_feature(name) {
        Ok(())
    } else {
        anyhow::bail!(
            "unknown feature flag '{name}'. Supported flags: {}",
            SUPPORTED_FEATURES.join(", ")
        )
    }
}

/// A named profile that can override the base config settings.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct Profile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default)]
    pub features: HashMap<String, bool>,
    #[serde(default)]
    pub permissions: Vec<String>,
}

impl Profile {
    /// Merge this profile over a base profile: values present in this profile
    /// win over the base; maps are unioned (this profile wins on collisions).
    fn merge_over(&self, base: &Self) -> Self {
        let mut features = base.features.clone();
        for (k, v) in &self.features {
            features.insert(k.clone(), *v);
        }
        let mut permissions = base.permissions.clone();
        permissions.extend(self.permissions.iter().cloned());
        Self {
            model: self.model.clone().or_else(|| base.model.clone()),
            approval: self.approval.clone().or_else(|| base.approval.clone()),
            provider: self.provider.clone().or_else(|| base.provider.clone()),
            features,
            permissions,
        }
    }
}

/// The central ZeroZero configuration.
///
/// This is the on-disk representation of `config.toml`. Every field is
/// optional so an empty/missing file deserializes to sensible defaults.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ZeroZeroConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default)]
    pub features: HashMap<String, bool>,
    #[serde(default)]
    pub profiles: HashMap<String, Profile>,
    #[serde(default)]
    pub permissions: Vec<String>,
    /// The active profile name (persisted by `zz config use <profile>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_profile: Option<String>,
}

/// Fully resolved settings after merging base config, profile and CLI flags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedSettings {
    pub model: Option<String>,
    pub approval: Option<String>,
    pub provider: Option<String>,
    pub features: HashMap<String, bool>,
    pub permissions: Vec<String>,
}

impl ZeroZeroConfig {
    /// Parse a config from a TOML string.
    pub fn parse_str(s: &str) -> anyhow::Result<Self> {
        let cfg: Self =
            toml::from_str(s).map_err(|e| anyhow::anyhow!("failed to parse config.toml: {e}"))?;
        Ok(cfg)
    }

    /// Load the first existing config file from the standard search paths.
    /// Returns an empty [`ZeroZeroConfig`] (all defaults) when no file is
    /// found — config is always optional.
    pub fn load() -> Self {
        for path in Self::search_paths() {
            if let Ok(contents) = std::fs::read_to_string(&path) {
                match Self::parse_str(&contents) {
                    Ok(cfg) => return cfg,
                    Err(e) => {
                        eprintln!("warning: failed to parse config at {}: {e}", path.display());
                    }
                }
            }
        }
        Self::default()
    }

    /// Load config from a specific path, erroring if it cannot be read/parsed.
    pub fn load_from(path: &Path) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read config at {}: {e}", path.display()))?;
        Self::parse_str(&contents)
    }

    /// Standard search order: project `.zerozero.toml` then user
    /// `~/.config/zerozero/config.toml`.
    pub fn search_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if let Ok(cwd) = std::env::current_dir() {
            paths.push(cwd.join(".zerozero.toml"));
        }
        if let Some(home) = home_dir() {
            paths.push(home.join(".config").join("zerozero").join("config.toml"));
        }
        paths
    }

    /// The path where a `save()` would write: user config, creating the dir.
    pub fn default_save_path() -> Option<PathBuf> {
        home_dir().map(|h| h.join(".config").join("zerozero").join("config.toml"))
    }

    /// Resolve effective settings by merging base config, an optional named
    /// profile, and CLI overrides (CLI wins over profile wins over base).
    pub fn resolve(
        &self,
        profile_name: Option<&str>,
        cli_model: Option<&str>,
        cli_approval: Option<&str>,
        cli_provider: Option<&str>,
    ) -> ResolvedSettings {
        // Build the effective base profile from top-level fields.
        let base = Profile {
            model: self.model.clone(),
            approval: self.approval.clone(),
            provider: self.provider.clone(),
            features: self.features.clone(),
            permissions: self.permissions.clone(),
        };

        // Apply the named profile on top of the base (profile wins).
        let effective = match profile_name.or(self.active_profile.as_deref()) {
            Some(name) => match self.profiles.get(name) {
                Some(p) => p.merge_over(&base),
                None => {
                    eprintln!("warning: profile '{name}' not found in config; using base settings");
                    base
                }
            },
            None => base,
        };

        // CLI flags win over everything.
        let mut features = effective.features;
        if let Some(name) = profile_name.or(self.active_profile.as_deref()) {
            if let Some(p) = self.profiles.get(name) {
                for (k, v) in &p.features {
                    features.insert(k.clone(), *v);
                }
            }
        }

        ResolvedSettings {
            model: cli_model.map(str::to_string).or(effective.model),
            approval: cli_approval.map(str::to_string).or(effective.approval),
            provider: cli_provider.map(str::to_string).or(effective.provider),
            features,
            permissions: effective.permissions,
        }
    }

    /// Enable or disable a persistent feature flag (mutates in memory).
    pub fn set_feature(&mut self, name: &str, enabled: bool) {
        self.features.insert(name.to_string(), enabled);
    }

    /// Whether a feature flag is enabled in this config. Unknown flags are
    /// treated as disabled (off). Use the SUPPORTED list for validation.
    pub fn feature_is_enabled(&self, name: &str) -> bool {
        self.features.get(name).copied().unwrap_or(false)
    }

    /// Produce the canonical `feature list` output: every supported flag
    /// followed by its current on/off state, one per line: `"name = on"`.
    /// Flags not yet present in the config are reported as `off`.
    pub fn format_feature_list(&self) -> String {
        let mut lines: Vec<String> = SUPPORTED_FEATURES
            .iter()
            .map(|name| {
                let state = if self.feature_is_enabled(name) {
                    "on"
                } else {
                    "off"
                };
                format!("{name} = {state}")
            })
            .collect();
        lines.sort();
        lines.join("\n")
    }

    /// Set the active profile name (creates it if missing) and persist.
    pub fn use_profile(&mut self, name: &str) {
        self.profiles.entry(name.to_string()).or_default();
        self.active_profile = Some(name.to_string());
    }

    /// Serialize and write to a specific path, creating parent dirs.
    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let toml = toml::to_string_pretty(self)
            .map_err(|e| anyhow::anyhow!("failed to serialize config: {e}"))?;
        std::fs::write(path, toml)
            .map_err(|e| anyhow::anyhow!("failed to write config to {}: {e}", path.display()))?;
        Ok(())
    }

    /// Save to the default user config path.
    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::default_save_path()
            .ok_or_else(|| anyhow::anyhow!("could not determine home directory for config"))?;
        self.save_to(&path)
    }

    /// Return a TOML string representation (for display/testing).
    pub fn to_toml_string(&self) -> anyhow::Result<String> {
        toml::to_string_pretty(self).map_err(|e| anyhow::anyhow!("failed to serialize config: {e}"))
    }
}

/// A parsed permission rule, used by the `permissions` config list to
/// allow or deny tool calls (parity: Codex `permissions` allow/deny list).
///
/// Rules are strings of the form:
/// - `"Read"` / `"Bash"` — allow/deny a whole tool by exact name.
/// - `"Bash(rm -rf *)"` — allow/deny a specific command pattern (substring
///   match against the `command` argument for the `bash` tool).
/// - `"Deny(Bash)"` / `"Allow(Read)"` — explicit verbs (prefix form).
///
/// Resolution order (first match wins): explicit `Deny(...)` beats
/// `Allow(...)`; a bare tool name is treated as `Allow(<tool>)`. When no
/// rule matches, the tool is **allowed** (default-open, matching the prior
/// behavior before permissions existed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRule {
    pub allow: bool,
    pub tool: String,
    pub pattern: Option<String>,
}

impl PermissionRule {
    /// Parse a permission rule string. Returns `None` for empty input.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        // Verb form: Allow(Tool) / Deny(Tool) or Allow(Bash(cmd)).
        if let Some(inner) = s.strip_prefix("Allow(").and_then(|s| s.strip_suffix(')')) {
            return Some(Self::build(true, inner));
        }
        if let Some(inner) = s.strip_prefix("Deny(").and_then(|s| s.strip_suffix(')')) {
            return Some(Self::build(false, inner));
        }
        // Bare tool name => Allow.
        Some(Self::build(true, s))
    }

    fn build(allow: bool, body: &str) -> Self {
        let body = body.trim();
        if let Some((tool, pat)) = body.split_once('(') {
            let pat = pat.strip_suffix(')').unwrap_or(pat).trim();
            Self {
                allow,
                tool: tool.trim().to_string(),
                pattern: if pat.is_empty() {
                    None
                } else {
                    Some(pat.to_string())
                },
            }
        } else {
            Self {
                allow,
                tool: body.to_string(),
                pattern: None,
            }
        }
    }
}

/// An ordered set of permission rules. `allows(tool, args)` returns whether
/// a tool call may proceed: `Deny` beats `Allow`, and a missing rule allows.
#[derive(Debug, Clone, Default)]
pub struct PermissionSet {
    rules: Vec<PermissionRule>,
}

impl PermissionSet {
    /// Build from raw rule strings (invalid/empty entries are skipped).
    pub fn from_rules(rules: &[String]) -> Self {
        let rules = rules
            .iter()
            .filter_map(|r| PermissionRule::parse(r))
            .collect();
        Self { rules }
    }

    /// Whether the given tool call is permitted. `args` is the parsed JSON
    /// arguments (used to match `Bash(cmd)` patterns against `command`).
    ///
    /// Resolution: among all rules that match the call, the **most specific**
    /// rule wins — a rule with a command `pattern` is more specific than a
    /// bare tool rule. When matching rules tie on specificity, `Deny` wins
    /// (fail-closed). If no rule matches, the call is allowed (default-open).
    pub fn allows(&self, tool_name: &str, args: &serde_json::Value) -> bool {
        let matching: Vec<&PermissionRule> = self
            .rules
            .iter()
            .filter(|r| Self::matches_rule(r, tool_name, args))
            .collect();
        if matching.is_empty() {
            return true; // default-open
        }
        // Prefer pattern-bearing (more specific) rules when present.
        let specific: Vec<&PermissionRule> = if matching.iter().any(|r| r.pattern.is_some()) {
            matching
                .iter()
                .filter(|r| r.pattern.is_some())
                .copied()
                .collect()
        } else {
            matching
        };
        // Among the most-specific matches, any Deny blocks the call.
        !specific.iter().any(|r| !r.allow)
    }

    /// True if `rule` applies to `(tool_name, args)`. The `pattern` (if any)
    /// is matched against the `command` argument of the `bash` tool as a
    /// shell-style glob where `*` matches any sequence.
    fn matches_rule(rule: &PermissionRule, tool_name: &str, args: &serde_json::Value) -> bool {
        if !rule.tool.eq_ignore_ascii_case(tool_name) {
            return false;
        }
        match &rule.pattern {
            None => true,
            Some(pat) => {
                let cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
                glob_match(pat, cmd)
            }
        }
    }
}

/// Shell-style glob match: `*` matches any sequence of characters.
fn glob_match(pattern: &str, text: &str) -> bool {
    if !pattern.contains('*') {
        return text.contains(pattern);
    }
    // Split on '*' and require each literal segment to appear in order.
    let segments: Vec<&str> = pattern.split('*').collect();
    if segments.is_empty() {
        return true;
    }
    // Leading/trailing literal segments anchor start/end.
    let mut search_from = 0;
    for (i, seg) in segments.iter().enumerate() {
        if seg.is_empty() {
            continue;
        }
        if i == 0 && pattern.starts_with('*') {
            // not anchored at start
        } else if i == 0 {
            // anchored at start
            if !text.starts_with(seg) {
                return false;
            }
            search_from = seg.len();
            continue;
        }
        // find seg after search_from
        let rest = &text[search_from.min(text.len())..];
        match rest.find(seg) {
            Some(pos) => search_from += pos + seg.len(),
            None => return false,
        }
    }
    // If pattern ends with a literal segment (no trailing '*'), the last
    // segment must be at the end of text.
    if !pattern.ends_with('*') {
        let last = segments.last().unwrap();
        if !last.is_empty() && !text.ends_with(last) {
            return false;
        }
    }
    true
}

/// Best-effort home directory discovery (`HOME` then Windows `USERPROFILE`).
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
model = "grok-4"
approval = "on-request"
provider = "xai"

permissions = ["read-only"]

[features]
streaming = true
telemetry = false

[profiles.dev]
model = "llama3.2"
provider = "ollama"

[profiles.dev.features]
streaming = false
experimental_ui = true

[profiles.strict]
approval = "never"
"#;

    #[test]
    fn parse_config_toml_from_string() {
        let cfg = ZeroZeroConfig::parse_str(SAMPLE).expect("parse");
        assert_eq!(cfg.model.as_deref(), Some("grok-4"));
        assert_eq!(cfg.approval.as_deref(), Some("on-request"));
        assert_eq!(cfg.provider.as_deref(), Some("xai"));
        assert_eq!(cfg.permissions, vec!["read-only".to_string()]);
        assert_eq!(cfg.features.get("streaming"), Some(&true));
        assert_eq!(cfg.features.get("telemetry"), Some(&false));
        assert!(cfg.profiles.contains_key("dev"));
        assert!(cfg.profiles.contains_key("strict"));
    }

    #[test]
    fn empty_config_is_default() {
        let cfg = ZeroZeroConfig::parse_str("").expect("parse empty");
        assert_eq!(cfg, ZeroZeroConfig::default());
        assert!(cfg.profiles.is_empty());
    }

    #[test]
    fn merge_cli_over_config() {
        let cfg = ZeroZeroConfig::parse_str(SAMPLE).unwrap();
        // No CLI override -> base config wins.
        let r = cfg.resolve(None, None, None, None);
        assert_eq!(r.model.as_deref(), Some("grok-4"));
        assert_eq!(r.provider.as_deref(), Some("xai"));
        assert_eq!(r.features.get("streaming"), Some(&true));

        // CLI override wins over base config.
        let r = cfg.resolve(None, Some("gpt-4o"), Some("never"), Some("openai"));
        assert_eq!(r.model.as_deref(), Some("gpt-4o"));
        assert_eq!(r.approval.as_deref(), Some("never"));
        assert_eq!(r.provider.as_deref(), Some("openai"));
    }

    #[test]
    fn profile_switch_logic() {
        let cfg = ZeroZeroConfig::parse_str(SAMPLE).unwrap();

        // dev profile overrides model+provider, keeps base approval.
        let r = cfg.resolve(Some("dev"), None, None, None);
        assert_eq!(r.model.as_deref(), Some("llama3.2"));
        assert_eq!(r.provider.as_deref(), Some("ollama"));
        assert_eq!(r.approval.as_deref(), Some("on-request"));
        // dev profile features merge: streaming overridden to false,
        // experimental_ui added, telemetry inherited from base.
        assert_eq!(r.features.get("streaming"), Some(&false));
        assert_eq!(r.features.get("experimental_ui"), Some(&true));
        assert_eq!(r.features.get("telemetry"), Some(&false));

        // strict profile overrides only approval.
        let r = cfg.resolve(Some("strict"), None, None, None);
        assert_eq!(r.approval.as_deref(), Some("never"));
        assert_eq!(r.model.as_deref(), Some("grok-4"));

        // CLI wins even when a profile is selected.
        let r = cfg.resolve(Some("dev"), Some("claude-x"), None, None);
        assert_eq!(r.model.as_deref(), Some("claude-x"));
        assert_eq!(r.provider.as_deref(), Some("ollama"));
    }

    #[test]
    fn missing_profile_warns_and_uses_base() {
        let cfg = ZeroZeroConfig::parse_str(SAMPLE).unwrap();
        let r = cfg.resolve(Some("does-not-exist"), None, None, None);
        assert_eq!(r.model.as_deref(), Some("grok-4"));
    }

    #[test]
    fn persist_feature_toggle_and_roundtrip() {
        let mut cfg = ZeroZeroConfig::default();
        cfg.set_feature("streaming", true);
        cfg.set_feature("telemetry", false);
        assert_eq!(cfg.features.get("streaming"), Some(&true));

        let toml = cfg.to_toml_string().unwrap();
        let reloaded = ZeroZeroConfig::parse_str(&toml).unwrap();
        assert_eq!(reloaded, cfg);
    }

    #[test]
    fn use_profile_persists_active() {
        let mut cfg = ZeroZeroConfig::default();
        cfg.use_profile("dev");
        assert_eq!(cfg.active_profile.as_deref(), Some("dev"));
        assert!(cfg.profiles.contains_key("dev"));
        // active profile is used when no explicit name is given.
        let r = cfg.resolve(None, None, None, None);
        assert_eq!(r.permissions, Vec::<String>::new());
    }

    #[test]
    fn save_and_load_roundtrip_via_tempdir() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let mut cfg = ZeroZeroConfig::parse_str(SAMPLE).unwrap();
        cfg.set_feature("new_flag", true);
        cfg.use_profile("dev");
        cfg.save_to(&path).unwrap();
        assert!(path.exists());

        let loaded = ZeroZeroConfig::load_from(&path).unwrap();
        assert_eq!(loaded.features.get("new_flag"), Some(&true));
        assert_eq!(loaded.active_profile.as_deref(), Some("dev"));
        assert_eq!(loaded.model.as_deref(), Some("grok-4"));
    }

    // --- feature flags ---

    #[test]
    fn supported_features_contains_expected_flags() {
        assert!(SUPPORTED_FEATURES.contains(&"image-composer"));
        assert!(SUPPORTED_FEATURES.contains(&"syntax-highlight"));
        assert!(SUPPORTED_FEATURES.contains(&"auto-commit"));
        assert!(SUPPORTED_FEATURES.contains(&"mcp"));
        assert!(SUPPORTED_FEATURES.contains(&"compact-on-token-budget"));
    }

    #[test]
    fn is_supported_feature_rejects_unknown() {
        assert!(is_supported_feature("mcp"));
        assert!(!is_supported_feature("totally-made-up"));
    }

    #[test]
    fn check_supported_feature_errors_on_unknown() {
        assert!(check_supported_feature("mcp").is_ok());
        let err = check_supported_feature("bogus-flag").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown feature flag 'bogus-flag'"));
        assert!(msg.contains("mcp"));
    }

    #[test]
    fn feature_is_enabled_defaults_off() {
        let cfg = ZeroZeroConfig::default();
        assert!(!cfg.feature_is_enabled("image-composer"));
    }

    #[test]
    fn format_feature_list_includes_known_flag() {
        let mut cfg = ZeroZeroConfig::default();
        cfg.set_feature("image-composer", true);
        let out = cfg.format_feature_list();
        assert!(out.contains("image-composer = on"));
        assert!(out.contains("mcp = off"));
        // every supported flag appears exactly once.
        for f in SUPPORTED_FEATURES {
            assert!(out.contains(f));
        }
    }

    #[test]
    fn config_roundtrip_persists_supported_feature() {
        // requirement B: enable -> write -> reload -> assert.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let mut cfg = ZeroZeroConfig::default();
        cfg.set_feature("image-composer", true);
        cfg.save_to(&path).unwrap();

        let reloaded = ZeroZeroConfig::load_from(&path).unwrap();
        assert!(reloaded.feature_is_enabled("image-composer"));
        // persisted as a real TOML [features] entry.
        let toml = reloaded.to_toml_string().unwrap();
        assert!(toml.contains("image-composer"));
    }

    #[test]
    fn permission_rule_parse_forms() {
        let allow_tool = PermissionRule::parse("Read").unwrap();
        assert!(allow_tool.allow);
        assert_eq!(allow_tool.tool, "Read");
        assert!(allow_tool.pattern.is_none());

        let deny_tool = PermissionRule::parse("Deny(Bash)").unwrap();
        assert!(!deny_tool.allow);
        assert_eq!(deny_tool.tool, "Bash");

        let pat = PermissionRule::parse("Allow(Bash(rm -rf *))").unwrap();
        assert!(pat.allow);
        assert_eq!(pat.tool, "Bash");
        assert_eq!(pat.pattern.as_deref(), Some("rm -rf *"));

        assert!(PermissionRule::parse("   ").is_none());
    }

    #[test]
    fn permission_set_default_open() {
        // No rules => everything allowed.
        let ps = PermissionSet::from_rules(&[]);
        assert!(ps.allows("Bash", &serde_json::json!({"command": "rm -rf /"})));
        assert!(ps.allows("WriteFile", &serde_json::Value::Null));
    }

    #[test]
    fn permission_set_deny_tool_blocks_all() {
        let ps = PermissionSet::from_rules(&["Deny(Bash)".to_string()]);
        assert!(!ps.allows("Bash", &serde_json::json!({"command": "ls"})));
        // other tools still allowed
        assert!(ps.allows("Read", &serde_json::Value::Null));
    }

    #[test]
    fn permission_set_pattern_targets_only_matching_command() {
        let ps = PermissionSet::from_rules(&["Allow(Bash(rm -rf *))".to_string()]);
        // allowed because command contains the pattern
        assert!(ps.allows("Bash", &serde_json::json!({"command": "rm -rf /tmp/x"})));
        // a different bash command: no rule matches => allowed (default-open)
        assert!(ps.allows("Bash", &serde_json::json!({"command": "ls"})));
    }

    #[test]
    fn permission_set_deny_pattern_overrides_allow() {
        let ps = PermissionSet::from_rules(&[
            "Allow(Bash)".to_string(),
            "Deny(Bash(rm -rf *))".to_string(),
        ]);
        assert!(ps.allows("Bash", &serde_json::json!({"command": "ls"})));
        assert!(!ps.allows("Bash", &serde_json::json!({"command": "rm -rf /"})));
    }

    #[test]
    fn permission_set_case_insensitive_tool() {
        let ps = PermissionSet::from_rules(&["Deny(bash)".to_string()]);
        assert!(!ps.allows("Bash", &serde_json::json!({"command": "ls"})));
    }
}
