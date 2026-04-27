//! Provider switching + catalog helpers extracted from `agent/mod.rs`
//! (YYC-109 redo). Owns the runtime model/profile swap surface plus
//! the catalog fetch / model-resolution helpers used by `build_from_parts`.

use anyhow::Result;
use tokio_util::sync::CancellationToken;

use crate::config::{Config, ProviderConfig};
use crate::context::ContextManager;
use crate::provider::factory::{DefaultProviderFactory, ProviderFactory};

use super::{Agent, ModelSelection, is_local_base_url};

impl Agent {
    pub async fn available_models(&self) -> Result<Vec<crate::provider::catalog::ModelInfo>> {
        Self::fetch_catalog_for(&self.provider_config, &self.provider_api_key).await
    }

    pub async fn switch_model(&mut self, model_id: &str) -> Result<ModelSelection> {
        if self.turn_cancel.is_cancelled() {
            self.turn_cancel = CancellationToken::new();
        }

        let mut next_config = self.provider_config.clone();
        next_config.model = model_id.to_string();
        let selection = Self::resolve_model_selection(&next_config, &self.provider_api_key).await?;
        let provider = DefaultProviderFactory.build(
            &next_config,
            &self.provider_api_key,
            selection.max_context,
            selection.model.features.json_mode,
        )?;

        self.provider = provider;
        self.provider_config = next_config;
        self.context =
            ContextManager::with_config(selection.max_context, self.compaction_config.clone());
        self.pricing = selection.pricing.clone();

        Ok(selection)
    }

    /// Switch to a named provider profile from `Config.providers` (YYC-94).
    /// Rebuilds the underlying `OpenAIProvider` against the chosen profile
    /// (base URL, model, retries, debug mode), refreshes the model catalog,
    /// and updates context window + pricing. Hooks, tools, memory, and the
    /// in-flight session state are left untouched.
    ///
    /// Pass `None` to revert to the unnamed legacy `[provider]` block.
    pub async fn switch_provider(
        &mut self,
        profile: Option<&str>,
        config: &Config,
    ) -> Result<ModelSelection> {
        if self.turn_cancel.is_cancelled() {
            self.turn_cancel = CancellationToken::new();
        }

        let next_config =
            match profile {
                Some(name) => config.providers.get(name).cloned().ok_or_else(|| {
                    anyhow::anyhow!("Provider profile '{name}' not found in config")
                })?,
                None => config.provider.clone(),
            };
        // Local / self-hosted endpoints (Ollama, llama.cpp, vLLM unauth)
        // typically don't need an API key; skip the requirement when the
        // base URL looks local or the user explicitly disabled catalog
        // fetching. Falls back to empty string so the OpenAI-compat path
        // sends `Authorization: Bearer ` and the server ignores it.
        let api_key = match config.api_key_for(&next_config) {
            Some(k) => k,
            None if next_config.disable_catalog || is_local_base_url(&next_config.base_url) => {
                String::new()
            }
            None => {
                anyhow::bail!(
                    "No API key for provider '{}' (set VULCAN_API_KEY or supply api_key in config)",
                    profile.unwrap_or("[provider]"),
                );
            }
        };

        let selection = Self::resolve_model_selection(&next_config, &api_key).await?;
        let provider = DefaultProviderFactory.build(
            &next_config,
            &api_key,
            selection.max_context,
            selection.model.features.json_mode,
        )?;

        self.provider = provider;
        self.provider_config = next_config;
        self.provider_api_key = api_key;
        self.context =
            ContextManager::with_config(selection.max_context, config.compaction.clone());
        self.compaction_config = config.compaction.clone();
        self.pricing = selection.pricing.clone();
        self.active_profile = profile.map(str::to_string);

        // YYC-95: persist the active profile so resume restores it.
        if let Err(e) = self.memory.save_provider_profile(&self.session_id, profile) {
            tracing::warn!("failed to persist provider profile: {e}");
        }

        Ok(selection)
    }

    /// Reapply the persisted provider profile for the current session
    /// (YYC-95). Call this after `resume_session` / `continue_last_session`
    /// to swap the agent onto whichever profile the session was last using.
    /// A stale profile reference (saved name that's been removed from
    /// config, or a swap that fails the catalog check) is logged as a
    /// warning and reverts to the legacy `[provider]` block — never an
    /// error, so resume can't be locked out by a config edit.
    pub async fn restore_persisted_provider(&mut self, config: &Config) -> Result<()> {
        let session_id = self.session_id.clone();
        let profile = self
            .memory
            .load_provider_profile(&session_id)
            .unwrap_or_else(|e| {
                tracing::warn!("could not read saved provider profile: {e}");
                None
            });
        let Some(name) = profile.as_deref() else {
            return Ok(());
        };
        if !config.providers.contains_key(name) {
            tracing::warn!(
                "saved provider profile '{name}' no longer exists; falling back to [provider]"
            );
            self.active_profile = None;
            let _ = self.memory.save_provider_profile(&session_id, None);
            return Ok(());
        }
        if let Err(e) = self.switch_provider(Some(name), config).await {
            tracing::warn!(
                "failed to restore provider profile '{name}': {e}; falling back to [provider]"
            );
            self.active_profile = None;
            let _ = self.memory.save_provider_profile(&session_id, None);
        }
        Ok(())
    }

    pub(in crate::agent) async fn fetch_catalog_for(
        provider: &ProviderConfig,
        api_key: &str,
    ) -> Result<Vec<crate::provider::catalog::ModelInfo>> {
        use std::time::Duration;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()?;
        let ttl = Duration::from_secs(provider.catalog_cache_ttl_hours * 3600);
        let catalog =
            crate::provider::catalog::for_base_url(client, &provider.base_url, api_key, ttl);
        catalog.list_models().await.map_err(Into::into)
    }

    pub(in crate::agent) async fn resolve_model_selection(
        provider: &ProviderConfig,
        api_key: &str,
    ) -> Result<ModelSelection> {
        let mut effective_max_context = provider.max_context;
        let mut model_info = crate::provider::catalog::ModelInfo {
            id: provider.model.clone(),
            display_name: provider.model.clone(),
            context_length: 0,
            pricing: None,
            features: crate::provider::catalog::ModelFeatures::default(),
            top_provider: None,
        };

        if !provider.disable_catalog {
            match Self::fetch_catalog_for(provider, api_key).await {
                Ok(models) => match models.iter().find(|m| m.id == provider.model) {
                    Some(found) => {
                        model_info = found.clone();
                        if model_info.context_length > 0 && provider.max_context == 128_000 {
                            effective_max_context = model_info.context_length;
                            tracing::info!(
                                "catalog: using context_length={} for {} (json_mode={})",
                                model_info.context_length,
                                model_info.id,
                                model_info.features.json_mode,
                            );
                        }
                    }
                    None => {
                        let suggestions =
                            crate::provider::catalog::fuzzy_suggest(&models, &provider.model, 3);
                        let hint = if suggestions.is_empty() {
                            String::new()
                        } else {
                            format!(" Did you mean: {}?", suggestions.join(", "))
                        };
                        anyhow::bail!(
                            "Model '{}' not found in provider catalog.{} \
                             (See `[provider].model` in config.)",
                            provider.model,
                            hint,
                        );
                    }
                },
                Err(e) => {
                    tracing::warn!("catalog fetch failed (continuing with config defaults): {e}");
                }
            }
        }

        Ok(ModelSelection {
            pricing: model_info.pricing.clone(),
            model: model_info,
            max_context: effective_max_context,
        })
    }
}
