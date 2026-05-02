//! Shared Vulcan observability foundation.
//!
//! This module owns the reusable naming, configuration-derived OTLP endpoints,
//! subscriber initialization, and snapshot shapes used by daemon, CLI, TUI,
//! gateway, hooks, tools, providers, and Symphony.

use std::time::Duration;

use anyhow::{Context, Result};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{
    Resource,
    metrics::{PeriodicReader, SdkMeterProvider},
    trace::{RandomIdGenerator, Sampler, SdkTracerProvider},
};
use serde::Serialize;
use tracing_subscriber::{EnvFilter, Layer, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use crate::config::ObservabilityConfig;
use crate::hooks::{HookFailureCounts, HookRegistry};

pub mod attr {
    pub const SESSION_ID: &str = "session_id";
    pub const RUN_ID: &str = "run_id";
    pub const TASK_ID: &str = "task_id";
    pub const TASK_IDENTIFIER: &str = "task_identifier";
    pub const TOOL_NAME: &str = "tool_name";
    pub const PROVIDER: &str = "provider";
    pub const MODEL: &str = "model";
    pub const HOOK_HANDLER: &str = "hook_handler";
    pub const OUTCOME: &str = "outcome";
    pub const ERROR_KIND: &str = "error_kind";
    pub const SURFACE: &str = "surface";
}

pub mod span {
    pub const PROCESS: &str = "vulcan.process";
    pub const AGENT_SESSION: &str = "vulcan.agent.session";
    pub const AGENT_TURN: &str = "vulcan.agent.turn";
    pub const HOOK_EVENT: &str = "vulcan.hook.event";
    pub const TOOL_CALL: &str = "vulcan.tool.call";
    pub const PROVIDER_REQUEST: &str = "vulcan.provider.request";
    pub const DAEMON_REQUEST: &str = "vulcan.daemon.request";
    pub const GATEWAY_MESSAGE: &str = "vulcan.gateway.message";
    pub const TASK_ORCHESTRATION: &str = "vulcan.task.orchestration";
}

pub mod metric {
    pub const HOOK_ERRORS: &str = "vulcan.hooks.errors";
    pub const HOOK_TIMEOUTS: &str = "vulcan.hooks.timeouts";
    pub const HOOK_HANDLERS: &str = "vulcan.hooks.handlers";
    pub const TOKENS_INPUT: &str = "vulcan.tokens.input";
    pub const TOKENS_OUTPUT: &str = "vulcan.tokens.output";
    pub const RUNTIME_SECONDS: &str = "vulcan.runtime.seconds";
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HookHealthSnapshot {
    pub handler_count: usize,
    pub errors: usize,
    pub timeouts: usize,
}

impl HookHealthSnapshot {
    pub fn from_counts(handler_count: usize, counts: HookFailureCounts) -> Self {
        Self {
            handler_count,
            errors: counts.errors,
            timeouts: counts.timeouts,
        }
    }

    pub fn from_registry(registry: &HookRegistry) -> Self {
        Self::from_counts(registry.handler_count(), registry.failure_metrics())
    }
}

#[derive(Debug)]
pub struct ObservabilityGuard {
    tracer_provider: Option<SdkTracerProvider>,
    meter_provider: Option<SdkMeterProvider>,
}

impl ObservabilityGuard {
    pub fn disabled() -> Self {
        Self {
            tracer_provider: None,
            meter_provider: None,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.tracer_provider.is_some() || self.meter_provider.is_some()
    }
}

impl Drop for ObservabilityGuard {
    fn drop(&mut self) {
        if let Some(provider) = self.tracer_provider.take()
            && let Err(err) = provider.shutdown()
        {
            eprintln!("OpenTelemetry tracer shutdown failed: {err:?}");
        }
        if let Some(provider) = self.meter_provider.take()
            && let Err(err) = provider.shutdown()
        {
            eprintln!("OpenTelemetry meter shutdown failed: {err:?}");
        }
    }
}

pub fn trace_endpoint(config: &ObservabilityConfig) -> String {
    config
        .traces_endpoint
        .clone()
        .or_else(|| {
            config
                .endpoint
                .as_ref()
                .map(|base| otlp_path(base, "traces"))
        })
        .unwrap_or_else(|| "http://localhost:4318/v1/traces".to_string())
}

pub fn metrics_endpoint(config: &ObservabilityConfig) -> String {
    config
        .metrics_endpoint
        .clone()
        .or_else(|| {
            config
                .endpoint
                .as_ref()
                .map(|base| otlp_path(base, "metrics"))
        })
        .unwrap_or_else(|| "http://localhost:4318/v1/metrics".to_string())
}

fn otlp_path(base: &str, suffix: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    if trimmed.ends_with("/v1/traces") || trimmed.ends_with("/v1/metrics") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/v1/{suffix}")
    }
}

pub fn init_with_writer<W>(
    config: &ObservabilityConfig,
    filter: EnvFilter,
    writer: W,
    ansi: bool,
) -> Result<ObservabilityGuard>
where
    W: for<'writer> fmt::MakeWriter<'writer> + Send + Sync + 'static,
{
    let fmt_layer = fmt::layer().with_writer(writer).with_ansi(ansi);
    if !config.enabled {
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt_layer)
            .try_init()
            .context("initializing tracing subscriber")?;
        return Ok(ObservabilityGuard::disabled());
    }

    let (otel_trace_layer, tracer_provider) = if config.traces {
        let provider = init_tracer_provider(config)?;
        let tracer = provider.tracer(config.service_name.clone());
        (
            Some(tracing_opentelemetry::layer().with_tracer(tracer).boxed()),
            Some(provider),
        )
    } else {
        (None, None)
    };

    let (otel_metrics_layer, meter_provider) = if config.metrics {
        let provider = init_meter_provider(config)?;
        (
            Some(tracing_opentelemetry::MetricsLayer::new(provider.clone()).boxed()),
            Some(provider),
        )
    } else {
        (None, None)
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .with(otel_trace_layer)
        .with(otel_metrics_layer)
        .try_init()
        .context("initializing tracing subscriber")?;

    Ok(ObservabilityGuard {
        tracer_provider,
        meter_provider,
    })
}

fn resource(config: &ObservabilityConfig) -> Resource {
    Resource::builder()
        .with_service_name(config.service_name.clone())
        .build()
}

fn init_tracer_provider(config: &ObservabilityConfig) -> Result<SdkTracerProvider> {
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(trace_endpoint(config))
        .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
        .build()
        .context("building OTLP trace exporter")?;

    Ok(SdkTracerProvider::builder()
        .with_sampler(Sampler::ParentBased(Box::new(Sampler::AlwaysOn)))
        .with_id_generator(RandomIdGenerator::default())
        .with_resource(resource(config))
        .with_batch_exporter(exporter)
        .build())
}

fn init_meter_provider(config: &ObservabilityConfig) -> Result<SdkMeterProvider> {
    let exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .with_endpoint(metrics_endpoint(config))
        .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
        .build()
        .context("building OTLP metrics exporter")?;

    let reader = PeriodicReader::builder(exporter)
        .with_interval(Duration::from_secs(config.export_interval_secs.max(1)))
        .build();

    Ok(SdkMeterProvider::builder()
        .with_resource(resource(config))
        .with_reader(reader)
        .build())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ObservabilityConfig, ObservabilitySurfaceConfig};

    #[test]
    fn disabled_guard_reports_disabled() {
        assert!(!ObservabilityGuard::disabled().is_enabled());
    }

    #[test]
    fn endpoint_helpers_derive_signal_paths_from_base_endpoint() {
        let config = ObservabilityConfig {
            enabled: true,
            endpoint: Some("http://collector:4318".into()),
            ..ObservabilityConfig::default()
        };

        assert_eq!(trace_endpoint(&config), "http://collector:4318/v1/traces");
        assert_eq!(
            metrics_endpoint(&config),
            "http://collector:4318/v1/metrics"
        );
    }

    #[test]
    fn explicit_signal_endpoints_override_base_endpoint() {
        let config = ObservabilityConfig {
            endpoint: Some("http://collector:4318".into()),
            traces_endpoint: Some("http://traces/v1/traces".into()),
            metrics_endpoint: Some("http://metrics/v1/metrics".into()),
            ..ObservabilityConfig::default()
        };

        assert_eq!(trace_endpoint(&config), "http://traces/v1/traces");
        assert_eq!(metrics_endpoint(&config), "http://metrics/v1/metrics");
    }

    #[test]
    fn stable_attribute_and_span_names_cover_runtime_surfaces() {
        let attrs = [
            attr::SESSION_ID,
            attr::RUN_ID,
            attr::TASK_ID,
            attr::TASK_IDENTIFIER,
            attr::TOOL_NAME,
            attr::PROVIDER,
            attr::MODEL,
            attr::HOOK_HANDLER,
            attr::OUTCOME,
            attr::ERROR_KIND,
        ];
        assert!(attrs.iter().all(|name| !name.is_empty()));
        assert!(attrs.iter().all(|name| !name.contains('.')));
        assert_eq!(span::PROCESS, "vulcan.process");
        assert_eq!(span::AGENT_SESSION, "vulcan.agent.session");
        assert_eq!(span::TASK_ORCHESTRATION, "vulcan.task.orchestration");
    }

    #[test]
    fn surface_config_defaults_to_full_supported_surface() {
        let surfaces = ObservabilitySurfaceConfig::default();
        assert!(surfaces.agent);
        assert!(surfaces.hooks);
        assert!(surfaces.tools);
        assert!(surfaces.provider);
        assert!(surfaces.daemon);
        assert!(surfaces.gateway);
        assert!(surfaces.symphony);
    }

    #[test]
    fn hook_health_snapshot_preserves_failure_counts() {
        let counts = HookFailureCounts {
            errors: 2,
            timeouts: 3,
        };
        assert_eq!(
            HookHealthSnapshot::from_counts(4, counts),
            HookHealthSnapshot {
                handler_count: 4,
                errors: 2,
                timeouts: 3,
            }
        );
    }

    #[test]
    fn observability_config_parses_full_surface_defaults() {
        let parsed: crate::config::Config = toml::from_str(
            r#"
            [observability]
            enabled = true
            endpoint = "http://localhost:4318"
            "#,
        )
        .expect("parse config");

        assert!(parsed.observability.enabled);
        assert!(parsed.observability.traces);
        assert!(parsed.observability.metrics);
        assert!(!parsed.observability.logs);
        assert_eq!(
            trace_endpoint(&parsed.observability),
            "http://localhost:4318/v1/traces"
        );
        assert!(parsed.observability.surfaces.provider);
        assert!(parsed.observability.surfaces.symphony);
    }
}
