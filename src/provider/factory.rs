//! Provider construction is decoupled from `Agent` via [`ProviderFactory`].
//!
//! `Agent` calls `DefaultProviderFactory::build` instead of constructing
//! `OpenAIProvider` directly. Adding a new backend (Anthropic native, Gemini,
//! Ollama-direct) means adding one match arm in [`DefaultProviderFactory`] —
//! no edits to the agent constructor or `switch_provider` paths. See YYC-112.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use parking_lot::RwLock;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::openai::OpenAIProvider;
use super::{ChatResponse, LLMProvider, Message, StreamEvent, ToolDefinition};
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

#[derive(Default)]
pub struct ExtensionProviderCatalog {
    singletons: RwLock<HashMap<String, Arc<dyn LLMProvider>>>,
    factories: RwLock<HashMap<String, Arc<dyn ProviderFactory>>>,
}

impl ExtensionProviderCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_singleton(
        &self,
        extension_id: &str,
        name: String,
        provider: Arc<dyn LLMProvider>,
    ) -> Result<()> {
        self.validate_name(extension_id, &name)?;
        if self.factories.read().contains_key(&name) || self.singletons.read().contains_key(&name) {
            anyhow::bail!("extension provider `{name}` is already registered");
        }
        self.singletons.write().insert(name, provider);
        Ok(())
    }

    pub fn register_factory(
        &self,
        extension_id: &str,
        name: String,
        factory: Arc<dyn ProviderFactory>,
    ) -> Result<()> {
        self.validate_name(extension_id, &name)?;
        if self.singletons.read().contains_key(&name) || self.factories.read().contains_key(&name) {
            anyhow::bail!("extension provider `{name}` is already registered");
        }
        self.factories.write().insert(name, factory);
        Ok(())
    }

    pub fn names(&self) -> Vec<String> {
        let mut names = self.singletons.read().keys().cloned().collect::<Vec<_>>();
        names.extend(self.factories.read().keys().cloned());
        names.sort();
        names.dedup();
        names
    }

    pub fn contains(&self, name: &str) -> bool {
        self.singletons.read().contains_key(name) || self.factories.read().contains_key(name)
    }

    pub fn build(
        &self,
        cfg: &ProviderConfig,
        api_key: &str,
        max_context: usize,
        json_mode: bool,
    ) -> Option<Result<Box<dyn LLMProvider>>> {
        if let Some(provider) = self.singletons.read().get(&cfg.r#type).cloned() {
            return Some(Ok(Box::new(SharedProvider(provider))));
        }
        self.factories
            .read()
            .get(&cfg.r#type)
            .cloned()
            .map(|factory| factory.build(cfg, api_key, max_context, json_mode))
    }

    fn validate_name(&self, extension_id: &str, name: &str) -> Result<()> {
        if name.trim().is_empty() {
            anyhow::bail!("extension provider name is empty");
        }
        if reserved_provider_name(name) {
            anyhow::bail!("extension provider `{name}` uses a reserved provider name");
        }
        let expected = format!("{extension_id}.");
        if !name.starts_with(&expected) {
            anyhow::bail!("extension provider `{name}` must be prefixed with `{expected}`");
        }
        Ok(())
    }
}

fn reserved_provider_name(name: &str) -> bool {
    matches!(
        name,
        "openai"
            | "openai-compat"
            | "anthropic"
            | "lm_studio"
            | "lm-studio"
            | "openrouter"
            | "ollama"
    )
}

struct SharedProvider(Arc<dyn LLMProvider>);

#[async_trait::async_trait]
impl LLMProvider for SharedProvider {
    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
    ) -> Result<ChatResponse> {
        self.0.chat(messages, tools, cancel).await
    }

    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        tx: mpsc::Sender<StreamEvent>,
        cancel: CancellationToken,
    ) -> Result<()> {
        self.0.chat_stream(messages, tools, tx, cancel).await
    }

    fn max_context(&self) -> usize {
        self.0.max_context()
    }
}

pub struct ExtensionAwareProviderFactory {
    extensions: Arc<ExtensionProviderCatalog>,
}

impl ExtensionAwareProviderFactory {
    pub fn new(extensions: Arc<ExtensionProviderCatalog>) -> Self {
        Self { extensions }
    }
}

impl ProviderFactory for ExtensionAwareProviderFactory {
    fn build(
        &self,
        cfg: &ProviderConfig,
        api_key: &str,
        max_context: usize,
        json_mode: bool,
    ) -> Result<Box<dyn LLMProvider>> {
        if let Some(result) = self.extensions.build(cfg, api_key, max_context, json_mode) {
            return result;
        }
        DefaultProviderFactory.build(cfg, api_key, max_context, json_mode)
    }
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
                    cfg.max_output_tokens,
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
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    struct TestProvider(usize);

    #[async_trait::async_trait]
    impl LLMProvider for TestProvider {
        async fn chat(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
            _cancel: CancellationToken,
        ) -> Result<ChatResponse> {
            Ok(ChatResponse {
                content: Some(self.0.to_string()),
                tool_calls: None,
                usage: None,
                finish_reason: Some("stop".into()),
                reasoning_content: None,
            })
        }

        async fn chat_stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
            _tx: mpsc::Sender<StreamEvent>,
            _cancel: CancellationToken,
        ) -> Result<()> {
            Ok(())
        }

        fn max_context(&self) -> usize {
            self.0
        }
    }

    struct FreshFactory(AtomicUsize);

    impl ProviderFactory for FreshFactory {
        fn build(
            &self,
            _cfg: &ProviderConfig,
            _api_key: &str,
            _max_context: usize,
            _json_mode: bool,
        ) -> Result<Box<dyn LLMProvider>> {
            let id = self.0.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(Box::new(TestProvider(id)))
        }
    }

    #[test]
    fn extension_catalog_singleton_reuses_provider_instance() {
        let catalog = ExtensionProviderCatalog::new();
        catalog
            .register_singleton("demo", "demo.echo".into(), Arc::new(TestProvider(42)))
            .unwrap();
        let cfg = cfg_with_type("demo.echo");

        let first = catalog.build(&cfg, "", 0, false).unwrap().unwrap();
        let second = catalog.build(&cfg, "", 0, false).unwrap().unwrap();

        assert_eq!(first.max_context(), 42);
        assert_eq!(second.max_context(), 42);
    }

    #[test]
    fn extension_catalog_factory_builds_fresh_provider_each_time() {
        let catalog = ExtensionProviderCatalog::new();
        catalog
            .register_factory(
                "demo",
                "demo.factory_echo".into(),
                Arc::new(FreshFactory(AtomicUsize::new(0))),
            )
            .unwrap();
        let cfg = cfg_with_type("demo.factory_echo");

        let first = catalog.build(&cfg, "", 0, false).unwrap().unwrap();
        let second = catalog.build(&cfg, "", 0, false).unwrap().unwrap();

        assert_eq!(first.max_context(), 1);
        assert_eq!(second.max_context(), 2);
    }

    #[test]
    fn extension_catalog_rejects_unprefixed_and_reserved_names() {
        let catalog = ExtensionProviderCatalog::new();
        let provider = Arc::new(TestProvider(1));
        assert!(
            catalog
                .register_singleton("demo", "echo".into(), provider.clone())
                .is_err()
        );
        assert!(
            catalog
                .register_singleton("demo", "openai".into(), provider)
                .is_err()
        );
    }
}
