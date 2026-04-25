//! Provider model catalog: fetches metadata from a provider's `/models`
//! endpoint and caches it locally so we can validate model selection at
//! startup, surface helpful "did you mean…" suggestions, and (later) gate
//! features per-model.
//!
//! Supports two shapes:
//! - **OpenRouter-style** rich catalog (context length, pricing, feature
//!   flags, top provider underneath the slug).
//! - **OpenAI-style** sparse catalog (just IDs and ownership).
//!
//! See YYC-64.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ProviderError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    /// Context window in tokens. 0 means unknown.
    pub context_length: usize,
    pub pricing: Option<Pricing>,
    pub features: ModelFeatures,
    /// E.g. "DeepSeek" under an OpenRouter slug like `deepseek/deepseek-v4-flash`.
    pub top_provider: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pricing {
    pub input_per_token: f64,
    pub output_per_token: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelFeatures {
    pub tools: bool,
    pub vision: bool,
    pub json_mode: bool,
    pub reasoning: bool,
}

#[async_trait]
pub trait ProviderCatalog: Send + Sync {
    async fn list_models(&self) -> std::result::Result<Vec<ModelInfo>, ProviderError>;

    async fn get_model(&self, id: &str) -> std::result::Result<Option<ModelInfo>, ProviderError> {
        let models = self.list_models().await?;
        Ok(models.into_iter().find(|m| m.id == id))
    }
}

// ─── OpenRouter ─────────────────────────────────────────────────────────────

pub struct OpenRouterCatalog {
    client: Client,
    base_url: String,
    api_key: String,
    cache_ttl: Duration,
}

impl OpenRouterCatalog {
    pub fn new(client: Client, base_url: &str, api_key: &str, cache_ttl: Duration) -> Self {
        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            cache_ttl,
        }
    }
}

#[async_trait]
impl ProviderCatalog for OpenRouterCatalog {
    async fn list_models(&self) -> std::result::Result<Vec<ModelInfo>, ProviderError> {
        if let Some(cached) = read_cache(&self.base_url, self.cache_ttl) {
            return Ok(cached);
        }

        let url = format!("{}/models", self.base_url);
        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::from_response(status, &body, ""));
        }

        let body: Value = response.json().await.map_err(ProviderError::Network)?;
        let models = parse_openrouter(&body);
        let _ = write_cache(&self.base_url, &models);
        Ok(models)
    }
}

fn parse_openrouter(body: &Value) -> Vec<ModelInfo> {
    let arr = match body.get("data").and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return Vec::new(),
    };
    arr.iter()
        .filter_map(|m| {
            let id = m.get("id")?.as_str()?.to_string();
            let display_name = m
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or(&id)
                .to_string();
            let context_length = m
                .get("context_length")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize)
                .unwrap_or(0);
            let top_provider = m
                .get("top_provider")
                .and_then(|tp| tp.get("name"))
                .and_then(|v| v.as_str())
                .map(String::from);
            // OpenRouter pricing is given as strings like "0.00000028" per token.
            let pricing = m.get("pricing").and_then(|p| {
                let inp = p
                    .get("prompt")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<f64>().ok())?;
                let out = p
                    .get("completion")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<f64>().ok())?;
                Some(Pricing {
                    input_per_token: inp,
                    output_per_token: out,
                })
            });
            // Feature flags via `supported_parameters` array (OpenRouter convention).
            let supported = m
                .get("supported_parameters")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(|s| s.to_string()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let features = ModelFeatures {
                tools: supported.iter().any(|s| s == "tools" || s == "tool_choice"),
                json_mode: supported.iter().any(|s| s == "response_format"),
                vision: m
                    .get("architecture")
                    .and_then(|a| a.get("input_modalities"))
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().any(|s| s.as_str() == Some("image")))
                    .unwrap_or(false),
                reasoning: m
                    .get("supported_parameters")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter().any(|s| {
                            s.as_str() == Some("reasoning")
                                || s.as_str() == Some("include_reasoning")
                        })
                    })
                    .unwrap_or(false),
            };
            Some(ModelInfo {
                id,
                display_name,
                context_length,
                pricing,
                features,
                top_provider,
            })
        })
        .collect()
}

// ─── OpenAI sparse ─────────────────────────────────────────────────────────

pub struct OpenAICatalog {
    client: Client,
    base_url: String,
    api_key: String,
    cache_ttl: Duration,
}

impl OpenAICatalog {
    pub fn new(client: Client, base_url: &str, api_key: &str, cache_ttl: Duration) -> Self {
        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            cache_ttl,
        }
    }
}

#[async_trait]
impl ProviderCatalog for OpenAICatalog {
    async fn list_models(&self) -> std::result::Result<Vec<ModelInfo>, ProviderError> {
        if let Some(cached) = read_cache(&self.base_url, self.cache_ttl) {
            return Ok(cached);
        }
        let url = format!("{}/models", self.base_url);
        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::from_response(status, &body, ""));
        }
        let body: Value = response.json().await.map_err(ProviderError::Network)?;
        let models: Vec<ModelInfo> = body
            .get("data")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| {
                        let id = m.get("id")?.as_str()?.to_string();
                        Some(ModelInfo {
                            display_name: id.clone(),
                            id,
                            context_length: 0,
                            pricing: None,
                            features: ModelFeatures::default(),
                            top_provider: m
                                .get("owned_by")
                                .and_then(|v| v.as_str())
                                .map(String::from),
                        })
                    })
                    .collect::<Vec<ModelInfo>>()
            })
            .unwrap_or_default();
        let _ = write_cache(&self.base_url, &models);
        Ok(models)
    }
}

// ─── Construction ─────────────────────────────────────────────────────────

/// Pick the right catalog for a given base_url. Detects OpenRouter via the
/// host; everything else falls back to the sparse OpenAI shape (which most
/// OpenAI-compatible providers also serve).
pub fn for_base_url(
    client: Client,
    base_url: &str,
    api_key: &str,
    cache_ttl: Duration,
) -> Box<dyn ProviderCatalog> {
    if base_url.contains("openrouter.ai") {
        Box::new(OpenRouterCatalog::new(client, base_url, api_key, cache_ttl))
    } else {
        Box::new(OpenAICatalog::new(client, base_url, api_key, cache_ttl))
    }
}

// ─── Cache layer ─────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct CacheFile {
    fetched_at_unix: u64,
    models: Vec<ModelInfo>,
}

fn cache_path(base_url: &str) -> PathBuf {
    let host = host_slug(base_url);
    crate::config::vulcan_home()
        .join("cache")
        .join(format!("{host}_models.json"))
}

fn host_slug(base_url: &str) -> String {
    base_url
        .replace("https://", "")
        .replace("http://", "")
        .replace('/', "_")
        .replace(':', "_")
}

fn read_cache(base_url: &str, ttl: Duration) -> Option<Vec<ModelInfo>> {
    let path = cache_path(base_url);
    let bytes = std::fs::read(&path).ok()?;
    let cache: CacheFile = serde_json::from_slice(&bytes).ok()?;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_secs();
    if now.saturating_sub(cache.fetched_at_unix) > ttl.as_secs() {
        tracing::debug!("catalog cache for {base_url} is stale");
        return None;
    }
    Some(cache.models)
}

fn write_cache(base_url: &str, models: &[ModelInfo]) -> Result<()> {
    let path = cache_path(base_url);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_secs();
    let cache = CacheFile {
        fetched_at_unix: now,
        models: models.to_vec(),
    };
    let json = serde_json::to_vec_pretty(&cache)?;
    std::fs::write(path, json)?;
    Ok(())
}

// ─── Validation helper ─────────────────────────────────────────────────────

/// Find the closest model IDs to `target` by Damerau-Levenshtein distance.
/// Returns up to `limit` candidates ordered by similarity.
pub fn fuzzy_suggest(models: &[ModelInfo], target: &str, limit: usize) -> Vec<String> {
    let mut scored: Vec<(usize, &str)> = models
        .iter()
        .map(|m| {
            let d = strsim::damerau_levenshtein(&m.id.to_lowercase(), &target.to_lowercase());
            (d, m.id.as_str())
        })
        .collect();
    scored.sort_by_key(|(d, _)| *d);
    scored
        .into_iter()
        .take(limit)
        .map(|(_, id)| id.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_openrouter_extracts_pricing_and_features() {
        let body = serde_json::json!({
            "data": [
                {
                    "id": "deepseek/deepseek-v4-flash",
                    "name": "DeepSeek V4 Flash",
                    "context_length": 128000,
                    "pricing": {
                        "prompt": "0.00000028",
                        "completion": "0.0000011"
                    },
                    "supported_parameters": ["tools", "tool_choice", "response_format", "include_reasoning"],
                    "architecture": {
                        "input_modalities": ["text"]
                    },
                    "top_provider": { "name": "DeepSeek" }
                },
                {
                    "id": "openai/gpt-4o",
                    "name": "GPT-4o",
                    "context_length": 128000,
                    "pricing": {
                        "prompt": "0.0000025",
                        "completion": "0.00001"
                    },
                    "supported_parameters": ["tools", "response_format"],
                    "architecture": {
                        "input_modalities": ["text", "image"]
                    },
                    "top_provider": { "name": "OpenAI" }
                }
            ]
        });

        let models = parse_openrouter(&body);
        assert_eq!(models.len(), 2);

        let deepseek = &models[0];
        assert_eq!(deepseek.id, "deepseek/deepseek-v4-flash");
        assert_eq!(deepseek.context_length, 128000);
        let pricing = deepseek.pricing.as_ref().unwrap();
        assert!((pricing.input_per_token - 0.00000028).abs() < f64::EPSILON);
        assert!(deepseek.features.tools);
        assert!(deepseek.features.json_mode);
        assert!(deepseek.features.reasoning);
        assert!(!deepseek.features.vision);
        assert_eq!(deepseek.top_provider.as_deref(), Some("DeepSeek"));

        let gpt = &models[1];
        assert!(gpt.features.tools);
        assert!(gpt.features.json_mode);
        assert!(gpt.features.vision); // image modality
        assert!(!gpt.features.reasoning);
    }

    #[test]
    fn fuzzy_suggest_finds_close_matches() {
        let models = vec![
            ModelInfo {
                id: "deepseek/deepseek-v4-flash".into(),
                display_name: "DeepSeek V4 Flash".into(),
                context_length: 128000,
                pricing: None,
                features: ModelFeatures::default(),
                top_provider: None,
            },
            ModelInfo {
                id: "deepseek/deepseek-chat".into(),
                display_name: "DeepSeek Chat".into(),
                context_length: 32000,
                pricing: None,
                features: ModelFeatures::default(),
                top_provider: None,
            },
            ModelInfo {
                id: "openai/gpt-4o".into(),
                display_name: "GPT-4o".into(),
                context_length: 128000,
                pricing: None,
                features: ModelFeatures::default(),
                top_provider: None,
            },
        ];

        let suggestions = fuzzy_suggest(&models, "deepseek/deepseek-v4-flsh", 3);
        // Closest match should be the deepseek-v4-flash slug.
        assert_eq!(suggestions[0], "deepseek/deepseek-v4-flash");
    }
}
