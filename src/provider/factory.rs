//! Provider construction is decoupled from `Agent` via [`ProviderFactory`].
//!
//! `Agent` calls `DefaultProviderFactory::build` instead of constructing
//! `OpenAIProvider` directly. Adding a new backend (Anthropic native, Gemini,
//! Ollama-direct) means adding one match arm in [`DefaultProviderFactory`] —
//! no edits to the agent constructor or `switch_provider` paths. See YYC-112.

use anyhow::{Context, Result};

use super::LLMProvider;
use super::openai::OpenAIProvider;
use crate::config::ProviderConfig;

/// Builds an [`LLMProvider`] for a configured provider profile.
///
/// `Agent` holds an `Arc<dyn ProviderFactory>` so tests can inject a stub.
/// Production code uses [`DefaultProviderFactory`], which dispatches on
/// `cfg.r#type` ("openai-compat" by default).
pub trait ProviderFactory: Send + Sync {
    fn build(
        &self,
        cfg: &ProviderConfig,
        api_key: &str,
        max_context: usize,
        json_mode: bool,
    ) -> Result<Box<dyn LLMProvider>>;
}

/// Dispatches on `ProviderConfig::r#type`. Empty string and "openai-compat"
/// (the config default) build an [`OpenAIProvider`]; "openai" is accepted as
/// an alias for the same path.
pub struct DefaultProviderFactory;

impl ProviderFactory for DefaultProviderFactory {
    fn build(
        &self,
        cfg: &ProviderConfig,
        api_key: &str,
        max_context: usize,
        json_mode: bool,
    ) -> Result<Box<dyn LLMProvider>> {
        match cfg.r#type.as_str() {
            "openai" | "openai-compat" | "" => Ok(Box::new(
                OpenAIProvider::new(
                    &cfg.base_url,
                    api_key,
                    &cfg.model,
                    max_context,
                    cfg.max_retries,
                    json_mode,
                    cfg.debug,
                )
                .context("Failed to initialize LLM provider")?,
            )),
            other => Err(anyhow::anyhow!(
                "Unknown provider type '{other}'. Supported: openai, openai-compat. \
                 Set [provider].type in ~/.vulcan/config.toml."
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with_type(r#type: &str) -> ProviderConfig {
        ProviderConfig {
            r#type: r#type.to_string(),
            ..ProviderConfig::default()
        }
    }

    #[test]
    fn default_factory_builds_openai_for_default_type() {
        let cfg = cfg_with_type("openai-compat");
        let provider = DefaultProviderFactory
            .build(&cfg, "sk-test", 128_000, true)
            .expect("openai-compat should build");
        assert_eq!(provider.max_context(), 128_000);
    }

    #[test]
    fn default_factory_accepts_empty_type_alias() {
        let cfg = cfg_with_type("");
        DefaultProviderFactory
            .build(&cfg, "sk-test", 64_000, false)
            .expect("empty type should build OpenAI");
    }

    #[test]
    fn default_factory_accepts_openai_alias() {
        let cfg = cfg_with_type("openai");
        DefaultProviderFactory
            .build(&cfg, "sk-test", 8_000, false)
            .expect("'openai' type should build OpenAI");
    }

    #[test]
    fn default_factory_rejects_unknown_type() {
        let cfg = cfg_with_type("anthropic-native");
        let err = DefaultProviderFactory
            .build(&cfg, "sk-test", 128_000, false)
            .err()
            .expect("unknown provider type should error");
        let msg = err.to_string();
        assert!(msg.contains("anthropic-native"), "got {msg:?}");
        assert!(
            msg.contains("openai"),
            "should list supported types: {msg:?}"
        );
    }
}
