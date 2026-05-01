use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Result;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use vulcan::extensions::api::{
    DaemonCodeExtension, ExtensionRegistration, SessionExtension, SessionExtensionCtx,
};
use vulcan::extensions::{
    ExtensionCapability, ExtensionMetadata, ExtensionProviderDefaults, ExtensionSource,
    ExtensionStatus,
};
use vulcan::provider::{ChatResponse, LLMProvider, Message, StreamEvent, ToolDefinition, Usage};

const ID: &str = "demo";

#[derive(Default)]
pub struct ProviderDemoExtension {
    singleton: Arc<EchoProvider>,
    factory_instances: Arc<AtomicUsize>,
}

impl DaemonCodeExtension for ProviderDemoExtension {
    fn metadata(&self) -> ExtensionMetadata {
        let mut m = ExtensionMetadata::new(
            ID,
            "Provider Demo",
            env!("CARGO_PKG_VERSION"),
            ExtensionSource::Builtin,
        );
        m.status = ExtensionStatus::Active;
        m.capabilities = vec![ExtensionCapability::Provider];
        m.description = "Registers demo.echo and demo.factory_echo providers.".into();
        m.provider_defaults = Some(ExtensionProviderDefaults {
            model: Some("demo-echo".into()),
            base_url: Some("vulcan-extension://demo.echo".into()),
            timeout_ms: Some(30_000),
        });
        m
    }

    fn instantiate(&self, _ctx: SessionExtensionCtx) -> Arc<dyn SessionExtension> {
        Arc::new(ProviderDemoSession {
            singleton: Arc::clone(&self.singleton),
            factory_instances: Arc::clone(&self.factory_instances),
        })
    }
}

struct ProviderDemoSession {
    singleton: Arc<EchoProvider>,
    factory_instances: Arc<AtomicUsize>,
}

impl SessionExtension for ProviderDemoSession {
    fn providers(&self) -> Vec<(String, Arc<dyn LLMProvider>)> {
        vec![("demo.echo".into(), self.singleton.clone())]
    }

    fn provider_factories(
        &self,
    ) -> Vec<(String, Arc<dyn vulcan::provider::factory::ProviderFactory>)> {
        vec![(
            "demo.factory_echo".into(),
            Arc::new(EchoProviderFactory {
                instances: Arc::clone(&self.factory_instances),
            }),
        )]
    }
}

#[derive(Default)]
struct EchoProvider {
    instance_id: usize,
}

#[async_trait::async_trait]
impl LLMProvider for EchoProvider {
    async fn chat(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        _cancel: CancellationToken,
    ) -> Result<ChatResponse> {
        Ok(ChatResponse {
            content: Some(format!(
                "echo[{}]: {}",
                self.instance_id,
                latest_user(messages)
            )),
            tool_calls: None,
            usage: Some(Usage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
            }),
            finish_reason: Some("stop".into()),
            reasoning_content: None,
        })
    }

    async fn chat_stream(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        tx: mpsc::Sender<StreamEvent>,
        cancel: CancellationToken,
    ) -> Result<()> {
        let text = format!("echo[{}]: {}", self.instance_id, latest_user(messages));
        if !cancel.is_cancelled() {
            let _ = tx.send(StreamEvent::Text(text.clone())).await;
            let _ = tx
                .send(StreamEvent::Done(ChatResponse {
                    content: Some(text),
                    tool_calls: None,
                    usage: None,
                    finish_reason: Some("stop".into()),
                    reasoning_content: None,
                }))
                .await;
        }
        Ok(())
    }

    fn max_context(&self) -> usize {
        8192
    }
}

struct EchoProviderFactory {
    instances: Arc<AtomicUsize>,
}

impl vulcan::provider::factory::ProviderFactory for EchoProviderFactory {
    fn build(
        &self,
        _cfg: &vulcan::config::ProviderConfig,
        _api_key: &str,
        _max_context: usize,
        _json_mode: bool,
    ) -> Result<Box<dyn LLMProvider>> {
        let id = self.instances.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(Box::new(EchoProvider { instance_id: id }))
    }
}

fn latest_user(messages: &[Message]) -> String {
    messages
        .iter()
        .rev()
        .find_map(|message| match message {
            Message::User { content } => Some(content.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

inventory::submit! {
    ExtensionRegistration {
        register: || Arc::new(ProviderDemoExtension::default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use vulcan::extensions::ExtensionStateContext;
    use vulcan::memory::SessionStore;

    #[tokio::test]
    async fn demo_echo_provider_replays_latest_user_message() {
        let ext = ProviderDemoExtension::default();
        let session = ext.instantiate(SessionExtensionCtx {
            cwd: std::path::PathBuf::from("."),
            session_id: "sess".into(),
            memory: Arc::new(SessionStore::in_memory()),
            frontend_capabilities: Vec::new(),
            frontend_extensions: Vec::new(),
            state: ExtensionStateContext::in_memory_for_tests("sess", "demo"),
            frontend_events: vulcan::extensions::api::FrontendEventSink::noop(),
        });
        let provider = session.providers().remove(0).1;
        let response = provider
            .chat(
                &[Message::User {
                    content: "hello".into(),
                }],
                &[],
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(response.content.as_deref(), Some("echo[0]: hello"));
    }

    #[tokio::test]
    async fn agent_can_select_demo_echo_provider_from_profile() {
        let mut providers = HashMap::new();
        providers.insert(
            "demo-echo".into(),
            vulcan::config::ProviderConfig {
                r#type: "demo.echo".into(),
                model: "demo-echo".into(),
                disable_catalog: true,
                ..vulcan::config::ProviderConfig::default()
            },
        );
        let config = vulcan::config::Config {
            active_profile: Some("demo-echo".into()),
            providers,
            ..vulcan::config::Config::default()
        };
        let pool = Arc::new(vulcan::runtime_pool::RuntimeResourcePool::for_tests());
        let mut agent = vulcan::agent::Agent::builder(&config)
            .with_pool(pool)
            .build()
            .await
            .unwrap();

        let response = agent.run_prompt("ping").await.unwrap();

        assert_eq!(response, "echo[0]: ping");
    }
}
