//! Multi-provider fallback for ZeroZero .
//!
//! Wraps multiple providers and tries each in order. If one fails
//! (rate limit, network error, etc.), automatically falls back to
//! the next provider.

use crate::{ChatMessage, DeltaStream, Effort, Provider, SseEventStream};
use std::sync::Arc;

/// A provider that tries multiple providers in order.
pub struct FallbackProvider {
    providers: Vec<Arc<dyn Provider>>,
    names: Vec<String>,
}

impl Default for FallbackProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl FallbackProvider {
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
            names: Vec::new(),
        }
    }

    /// Add a provider with a name for logging.
    pub fn with_provider(mut self, name: impl Into<String>, provider: Arc<dyn Provider>) -> Self {
        self.providers.push(provider);
        self.names.push(name.into());
        self
    }

    /// Get the number of providers.
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

#[async_trait::async_trait]
impl Provider for FallbackProvider {
    async fn chat_stream(&self, prompt: &str) -> anyhow::Result<DeltaStream> {
        let mut last_error = None;
        for (i, provider) in self.providers.iter().enumerate() {
            match provider.chat_stream(prompt).await {
                Ok(stream) => return Ok(stream),
                Err(e) => {
                    eprintln!(
                        "warning: provider '{}' failed: {e}, trying next...",
                        self.names[i]
                    );
                    last_error = Some(e);
                }
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("no providers configured")))
    }

    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        effort: Effort,
        images: &[String],
    ) -> anyhow::Result<SseEventStream> {
        let mut last_error = None;
        for (i, provider) in self.providers.iter().enumerate() {
            match provider
                .chat_with_tools(messages, tools, effort, images)
                .await
            {
                Ok(stream) => return Ok(stream),
                Err(e) => {
                    eprintln!(
                        "warning: provider '{}' failed: {e}, trying next...",
                        self.names[i]
                    );
                    last_error = Some(e);
                }
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("no providers configured")))
    }

    /// Return the primary (first) provider's model name .
    /// Returns an empty string when no providers are configured.
    fn model(&self) -> &str {
        self.providers.first().map(|p| p.model()).unwrap_or("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fallback_empty() {
        let fb = FallbackProvider::new();
        assert!(fb.is_empty());
        assert_eq!(fb.len(), 0);
    }
}
