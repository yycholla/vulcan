use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use crate::config::Config;
use crate::gateway::commands::CommandDispatcher;
use crate::gateway::daemon_client::GatewayDaemonClient;
#[cfg(feature = "discord")]
use crate::gateway::discord::DiscordPlatform;
use crate::gateway::lane::{LaneKey, LaneRouter as PerLaneSerialRouter, from_closure};
use crate::gateway::lane_router::DaemonLaneRouter;
use crate::gateway::loopback::LoopbackPlatform;
use crate::gateway::outbound::OutboundDispatcher;
use crate::gateway::queue::{InboundQueue, InboundRow, OutboundQueue};
use crate::gateway::registry::PlatformRegistry;
use crate::gateway::server::{AppState, build_router};
use anyhow::{Context, Result};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

pub mod commands;
pub mod daemon_client;
#[cfg(feature = "discord")]
pub mod discord;
pub mod lane;
pub mod lane_router;
pub mod loopback;
pub mod outbound;
pub mod queue;
mod queue_turso;
pub mod registry;
pub mod render_registry;
pub mod routes;
pub mod scheduler;
pub mod scheduler_store;
mod scheduler_store_turso;
pub mod server;
#[cfg(feature = "telegram")]
pub mod telegram;
pub mod worker;

pub async fn run(config: &Config, bind_override: Option<String>) -> Result<()> {
    // YYC-145: validate required gateway config before opening any
    // socket. A missing api_token or empty connector token here would
    // otherwise leak a bound listener before run_on_listener noticed.
    let gateway = config
        .gateway
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("missing [gateway] config; set api_token before running"))?;
    gateway.validate()?;
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
    // YYC-145: validate again on the test path. `run` already calls
    // this, but tests drive `run_on_listener` directly with a
    // pre-bound listener and would otherwise skip the check.
    gateway.validate()?;

    // GH #704: one turso Database per file; each store gets its own
    // Connection from it (a shared connection would serialize the
    // worker/dispatcher/scheduler loops and risks the open-statement
    // write-swallowing footgun documented in memory/turso_store.rs).
    let db = {
        // Keep the Turso filename distinct until users intentionally
        // move old gateway data.
        let path = crate::config::vulcan_home().join("gateway.turso.db");
        let database = crate::db::open_database(&path).await?;
        let conn = crate::db::connect_database(&database).await?;
        queue_turso::initialize_gateway_db(&conn).await?;
        database
    };
    let mut registry = PlatformRegistry::new();
    if gateway.loopback {
        registry.register("loopback", Arc::new(LoopbackPlatform::default()));
    }
    #[cfg(feature = "discord")]
    if gateway.discord.enabled {
        registry.register(
            "discord",
            Arc::new(DiscordPlatform::new(&gateway.discord.bot_token)?),
        );
    }
    #[cfg(feature = "telegram")]
    if gateway.telegram.enabled {
        use crate::gateway::telegram::TelegramPlatform;
        let webhook_secret = if gateway.telegram.webhook_secret.is_empty() {
            None
        } else {
            Some(gateway.telegram.webhook_secret.clone())
        };
        registry.register(
            "telegram",
            Arc::new(TelegramPlatform::new(
                gateway.telegram.bot_token.clone(),
                gateway.telegram.allowed_chat_ids.clone(),
                webhook_secret,
            )?),
        );
    }
    let registry = Arc::new(registry);
    let scheduler_config = config.scheduler.clone();
    // YYC-266 Slice 3 Task 3.4: lane → daemon-session routing
    // replaces the in-process per-lane Agent cache. The daemon owns
    // the Agent (one per session, lazy-built on first prompt.run)
    // and the gateway becomes a thin Axum + queue front-end. The
    // gateway.idle_ttl_secs setting is now ignored — daemon-side
    // session eviction handles cleanup.
    let lane_router = Arc::new(DaemonLaneRouter::new());
    let daemon_client = Arc::new(GatewayDaemonClient::new());

    run_on_listener_with_parts(
        gateway,
        scheduler_config,
        listener,
        shutdown,
        db,
        registry,
        lane_router,
        daemon_client,
    )
    .await
}

async fn run_on_listener_with_parts<S>(
    gateway: crate::config::GatewayConfig,
    scheduler_config: crate::config::SchedulerConfig,
    listener: TcpListener,
    shutdown: S,
    db: turso::Database,
    registry: Arc<PlatformRegistry>,
    lane_router: Arc<DaemonLaneRouter>,
    daemon_client: Arc<GatewayDaemonClient>,
) -> Result<()>
where
    S: Future<Output = ()> + Send + 'static,
{
    let inbound = Arc::new(InboundQueue::new(crate::db::connect_database(&db).await?));
    let outbound = Arc::new(OutboundQueue::new(
        crate::db::connect_database(&db).await?,
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

    // Slice 3 Task 3.4: lane eviction is the daemon's job now;
    // the gateway-side per-lane Agent cache (and its evictor) was
    // deleted alongside the daemon-Client port.
    let render_registry = Arc::new(crate::gateway::render_registry::RenderRegistry::new());
    let outbound_dispatcher = OutboundDispatcher::new(
        Arc::clone(&outbound),
        Arc::clone(&registry),
        Arc::clone(&render_registry),
    )
    .spawn();
    #[cfg(feature = "discord")]
    let discord_dispatcher = if gateway.discord.enabled {
        Some(DiscordPlatform::spawn_gateway_client(
            gateway.discord.bot_token.clone(),
            gateway.discord.allow_bots,
            gateway.discord.allowed_guild_ids.clone(),
            gateway.discord.allowed_channel_ids.clone(),
            gateway.discord.require_mention,
            Arc::clone(&inbound),
        )?)
    } else {
        None
    };
    // YYC-18 PR-3: Telegram long-poll runs alongside the Axum server.
    // Mirrors Discord's enable-and-spawn pattern, gated on the
    // `telegram` cargo feature so default builds stay free of
    // teloxide-core.
    #[cfg(feature = "telegram")]
    let telegram_dispatcher = if gateway.telegram.enabled {
        Some(crate::gateway::telegram::TelegramPlatform::spawn_long_poll(
            gateway.telegram.bot_token.clone(),
            gateway.telegram.allowed_chat_ids.clone(),
            gateway.telegram.poll_interval_secs,
            Arc::clone(&inbound),
        )?)
    } else {
        None
    };
    // YYC-17 PR-3/4: the runtime worker path, scheduler loop, and
    // observability route all need the same scheduler_runs backing
    // store. Build it once and clone the cheap handle instead of
    // reconstructing it in three places.
    let scheduler_store = if !scheduler_config.jobs.is_empty() {
        let store = scheduler_store::SchedulerStore::new(crate::db::connect_database(&db).await?);
        // A crash between enqueue and completion leaves a stale
        // active_fires count that would suppress OverlapPolicy::Skip
        // jobs forever; nothing is genuinely in-flight at startup.
        let reset = store.reset_active_fires().await?;
        if reset > 0 {
            tracing::info!(reset, "scheduler reset stale active fire counts");
        }
        Some(store)
    } else {
        None
    };
    // YYC-17 PR-2: spawn the cron scheduler once the inbound queue
    // exists. Validates jobs up front; configuration errors here
    // bubble out before the worker pipeline starts so a bad cron
    // expression can't slip past startup. The handle drops with
    // the function scope so the loop is reaped on shutdown.
    let scheduler_handle = if let Some(store) = scheduler_store.clone() {
        let scheduler = scheduler::Scheduler::from_config_with_store(
            &scheduler_config,
            Arc::clone(&inbound),
            Some(store),
        )?;
        if scheduler.enabled_jobs() > 0 {
            Some(scheduler.spawn())
        } else {
            None
        }
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
        Arc::clone(&lane_router),
        Arc::clone(&daemon_client),
        Arc::clone(&render_registry),
        Arc::clone(&registry),
        scheduler_store.clone(),
        Arc::clone(&commands),
    );

    // YYC-17 PR-4: clone the scheduler config + store handle into
    // AppState so the /v1/scheduler observability route can answer
    // without going through the runtime.
    let scheduler_jobs = Arc::new(scheduler_config.jobs.clone());
    let scheduler_store_for_route = scheduler_store.clone();
    let app = build_router(AppState {
        api_token: Arc::new(gateway.api_token),
        inbound,
        outbound,
        registry,
        lane_router,
        daemon_client,
        scheduler_jobs,
        scheduler_store: scheduler_store_for_route,
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
    drop(scheduler_handle);
    #[cfg(feature = "discord")]
    if let Some(handle) = discord_dispatcher {
        handle.abort();
    }
    #[cfg(feature = "telegram")]
    if let Some(handle) = telegram_dispatcher {
        handle.abort();
    }
    drop(outbound_dispatcher);

    result
}

fn spawn_inbound_dispatcher(
    inbound: Arc<InboundQueue>,
    outbound: Arc<OutboundQueue>,
    lane_router: Arc<DaemonLaneRouter>,
    daemon_client: Arc<GatewayDaemonClient>,
    render_registry: Arc<crate::gateway::render_registry::RenderRegistry>,
    platform_registry: Arc<PlatformRegistry>,
    scheduler_store: Option<scheduler_store::SchedulerStore>,
    commands: Arc<CommandDispatcher>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        // YYC-146: worker lifecycle is observable via tracing so a stuck
        // dispatcher (e.g. claim_next failing every tick) is visible
        // without inspecting queue state directly.
        tracing::info!(target: "gateway::worker", "inbound dispatcher started");
        let handler_inbound = Arc::clone(&inbound);
        let handler_outbound = Arc::clone(&outbound);
        let handler_lane_router = Arc::clone(&lane_router);
        let handler_daemon_client = Arc::clone(&daemon_client);
        let handler_render_registry = Arc::clone(&render_registry);
        let handler_platform_registry = Arc::clone(&platform_registry);
        let handler_commands = Arc::clone(&commands);
        let handler = from_closure(move |_lane: LaneKey, row: InboundRow| {
            let inbound = Arc::clone(&handler_inbound);
            let outbound = Arc::clone(&handler_outbound);
            let lane_router = Arc::clone(&handler_lane_router);
            let daemon_client = Arc::clone(&handler_daemon_client);
            let render_registry = Arc::clone(&handler_render_registry);
            let platform_registry = Arc::clone(&handler_platform_registry);
            let commands = Arc::clone(&handler_commands);
            let scheduler_store = scheduler_store.clone();
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
                    &lane_router,
                    &daemon_client,
                    &inbound,
                    &outbound,
                    &render_registry,
                    caps,
                    scheduler_store.as_ref(),
                    &commands,
                )
                .await
                {
                    tracing::error!(target: "gateway::inbound", error = %e, "inbound row failed");
                }
            }
        });
        let router: PerLaneSerialRouter<InboundRow> = PerLaneSerialRouter::new(handler);
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
