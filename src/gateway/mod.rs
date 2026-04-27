use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use crate::config::Config;
use crate::gateway::agent_map::AgentMap;
use crate::gateway::commands::CommandDispatcher;
use crate::gateway::discord::DiscordPlatform;
use crate::gateway::lane::{LaneKey, LaneRouter, from_closure};
use crate::gateway::loopback::LoopbackPlatform;
use crate::gateway::outbound::OutboundDispatcher;
use crate::gateway::queue::{InboundQueue, InboundRow, OutboundQueue};
use crate::gateway::registry::PlatformRegistry;
use crate::gateway::server::{AppState, build_router};
use crate::memory::DbPool;
use anyhow::{Context, Result};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

pub mod agent_map;
pub mod commands;
pub mod discord;
pub mod lane;
pub mod loopback;
pub mod outbound;
pub mod queue;
pub mod registry;
pub mod render_registry;
pub mod routes;
pub mod server;
pub mod stream_render;
#[cfg(feature = "telegram")]
pub mod telegram;
pub mod worker;

pub async fn run(config: &Config, bind_override: Option<String>) -> Result<()> {
    let bind = bind_addr(config, bind_override)?;
    let listener = TcpListener::bind(&bind)
        .await
        .with_context(|| format!("failed to bind gateway on {bind}"))?;
    run_on_listener(config.clone(), listener, shutdown_signal()).await
}

async fn run_on_listener<S>(config: Config, listener: TcpListener, shutdown: S) -> Result<()>
where
    S: Future<Output = ()> + Send + 'static,
{
    let gateway = config
        .gateway
        .clone()
        .ok_or_else(|| anyhow::anyhow!("missing [gateway] config; set api_token before running"))?;

    let db = crate::memory::open_gateway_pool()?;
    let mut registry = PlatformRegistry::new();
    registry.register("loopback", Arc::new(LoopbackPlatform::default()));
    if gateway.discord.enabled {
        registry.register(
            "discord",
            Arc::new(DiscordPlatform::new(&gateway.discord.bot_token)?),
        );
    }
    let registry = Arc::new(registry);
    let agent_map = Arc::new(AgentMap::new(
        Arc::new(config),
        Duration::from_secs(gateway.idle_ttl_secs),
    ));

    run_on_listener_with_parts(gateway, listener, shutdown, db, registry, agent_map).await
}

async fn run_on_listener_with_parts<S>(
    gateway: crate::config::GatewayConfig,
    listener: TcpListener,
    shutdown: S,
    db: DbPool,
    registry: Arc<PlatformRegistry>,
    agent_map: Arc<AgentMap>,
) -> Result<()>
where
    S: Future<Output = ()> + Send + 'static,
{
    let inbound = Arc::new(InboundQueue::new(db.clone()));
    let outbound = Arc::new(OutboundQueue::new(
        db.clone(),
        gateway.outbound_max_attempts,
    ));

    let recovered_inbound = inbound.recover_processing().await?;
    let recovered_outbound = outbound.recover_sending().await?;
    if recovered_inbound > 0 || recovered_outbound > 0 {
        tracing::info!(
            recovered_inbound,
            recovered_outbound,
            "gateway recovered stuck queue rows"
        );
    }

    let evictor = agent_map.spawn_evictor();
    let render_registry = Arc::new(crate::gateway::render_registry::RenderRegistry::new());
    let outbound_dispatcher = OutboundDispatcher::new(
        Arc::clone(&outbound),
        Arc::clone(&registry),
        Arc::clone(&render_registry),
    )
    .spawn();
    let discord_dispatcher = if gateway.discord.enabled {
        Some(DiscordPlatform::spawn_gateway_client(
            gateway.discord.bot_token.clone(),
            gateway.discord.allow_bots,
            Arc::clone(&inbound),
        )?)
    } else {
        None
    };
    // YYC-18 PR-2c: build the dispatcher once at startup from
    // Config.gateway.commands; the four builtins are pre-registered
    // by CommandDispatcher::new regardless of TOML contents.
    let commands = Arc::new(CommandDispatcher::new(&gateway.commands));
    let inbound_dispatcher = spawn_inbound_dispatcher(
        Arc::clone(&inbound),
        Arc::clone(&outbound),
        Arc::clone(&agent_map),
        Arc::clone(&render_registry),
        Arc::clone(&registry),
        Arc::clone(&commands),
    );

    let app = build_router(AppState {
        api_token: Arc::new(gateway.api_token),
        inbound,
        outbound,
        registry,
        agent_map,
    });

    let addr = listener
        .local_addr()
        .context("gateway listener local_addr")?;
    tracing::info!(%addr, "gateway listening");
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .context("gateway server failed");

    inbound_dispatcher.abort();
    if let Some(handle) = discord_dispatcher {
        handle.abort();
    }
    drop(outbound_dispatcher);
    drop(evictor);

    result
}

fn spawn_inbound_dispatcher(
    inbound: Arc<InboundQueue>,
    outbound: Arc<OutboundQueue>,
    agent_map: Arc<AgentMap>,
    render_registry: Arc<crate::gateway::render_registry::RenderRegistry>,
    platform_registry: Arc<PlatformRegistry>,
    commands: Arc<CommandDispatcher>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let handler_inbound = Arc::clone(&inbound);
        let handler_outbound = Arc::clone(&outbound);
        let handler_agent_map = Arc::clone(&agent_map);
        let handler_render_registry = Arc::clone(&render_registry);
        let handler_platform_registry = Arc::clone(&platform_registry);
        let handler_commands = Arc::clone(&commands);
        let handler = from_closure(move |_lane: LaneKey, row: InboundRow| {
            let inbound = Arc::clone(&handler_inbound);
            let outbound = Arc::clone(&handler_outbound);
            let agent_map = Arc::clone(&handler_agent_map);
            let render_registry = Arc::clone(&handler_render_registry);
            let platform_registry = Arc::clone(&handler_platform_registry);
            let commands = Arc::clone(&handler_commands);
            async move {
                // Pick capabilities from the registered platform; default
                // (zero-feature) for an unknown platform name so the
                // worker still runs but the renderer's throttle behaves
                // as a "no edits, single-shot send" pipeline.
                let caps = platform_registry
                    .get(&row.platform)
                    .map(|p| p.capabilities())
                    .unwrap_or_default();
                if let Err(e) = worker::process_one(
                    row,
                    &agent_map,
                    &inbound,
                    &outbound,
                    &render_registry,
                    caps,
                    &commands,
                )
                .await
                {
                    tracing::error!(target: "gateway::inbound", error = %e, "inbound row failed");
                }
            }
        });
        let router: LaneRouter<InboundRow> = LaneRouter::new(handler);
        let mut ticker = tokio::time::interval(Duration::from_millis(100));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            loop {
                let row = match inbound.claim_next().await {
                    Ok(Some(row)) => row,
                    Ok(None) => break,
                    Err(e) => {
                        tracing::error!(target: "gateway::inbound", error = %e, "claim_next failed");
                        break;
                    }
                };
                let lane = LaneKey {
                    platform: row.platform.clone(),
                    chat_id: row.chat_id.clone(),
                };
                router.dispatch(lane, row).await;
            }
        }
    })
}

fn bind_addr(config: &Config, bind_override: Option<String>) -> Result<String> {
    if let Some(bind) = bind_override {
        return Ok(bind);
    }
    Ok(config
        .gateway
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("missing [gateway] config; set api_token before running"))?
        .bind
        .clone())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Agent;
    use crate::gateway::agent_map::AgentBuilder;
    use crate::hooks::HookRegistry;
    use crate::provider::mock::MockProvider;
    use crate::provider::{ChatResponse, LLMProvider, Message, StreamEvent, ToolDefinition};
    use crate::skills::SkillRegistry;
    use crate::tools::ToolRegistry;
    use async_trait::async_trait;
    use tokio::sync::oneshot;
    use tokio_util::sync::CancellationToken;

    fn config_with_gateway() -> Config {
        let mut config = Config::default();
        config.provider.api_key = Some("test-key".into());
        config.provider.disable_catalog = true;
        config.gateway = Some(crate::config::GatewayConfig {
            bind: "127.0.0.1:0".into(),
            api_token: "test-token".into(),
            idle_ttl_secs: 1800,
            max_concurrent_lanes: 64,
            outbound_max_attempts: 5,
            discord: crate::config::DiscordConfig::default(),
            telegram: crate::config::TelegramConfig::default(),
            commands: std::collections::HashMap::new(),
        });
        config
    }

    fn fresh_db() -> DbPool {
        crate::memory::in_memory_gateway_pool().expect("in-memory pool")
    }

    fn empty_skills() -> Arc<SkillRegistry> {
        Arc::new(SkillRegistry::new(&std::path::PathBuf::from(
            "/tmp/vulcan-test-skills-nonexistent",
        )))
    }

    struct ProviderHandle(Arc<MockProvider>);
    #[async_trait]
    impl LLMProvider for ProviderHandle {
        async fn chat(
            &self,
            m: &[Message],
            t: &[ToolDefinition],
            c: CancellationToken,
        ) -> Result<ChatResponse> {
            self.0.chat(m, t, c).await
        }
        async fn chat_stream(
            &self,
            m: &[Message],
            t: &[ToolDefinition],
            tx: tokio::sync::mpsc::Sender<StreamEvent>,
            c: CancellationToken,
        ) -> Result<()> {
            self.0.chat_stream(m, t, tx, c).await
        }
        fn max_context(&self) -> usize {
            self.0.max_context()
        }
    }

    fn mock_agent_map(config: Arc<Config>, reply: &'static str) -> Arc<AgentMap> {
        let builder: AgentBuilder = Arc::new(move |hooks: HookRegistry| {
            Box::pin(async move {
                let mock = Arc::new(MockProvider::new(128_000));
                mock.enqueue_text(reply);
                Ok(Agent::for_test(
                    Box::new(ProviderHandle(mock)),
                    ToolRegistry::new(),
                    hooks,
                    empty_skills(),
                ))
            })
        });
        Arc::new(AgentMap::with_builder(
            config,
            Duration::from_secs(1800),
            builder,
        ))
    }

    #[tokio::test]
    async fn run_wires_health_endpoint_on_supplied_listener() {
        let config = config_with_gateway();
        let gateway = config.gateway.clone().expect("gateway config");
        let db = fresh_db();
        let mut registry = PlatformRegistry::new();
        registry.register("loopback", Arc::new(LoopbackPlatform::default()));
        let registry = Arc::new(registry);
        let agent_map = mock_agent_map(Arc::new(config), "unused");
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let handle = tokio::spawn(async move {
            run_on_listener_with_parts(
                gateway,
                listener,
                async move {
                    let _ = shutdown_rx.await;
                },
                db,
                registry,
                agent_map,
            )
            .await
        });

        let client = reqwest::Client::new();
        let mut ok = false;
        for _ in 0..50 {
            match client.get(format!("http://{addr}/health")).send().await {
                Ok(resp) if resp.status().is_success() => {
                    ok = true;
                    break;
                }
                _ => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
        assert!(ok, "gateway health endpoint did not become ready");

        let _ = shutdown_tx.send(());
        handle.await.expect("join").expect("gateway exits cleanly");
    }

    #[tokio::test]
    async fn loopback_http_inbound_produces_outbound_reply() {
        let config = config_with_gateway();
        let gateway = config.gateway.clone().expect("gateway config");
        let db = fresh_db();
        let mut registry = PlatformRegistry::new();
        let loopback = Arc::new(LoopbackPlatform::default());
        registry.register("loopback", loopback.clone());
        let registry = Arc::new(registry);
        let agent_map = mock_agent_map(Arc::new(config), "hi back");
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let handle = tokio::spawn(async move {
            run_on_listener_with_parts(
                gateway,
                listener,
                async move {
                    let _ = shutdown_rx.await;
                },
                db,
                registry,
                agent_map,
            )
            .await
        });

        let client = reqwest::Client::new();
        let mut accepted = false;
        for _ in 0..50 {
            let response = client
                .post(format!("http://{addr}/v1/inbound"))
                .bearer_auth("test-token")
                .json(&serde_json::json!({
                    "platform": "loopback",
                    "chat_id": "c",
                    "user_id": "u",
                    "text": "hi"
                }))
                .send()
                .await;
            match response {
                Ok(resp) if resp.status() == reqwest::StatusCode::ACCEPTED => {
                    accepted = true;
                    break;
                }
                _ => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
        assert!(accepted, "gateway did not accept inbound message");

        let mut delivered = None;
        for _ in 0..100 {
            let recorded = loopback.recorded().await;
            if let Some(msg) = recorded.first() {
                delivered = Some(msg.clone());
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        let delivered = delivered.expect("loopback outbound reply");
        assert_eq!(delivered.platform, "loopback");
        assert_eq!(delivered.chat_id, "c");
        assert_eq!(delivered.text, "hi back");

        let _ = shutdown_tx.send(());
        handle.await.expect("join").expect("gateway exits cleanly");
    }

    #[tokio::test]
    async fn restart_recovers_processing_inbound_and_delivers_reply() {
        let config = config_with_gateway();
        let gateway = config.gateway.clone().expect("gateway config");
        let db = fresh_db();
        let inbound = InboundQueue::new(db.clone());
        inbound
            .enqueue(crate::platform::InboundMessage {
                platform: "loopback".into(),
                chat_id: "c".into(),
                user_id: "u".into(),
                text: "recover me".into(),
                message_id: None,
                reply_to: None,
                attachments: vec![],
            })
            .await
            .unwrap();
        let claimed = inbound.claim_next().await.unwrap().expect("processing row");
        assert_eq!(claimed.text, "recover me");
        // YYC-137: stamp the heartbeat into the past so the gateway's
        // startup recover_processing (which only resets stale rows)
        // picks this up. Without this the row's fresh heartbeat looks
        // like a live worker is mid-flight and recovery skips it.
        {
            let c = db.get().unwrap();
            let now = chrono::Utc::now().timestamp();
            c.execute(
                "UPDATE inbound_queue SET last_heartbeat_at = ?1 WHERE id = ?2",
                rusqlite::params![now - 7200, claimed.id],
            )
            .unwrap();
        }

        let mut registry = PlatformRegistry::new();
        let loopback = Arc::new(LoopbackPlatform::default());
        registry.register("loopback", loopback.clone());
        let registry = Arc::new(registry);
        let agent_map = mock_agent_map(Arc::new(config), "recovered reply");
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let handle = tokio::spawn(async move {
            run_on_listener_with_parts(
                gateway,
                listener,
                async move {
                    let _ = shutdown_rx.await;
                },
                db,
                registry,
                agent_map,
            )
            .await
        });

        let mut delivered = None;
        for _ in 0..100 {
            let recorded = loopback.recorded().await;
            if let Some(msg) = recorded.first() {
                delivered = Some(msg.clone());
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        let delivered = delivered.expect("recovered loopback reply");
        assert_eq!(delivered.text, "recovered reply");
        assert_eq!(delivered.chat_id, "c");

        let _ = shutdown_tx.send(());
        handle.await.expect("join").expect("gateway exits cleanly");
    }
}
