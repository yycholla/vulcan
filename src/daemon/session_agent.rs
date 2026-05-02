//! Daemon Session-facing Agent assembly.
//!
//! `SessionState` owns lazy installation and concurrency; this module owns the
//! construction policy for daemon-managed Agents.

use std::sync::Arc;

use anyhow::Result;

use crate::agent::Agent;
use crate::config::Config;
use crate::runtime_pool::RuntimeResourcePool;

#[derive(Clone)]
pub struct SessionAgentAssembler {
    config: Arc<Config>,
    pool: Option<Arc<RuntimeResourcePool>>,
}

impl SessionAgentAssembler {
    pub fn new(config: Arc<Config>, pool: Option<Arc<RuntimeResourcePool>>) -> Self {
        Self { config, pool }
    }

    pub async fn assemble(&self, options: SessionAgentOptions) -> Result<Agent> {
        let mut builder = Agent::builder(&self.config);
        if let Some(pool) = &self.pool {
            builder = builder.with_pool(Arc::clone(pool));
        }
        builder = builder.with_frontend_context(
            options.frontend_capabilities.clone(),
            options.frontend_extensions.clone(),
            options.frontend_events.clone(),
        );
        if let Some(max_iterations) = options.max_iterations {
            builder = builder.with_max_iterations(max_iterations);
        }
        builder = builder.with_tool_profile(options.tool_profile.clone());

        let mut agent = builder.build().await?;
        if options.tool_profile.is_none() {
            if let Some(allowed_tools) = options.allowed_tools.as_deref() {
                agent.restrict_tools(allowed_tools);
            }
        }
        Ok(agent)
    }
}

#[derive(Clone, Debug)]
pub struct SessionAgentOptions {
    max_iterations: Option<u32>,
    tool_profile: Option<String>,
    allowed_tools: Option<Vec<String>>,
    frontend_capabilities: Vec<crate::extensions::FrontendCapability>,
    frontend_extensions: Vec<vulcan_frontend_api::FrontendExtensionDescriptor>,
    frontend_events: crate::extensions::api::FrontendEventSink,
}

impl Default for SessionAgentOptions {
    fn default() -> Self {
        Self {
            max_iterations: None,
            tool_profile: None,
            allowed_tools: None,
            frontend_capabilities: crate::extensions::FrontendCapability::text_only(),
            frontend_extensions: Vec::new(),
            frontend_events: crate::extensions::api::FrontendEventSink::default(),
        }
    }
}

impl SessionAgentOptions {
    pub fn subagent(
        max_iterations: u32,
        tool_profile: Option<String>,
        allowed_tools: Vec<String>,
    ) -> Self {
        Self {
            max_iterations: Some(max_iterations),
            tool_profile,
            allowed_tools: Some(allowed_tools),
            ..Self::default()
        }
    }

    pub fn with_frontend_context(
        mut self,
        capabilities: Vec<crate::extensions::FrontendCapability>,
        extensions: Vec<vulcan_frontend_api::FrontendExtensionDescriptor>,
        events: crate::extensions::api::FrontendEventSink,
    ) -> Self {
        self.frontend_capabilities = capabilities;
        self.frontend_extensions = extensions;
        self.frontend_events = events;
        self
    }

    pub fn frontend_capabilities(&self) -> Vec<crate::extensions::FrontendCapability> {
        self.frontend_capabilities.clone()
    }

    pub fn frontend_extensions(&self) -> Vec<vulcan_frontend_api::FrontendExtensionDescriptor> {
        self.frontend_extensions.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Message;

    fn local_config() -> Config {
        let mut config = Config::default();
        config.provider.base_url = "http://127.0.0.1:11434/v1".into();
        config.provider.disable_catalog = true;
        config
    }

    #[tokio::test]
    async fn pooled_assembly_uses_runtime_pool_resources() {
        let config = Arc::new(local_config());
        let pool = Arc::new(RuntimeResourcePool::for_tests());
        let assembler = SessionAgentAssembler::new(Arc::clone(&config), Some(Arc::clone(&pool)));

        let agent = assembler
            .assemble(SessionAgentOptions::default())
            .await
            .unwrap();
        let session_id = agent.session_id().to_string();
        agent
            .memory()
            .save_messages(
                &session_id,
                &[Message::User {
                    content: "pooled assembly".into(),
                }],
            )
            .unwrap();

        let loaded = pool.session_store().load_history(&session_id).unwrap();
        assert!(
            matches!(
                loaded.as_deref().and_then(|messages| messages.first()),
                Some(Message::User { content }) if content == "pooled assembly"
            ),
            "pooled assembly must share the RuntimeResourcePool SessionStore"
        );
        assert!(
            agent
                .tool_definitions()
                .iter()
                .any(|tool| tool.function.name == "spawn_subagent"),
            "spawn_subagent must remain registered during pooled assembly"
        );
    }

    #[tokio::test]
    async fn non_pooled_assembly_applies_session_specific_allowlist() {
        let config = Arc::new(local_config());
        let assembler = SessionAgentAssembler::new(config, None);
        let agent = assembler
            .assemble(SessionAgentOptions::subagent(
                2,
                None,
                vec!["read_file".into()],
            ))
            .await
            .unwrap();
        let tools: Vec<_> = agent
            .tool_definitions()
            .into_iter()
            .map(|tool| tool.function.name)
            .collect();

        assert_eq!(agent.iterations(), 0);
        assert!(tools.contains(&"read_file".to_string()));
        assert!(!tools.contains(&"spawn_subagent".to_string()));
    }
}
