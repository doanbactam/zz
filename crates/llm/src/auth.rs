//! Persistent API-key store (OpenCode `auth.json` parity).
//!
//! Keys are stored at `~/.config/zerozero/auth.json` (override with
//! `ZZ_AUTH_PATH`). Resolution order for a provider key:
//!
//! 1. Process environment (`XAI_API_KEY`, …) — highest priority
//! 2. Entry in the auth store for that provider id
//! 3. (xAI only) legacy `OPENAI_API_KEY` env fallback
//!
//! Never write secrets to logs; callers should only print "set"/"not set".

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::providers::{self, ProviderSpec};

/// One credential entry (OpenCode-compatible shape).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthEntry {
    /// Credential type. Currently only `"api"`.
    #[serde(rename = "type", default = "default_type")]
    pub kind: String,
    /// Secret API key / token.
    pub key: String,
}

fn default_type() -> String {
    "api".to_string()
}

impl AuthEntry {
    pub fn api(key: impl Into<String>) -> Self {
        Self {
            kind: "api".to_string(),
            key: key.into(),
        }
    }
}

/// Map of provider id → credential.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthStore {
    #[serde(flatten)]
    pub entries: HashMap<String, AuthEntry>,
}

impl AuthStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load from disk. Missing file → empty store. Invalid JSON → error.
    pub fn load_from(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::new());
        }
        let raw = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read auth store {}: {e}", path.display()))?;
        if raw.trim().is_empty() {
            return Ok(Self::new());
        }
        let store: Self = serde_json::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("failed to parse auth store {}: {e}", path.display()))?;
        Ok(store)
    }

    /// Load from the default / `ZZ_AUTH_PATH` location.
    pub fn load() -> anyhow::Result<Self> {
        Self::load_from(&auth_path())
    }

    /// Persist to `path`, creating parent directories.
    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                anyhow::anyhow!("failed to create auth dir {}: {e}", parent.display())
            })?;
        }
        let raw = serde_json::to_string_pretty(self)
            .map_err(|e| anyhow::anyhow!("failed to serialize auth store: {e}"))?;
        std::fs::write(path, raw)
            .map_err(|e| anyhow::anyhow!("failed to write auth store {}: {e}", path.display()))?;
        // Best-effort: restrict permissions on Unix (0600).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    /// Save to the default / `ZZ_AUTH_PATH` location.
    pub fn save(&self) -> anyhow::Result<()> {
        self.save_to(&auth_path())
    }

    /// Stored key for `provider_id`, if non-empty.
    pub fn get(&self, provider_id: &str) -> Option<&str> {
        self.entries
            .get(provider_id)
            .map(|e| e.key.as_str())
            .filter(|k| !k.is_empty())
    }

    /// Insert or replace a key for `provider_id`.
    pub fn set(&mut self, provider_id: &str, key: impl Into<String>) {
        self.entries
            .insert(provider_id.to_string(), AuthEntry::api(key));
    }

    /// Remove a provider's credential. Returns true if it existed.
    pub fn remove(&mut self, provider_id: &str) -> bool {
        self.entries.remove(provider_id).is_some()
    }

    /// Whether the store has a non-empty key for this provider.
    pub fn has(&self, provider_id: &str) -> bool {
        self.get(provider_id).is_some()
    }
}

/// Path to the auth store.
///
/// Priority:
/// 1. `ZZ_AUTH_PATH` (tests / custom layout)
/// 2. `$HOME/.config/zerozero/auth.json` or `%USERPROFILE%\.config\zerozero\auth.json`
/// 3. Fallback: `./.zerozero/auth.json` (cwd)
pub fn auth_path() -> PathBuf {
    if let Ok(p) = std::env::var("ZZ_AUTH_PATH") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    if let Some(home) = home_dir() {
        return home.join(".config").join("zerozero").join("auth.json");
    }
    PathBuf::from(".zerozero").join("auth.json")
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Resolve an API key for `provider_type` (env → auth store → legacy fallback).
///
/// Returns `Ok(Some(key))`, `Ok(None)` when key is optional and missing,
/// or empty string when required but missing (caller validates).
pub fn resolve_api_key(provider_type: &str) -> String {
    let spec = providers::provider_spec(provider_type);
    resolve_api_key_for_spec(spec)
}

/// Resolve key using an explicit [`ProviderSpec`].
pub fn resolve_api_key_for_spec(spec: &ProviderSpec) -> String {
    // 1. Primary env var
    if !spec.api_key_env.is_empty() {
        if let Ok(v) = std::env::var(spec.api_key_env) {
            if !v.is_empty() {
                return v;
            }
        }
    }

    // 2. Auth store (OpenCode parity)
    if let Ok(store) = AuthStore::load() {
        if let Some(k) = store.get(spec.id) {
            return k.to_string();
        }
    }

    // 3. xAI legacy: OPENAI_API_KEY as last-resort env fallback only for xai
    if spec.id == "xai" {
        if let Ok(v) = std::env::var("OPENAI_API_KEY") {
            if !v.is_empty() {
                return v;
            }
        }
    }

    // 4. Gemini also accepts GOOGLE_API_KEY (common convention)
    if spec.id == "gemini" {
        if let Ok(v) = std::env::var("GOOGLE_API_KEY") {
            if !v.is_empty() {
                return v;
            }
        }
    }

    String::new()
}

/// Resolve base URL: env override → default from spec.
pub fn resolve_base_url(spec: &ProviderSpec) -> String {
    if !spec.base_url_env.is_empty() {
        if let Ok(v) = std::env::var(spec.base_url_env) {
            if !v.is_empty() {
                return v;
            }
        }
    }
    spec.default_base_url.to_string()
}

/// Resolve model: override → `ZZ_MODEL` → provider default.
pub fn resolve_model(spec: &ProviderSpec, model_override: Option<String>) -> String {
    if let Some(m) = model_override {
        if !m.is_empty() {
            return m;
        }
    }
    std::env::var("ZZ_MODEL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| spec.default_model.to_string())
}

/// Whether a usable key exists for this provider (env or auth store).
pub fn has_api_key(provider_type: &str) -> bool {
    let spec = providers::provider_spec(provider_type);
    if !spec.requires_key {
        return true;
    }
    !resolve_api_key_for_spec(spec).is_empty()
}

/// Source label for doctor/diagnostics (does not reveal the secret).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    Env,
    AuthStore,
    LegacyFallback,
    Missing,
    NotRequired,
}

/// Where the key would come from (without returning the secret).
pub fn key_source(provider_type: &str) -> KeySource {
    let spec = providers::provider_spec(provider_type);
    if !spec.requires_key {
        return KeySource::NotRequired;
    }
    if !spec.api_key_env.is_empty() {
        if let Ok(v) = std::env::var(spec.api_key_env) {
            if !v.is_empty() {
                return KeySource::Env;
            }
        }
    }
    if let Ok(store) = AuthStore::load() {
        if store.has(spec.id) {
            return KeySource::AuthStore;
        }
    }
    if spec.id == "xai" {
        if let Ok(v) = std::env::var("OPENAI_API_KEY") {
            if !v.is_empty() {
                return KeySource::LegacyFallback;
            }
        }
    }
    if spec.id == "gemini" {
        if let Ok(v) = std::env::var("GOOGLE_API_KEY") {
            if !v.is_empty() {
                return KeySource::Env;
            }
        }
    }
    KeySource::Missing
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Serialize env-mutating tests.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn auth_store_roundtrip() {
        let dir = std::env::temp_dir().join(format!(
            "zz-auth-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("auth.json");

        let mut store = AuthStore::new();
        store.set("xai", "xai-secret");
        store.set("openai", "sk-test");
        store.save_to(&path).unwrap();

        let mut loaded = AuthStore::load_from(&path).unwrap();
        assert_eq!(loaded.get("xai"), Some("xai-secret"));
        assert_eq!(loaded.get("openai"), Some("sk-test"));
        assert!(loaded.remove("openai"));
        assert!(!loaded.has("openai"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_prefers_env_over_auth() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!(
            "zz-auth-env-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("auth.json");
        let mut store = AuthStore::new();
        store.set("xai", "from-auth");
        store.save_to(&path).unwrap();

        // SAFETY: single-threaded test under mutex
        unsafe {
            std::env::set_var("ZZ_AUTH_PATH", &path);
            std::env::set_var("XAI_API_KEY", "from-env");
            std::env::remove_var("OPENAI_API_KEY");
        }
        assert_eq!(resolve_api_key("xai"), "from-env");
        unsafe {
            std::env::remove_var("XAI_API_KEY");
        }
        assert_eq!(resolve_api_key("xai"), "from-auth");
        unsafe {
            std::env::remove_var("ZZ_AUTH_PATH");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn openai_does_not_use_xai_key() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!(
            "zz-auth-iso-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("empty-auth.json");
        // non-existent path → empty store
        unsafe {
            std::env::set_var("ZZ_AUTH_PATH", &path);
            std::env::set_var("XAI_API_KEY", "xai-only");
            std::env::remove_var("OPENAI_API_KEY");
        }
        assert!(resolve_api_key("openai").is_empty());
        assert_eq!(resolve_api_key("xai"), "xai-only");
        unsafe {
            std::env::remove_var("XAI_API_KEY");
            std::env::remove_var("ZZ_AUTH_PATH");
        }
    }
}
