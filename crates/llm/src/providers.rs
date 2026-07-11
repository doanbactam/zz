//! Built-in LLM provider registry (OpenCode-inspired).
//!
//! Single source of truth for provider id, API-key env, base URL, default
//! model, and wire protocol kind. CLI key resolution, `zz doctor`, and the
//! TUI model catalog all derive from this list.

/// How a provider speaks on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    /// OpenAI Chat Completions + SSE (`/chat/completions`).
    OpenAiCompat,
    /// Anthropic Messages API.
    Anthropic,
    /// Google Gemini `streamGenerateContent`.
    Gemini,
    /// Local OpenAI-compatible (key optional).
    Local,
}

/// Static description of a built-in provider.
#[derive(Debug, Clone, Copy)]
pub struct ProviderSpec {
    /// Canonical id (`ZZ_PROVIDER` / `--provider`).
    pub id: &'static str,
    /// Human-readable name.
    pub name: &'static str,
    /// Primary API key environment variable (empty = not required).
    pub api_key_env: &'static str,
    /// Optional base-URL override env var.
    pub base_url_env: &'static str,
    /// Default base URL (empty for Gemini native, which ignores it).
    pub default_base_url: &'static str,
    /// Default model when `ZZ_MODEL` is unset.
    pub default_model: &'static str,
    /// Wire protocol.
    pub kind: ProviderKind,
    /// Whether a non-empty API key is required to call the provider.
    pub requires_key: bool,
}

/// Built-in providers (display / resolution order).
pub const PROVIDERS: &[ProviderSpec] = &[
    ProviderSpec {
        id: "xai",
        name: "xAI (Grok)",
        api_key_env: "XAI_API_KEY",
        base_url_env: "XAI_BASE_URL",
        default_base_url: "https://api.x.ai/v1",
        default_model: "grok-4",
        kind: ProviderKind::OpenAiCompat,
        requires_key: true,
    },
    ProviderSpec {
        id: "openai",
        name: "OpenAI",
        api_key_env: "OPENAI_API_KEY",
        base_url_env: "OPENAI_BASE_URL",
        default_base_url: "https://api.openai.com/v1",
        default_model: "gpt-4o-mini",
        kind: ProviderKind::OpenAiCompat,
        requires_key: true,
    },
    ProviderSpec {
        id: "anthropic",
        name: "Anthropic (Claude)",
        api_key_env: "ANTHROPIC_API_KEY",
        base_url_env: "ANTHROPIC_BASE_URL",
        default_base_url: "https://api.anthropic.com",
        default_model: "claude-sonnet-4-20250514",
        kind: ProviderKind::Anthropic,
        requires_key: true,
    },
    ProviderSpec {
        id: "gemini",
        name: "Google Gemini",
        api_key_env: "GEMINI_API_KEY",
        base_url_env: "GEMINI_BASE_URL",
        default_base_url: "https://generativelanguage.googleapis.com",
        default_model: "gemini-2.0-flash",
        kind: ProviderKind::Gemini,
        requires_key: true,
    },
    ProviderSpec {
        id: "openrouter",
        name: "OpenRouter",
        api_key_env: "OPENROUTER_API_KEY",
        base_url_env: "OPENROUTER_BASE_URL",
        default_base_url: "https://openrouter.ai/api/v1",
        default_model: "openai/gpt-4o-mini",
        kind: ProviderKind::OpenAiCompat,
        requires_key: true,
    },
    ProviderSpec {
        id: "groq",
        name: "Groq",
        api_key_env: "GROQ_API_KEY",
        base_url_env: "GROQ_BASE_URL",
        default_base_url: "https://api.groq.com/openai/v1",
        default_model: "llama-3.3-70b-versatile",
        kind: ProviderKind::OpenAiCompat,
        requires_key: true,
    },
    ProviderSpec {
        id: "deepseek",
        name: "DeepSeek",
        api_key_env: "DEEPSEEK_API_KEY",
        base_url_env: "DEEPSEEK_BASE_URL",
        default_base_url: "https://api.deepseek.com/v1",
        default_model: "deepseek-chat",
        kind: ProviderKind::OpenAiCompat,
        requires_key: true,
    },
    ProviderSpec {
        id: "together",
        name: "Together AI",
        api_key_env: "TOGETHER_API_KEY",
        base_url_env: "TOGETHER_BASE_URL",
        default_base_url: "https://api.together.xyz/v1",
        default_model: "meta-llama/Meta-Llama-3.1-70B-Instruct-Turbo",
        kind: ProviderKind::OpenAiCompat,
        requires_key: true,
    },
    ProviderSpec {
        id: "fireworks",
        name: "Fireworks AI",
        api_key_env: "FIREWORKS_API_KEY",
        base_url_env: "FIREWORKS_BASE_URL",
        default_base_url: "https://api.fireworks.ai/inference/v1",
        default_model: "accounts/fireworks/models/llama-v3p1-70b-instruct",
        kind: ProviderKind::OpenAiCompat,
        requires_key: true,
    },
    ProviderSpec {
        id: "mistral",
        name: "Mistral AI",
        api_key_env: "MISTRAL_API_KEY",
        base_url_env: "MISTRAL_BASE_URL",
        default_base_url: "https://api.mistral.ai/v1",
        default_model: "mistral-large-latest",
        kind: ProviderKind::OpenAiCompat,
        requires_key: true,
    },
    ProviderSpec {
        id: "ollama",
        name: "Ollama (local)",
        api_key_env: "OLLAMA_API_KEY",
        base_url_env: "OLLAMA_BASE_URL",
        default_base_url: "http://localhost:11434/v1",
        default_model: "llama3.2",
        kind: ProviderKind::Local,
        requires_key: false,
    },
];

/// Default provider when `ZZ_PROVIDER` is unset/empty.
pub const DEFAULT_PROVIDER_ID: &str = "xai";

/// Look up a provider by id (case-insensitive). Unknown ids return `None`.
pub fn find_provider(id: &str) -> Option<&'static ProviderSpec> {
    let id = id.trim();
    if id.is_empty() {
        return None;
    }
    PROVIDERS.iter().find(|p| p.id.eq_ignore_ascii_case(id))
}

/// Resolve the effective provider id: empty/`default` → xai.
pub fn resolve_provider_id(raw: &str) -> &'static str {
    let raw = raw.trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case("default") {
        return DEFAULT_PROVIDER_ID;
    }
    find_provider(raw)
        .map(|p| p.id)
        .unwrap_or(DEFAULT_PROVIDER_ID)
}

/// Spec for the resolved provider. Unknown ids fall back to xAI default.
pub fn provider_spec(raw: &str) -> &'static ProviderSpec {
    let id = resolve_provider_id(raw);
    find_provider(id).expect("DEFAULT_PROVIDER_ID is always in PROVIDERS")
}

/// List of known provider ids (for help text).
pub fn provider_ids() -> Vec<&'static str> {
    PROVIDERS.iter().map(|p| p.id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_provider_case_insensitive() {
        assert_eq!(find_provider("XAI").unwrap().id, "xai");
        assert_eq!(find_provider("OpenAI").unwrap().id, "openai");
        assert!(find_provider("nope").is_none());
    }

    #[test]
    fn resolve_empty_defaults_to_xai() {
        assert_eq!(resolve_provider_id(""), "xai");
        assert_eq!(resolve_provider_id("  "), "xai");
        assert_eq!(resolve_provider_id("groq"), "groq");
    }

    #[test]
    fn every_provider_has_unique_id() {
        let mut seen = std::collections::HashSet::new();
        for p in PROVIDERS {
            assert!(seen.insert(p.id), "duplicate provider id: {}", p.id);
        }
    }

    #[test]
    fn openai_compat_providers_have_v1_base() {
        for p in PROVIDERS {
            if matches!(p.kind, ProviderKind::OpenAiCompat | ProviderKind::Local) {
                assert!(!p.default_base_url.is_empty(), "{} missing base url", p.id);
            }
        }
    }
}
