//! Model catalog for the 3-tier model picker (Codex-style).
//!
//! Tier 1: Provider (xai, openai, anthropic, gemini, openrouter, …)
//! Tier 2: Model (per-provider predefined list)
//! Tier 3: Reasoning effort (none/low/medium/high) — shown only when the
//!         selected model supports reasoning.

use zerozero_llm::Effort;

/// One provider entry in the catalog.
#[derive(Debug, Clone)]
pub struct ProviderEntry {
    /// Canonical provider id (matches `ZZ_PROVIDER` env var values).
    pub id: &'static str,
    /// Human-readable display name.
    pub name: &'static str,
    /// API key env var (empty = no key required, e.g. ollama).
    pub api_key_env: &'static str,
    /// Default base URL.
    pub default_base_url: &'static str,
    /// Whether this is a local/offline provider.
    pub local: bool,
    /// Predefined models for this provider.
    pub models: &'static [ModelEntry],
}

/// One model entry within a provider.
#[derive(Debug, Clone, Copy)]
pub struct ModelEntry {
    /// Model id (passed to the API).
    pub id: &'static str,
    /// Display name.
    pub name: &'static str,
    /// Short description / tagline.
    pub description: &'static str,
    /// Whether this model supports reasoning effort (tier 3).
    pub reasoning: bool,
    /// Recommended default effort for this model (used when tier 3 is skipped).
    pub default_effort: Effort,
}

/// The full static catalog. Order = display order in the picker.
/// Keep ids in sync with `zerozero_llm::providers::PROVIDERS`.
pub const CATALOG: &[ProviderEntry] = &[
    ProviderEntry {
        id: "xai",
        name: "xAI (Grok)",
        api_key_env: "XAI_API_KEY",
        default_base_url: "https://api.x.ai/v1",
        local: false,
        models: &[
            ModelEntry {
                id: "grok-4",
                name: "Grok 4",
                description: "Fast, capable default — balanced quality/latency",
                reasoning: false,
                default_effort: Effort::None,
            },
            ModelEntry {
                id: "grok-4.3",
                name: "Grok 4.3",
                description: "Reasoning-capable frontier model",
                reasoning: true,
                default_effort: Effort::Medium,
            },
            ModelEntry {
                id: "grok-3",
                name: "Grok 3",
                description: "Previous generation — cheaper",
                reasoning: false,
                default_effort: Effort::None,
            },
        ],
    },
    ProviderEntry {
        id: "openai",
        name: "OpenAI",
        api_key_env: "OPENAI_API_KEY",
        default_base_url: "https://api.openai.com/v1",
        local: false,
        models: &[
            ModelEntry {
                id: "gpt-5.5",
                name: "GPT-5.5",
                description: "Newest frontier model — recommended for most tasks",
                reasoning: true,
                default_effort: Effort::Medium,
            },
            ModelEntry {
                id: "gpt-5.4",
                name: "GPT-5.4",
                description: "Previous frontier — solid all-rounder",
                reasoning: true,
                default_effort: Effort::Medium,
            },
            ModelEntry {
                id: "o3-mini",
                name: "o3-mini",
                description: "Compact reasoning model — fast + cheap",
                reasoning: true,
                default_effort: Effort::Medium,
            },
            ModelEntry {
                id: "gpt-4o-mini",
                name: "GPT-4o mini",
                description: "Cheapest non-reasoning option",
                reasoning: false,
                default_effort: Effort::None,
            },
        ],
    },
    ProviderEntry {
        id: "anthropic",
        name: "Anthropic (Claude)",
        api_key_env: "ANTHROPIC_API_KEY",
        default_base_url: "https://api.anthropic.com",
        local: false,
        models: &[
            ModelEntry {
                id: "claude-opus-4-20250514",
                name: "Claude Opus 4",
                description: "Most capable Claude — deep reasoning",
                reasoning: true,
                default_effort: Effort::High,
            },
            ModelEntry {
                id: "claude-sonnet-4-20250514",
                name: "Claude Sonnet 4",
                description: "Balanced quality/latency — default",
                reasoning: true,
                default_effort: Effort::Medium,
            },
            ModelEntry {
                id: "claude-3-5-haiku-20241022",
                name: "Claude 3.5 Haiku",
                description: "Fast + affordable",
                reasoning: true,
                default_effort: Effort::Low,
            },
        ],
    },
    ProviderEntry {
        id: "gemini",
        name: "Google Gemini",
        api_key_env: "GEMINI_API_KEY",
        default_base_url: "https://generativelanguage.googleapis.com",
        local: false,
        models: &[
            ModelEntry {
                id: "gemini-2.0-flash",
                name: "Gemini 2.0 Flash",
                description: "Fast multimodal default",
                reasoning: false,
                default_effort: Effort::None,
            },
            ModelEntry {
                id: "gemini-2.5-pro",
                name: "Gemini 2.5 Pro",
                description: "Higher quality Gemini",
                reasoning: true,
                default_effort: Effort::Medium,
            },
        ],
    },
    ProviderEntry {
        id: "openrouter",
        name: "OpenRouter",
        api_key_env: "OPENROUTER_API_KEY",
        default_base_url: "https://openrouter.ai/api/v1",
        local: false,
        models: &[
            ModelEntry {
                id: "openai/gpt-4o-mini",
                name: "GPT-4o mini (via OR)",
                description: "Routed through OpenRouter",
                reasoning: false,
                default_effort: Effort::None,
            },
            ModelEntry {
                id: "anthropic/claude-sonnet-4",
                name: "Claude Sonnet 4 (via OR)",
                description: "Routed through OpenRouter",
                reasoning: true,
                default_effort: Effort::Medium,
            },
        ],
    },
    ProviderEntry {
        id: "groq",
        name: "Groq",
        api_key_env: "GROQ_API_KEY",
        default_base_url: "https://api.groq.com/openai/v1",
        local: false,
        models: &[
            ModelEntry {
                id: "llama-3.3-70b-versatile",
                name: "Llama 3.3 70B",
                description: "Very fast inference",
                reasoning: false,
                default_effort: Effort::None,
            },
            ModelEntry {
                id: "qwen/qwen3-32b",
                name: "Qwen3 32B",
                description: "Strong open model on Groq",
                reasoning: false,
                default_effort: Effort::None,
            },
        ],
    },
    ProviderEntry {
        id: "deepseek",
        name: "DeepSeek",
        api_key_env: "DEEPSEEK_API_KEY",
        default_base_url: "https://api.deepseek.com/v1",
        local: false,
        models: &[
            ModelEntry {
                id: "deepseek-chat",
                name: "DeepSeek Chat",
                description: "General chat / coding",
                reasoning: false,
                default_effort: Effort::None,
            },
            ModelEntry {
                id: "deepseek-reasoner",
                name: "DeepSeek Reasoner",
                description: "Reasoning-tuned",
                reasoning: true,
                default_effort: Effort::Medium,
            },
        ],
    },
    ProviderEntry {
        id: "together",
        name: "Together AI",
        api_key_env: "TOGETHER_API_KEY",
        default_base_url: "https://api.together.xyz/v1",
        local: false,
        models: &[ModelEntry {
            id: "meta-llama/Meta-Llama-3.1-70B-Instruct-Turbo",
            name: "Llama 3.1 70B Turbo",
            description: "Together-hosted Llama",
            reasoning: false,
            default_effort: Effort::None,
        }],
    },
    ProviderEntry {
        id: "fireworks",
        name: "Fireworks AI",
        api_key_env: "FIREWORKS_API_KEY",
        default_base_url: "https://api.fireworks.ai/inference/v1",
        local: false,
        models: &[ModelEntry {
            id: "accounts/fireworks/models/llama-v3p1-70b-instruct",
            name: "Llama 3.1 70B",
            description: "Fireworks-hosted Llama",
            reasoning: false,
            default_effort: Effort::None,
        }],
    },
    ProviderEntry {
        id: "mistral",
        name: "Mistral AI",
        api_key_env: "MISTRAL_API_KEY",
        default_base_url: "https://api.mistral.ai/v1",
        local: false,
        models: &[
            ModelEntry {
                id: "mistral-large-latest",
                name: "Mistral Large",
                description: "Flagship Mistral model",
                reasoning: false,
                default_effort: Effort::None,
            },
            ModelEntry {
                id: "codestral-latest",
                name: "Codestral",
                description: "Code-focused Mistral",
                reasoning: false,
                default_effort: Effort::None,
            },
        ],
    },
    ProviderEntry {
        id: "ollama",
        name: "Ollama (local)",
        api_key_env: "",
        default_base_url: "http://localhost:11434/v1",
        local: true,
        models: &[
            ModelEntry {
                id: "llama3.2",
                name: "Llama 3.2",
                description: "Meta's open model — default local",
                reasoning: true,
                default_effort: Effort::Low,
            },
            ModelEntry {
                id: "qwen2.5-coder:7b",
                name: "Qwen2.5 Coder 7B",
                description: "Code-tuned open model",
                reasoning: true,
                default_effort: Effort::Low,
            },
            ModelEntry {
                id: "deepseek-r1:7b",
                name: "DeepSeek R1 7B",
                description: "Reasoning-tuned open model",
                reasoning: true,
                default_effort: Effort::Medium,
            },
        ],
    },
];

/// All valid effort levels in display order.
pub const EFFORT_TIERS: &[Effort] = &[Effort::None, Effort::Low, Effort::Medium, Effort::High];

/// Find a provider entry by id.
pub fn find_provider(id: &str) -> Option<&'static ProviderEntry> {
    CATALOG.iter().find(|p| p.id == id)
}

/// Find a model entry within a provider by model id.
pub fn find_model(provider_id: &str, model_id: &str) -> Option<&'static ModelEntry> {
    find_provider(provider_id)?
        .models
        .iter()
        .find(|m| m.id == model_id)
}

/// Detect the provider id from the current model name (best-effort).
/// Used to pre-select the right tier-1 entry when the picker opens.
pub fn detect_provider_for_model(model: &str) -> &'static str {
    let m = model.to_ascii_lowercase();
    if m.starts_with("grok") {
        "xai"
    } else if m.starts_with("claude") {
        "anthropic"
    } else if m.starts_with("gemini") {
        "gemini"
    } else if m.starts_with("gpt") || m.starts_with("o3") || m.starts_with("o4") {
        "openai"
    } else if m.contains("deepseek") && !m.contains(":") {
        "deepseek"
    } else if m.contains("mistral") || m.contains("codestral") {
        "mistral"
    } else if m.contains("llama") || m.contains("qwen") || m.contains("deepseek") {
        "ollama"
    } else if m.contains('/') {
        // openrouter-style vendor/model ids
        "openrouter"
    } else {
        "xai"
    }
}
