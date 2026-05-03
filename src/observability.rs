//! Shared Vulcan observability foundation.
//!
//! This module owns the reusable naming, configuration-derived OTLP endpoints,
//! subscriber initialization, and snapshot shapes used by daemon, CLI, TUI,
//! gateway, hooks, tools, providers, and Symphony.

use std::{sync::LazyLock, time::Duration};

use anyhow::{Context, Result};
use opentelemetry::{
    KeyValue, global,
    metrics::{Counter, Histogram},
    trace::TracerProvider as _,
};
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
    pub const OPERATION: &str = "operation";
    pub const REQUEST_ID: &str = "request_id";
    pub const SESSION_ID: &str = "session_id";
    pub const RUN_ID: &str = "run_id";
    pub const TASK_ID: &str = "task_id";
    pub const TASK_IDENTIFIER: &str = "task_identifier";
    pub const TOOL_NAME: &str = "tool_name";
    pub const PROVIDER: &str = "provider";
    pub const PROVIDER_MODE: &str = "provider_mode";
    pub const RPC_METHOD: &str = "rpc_method";
    pub const MODEL: &str = "model";
    pub const STREAMING: &str = "streaming";
    pub const MESSAGE_COUNT: &str = "message_count";
    pub const TOOL_COUNT: &str = "tool_count";
    pub const PROMPT_TOKENS: &str = "prompt_tokens";
    pub const COMPLETION_TOKENS: &str = "completion_tokens";
    pub const TOTAL_TOKENS: &str = "total_tokens";
    pub const HOOK_HANDLER: &str = "hook_handler";
    pub const HOOK_EVENT: &str = "hook_event";
    pub const OUTCOME: &str = "outcome";
    pub const ERROR_KIND: &str = "error_kind";
    pub const SURFACE: &str = "surface";
}

pub mod span {
    pub const PROCESS: &str = "vulcan.process";
    pub const AGENT_SESSION: &str = "vulcan.agent.session";
    pub const AGENT_TURN: &str = "vulcan.agent.turn";
    pub const HOOK_EVENT: &str = "vulcan.hook.event";
    pub const HOOK_AFTER_TOOL_CALL: &str = "vulcan.hook.after_tool_call";
    pub const HOOK_BEFORE_AGENT_END: &str = "vulcan.hook.before_agent_end";
    pub const HOOK_BEFORE_PROMPT: &str = "vulcan.hook.before_prompt";
    pub const HOOK_BEFORE_TOOL_CALL: &str = "vulcan.hook.before_tool_call";
    pub const HOOK_ON_AFTER_PROVIDER_RESPONSE: &str = "vulcan.hook.on_after_provider_response";
    pub const HOOK_ON_BEFORE_PROVIDER_REQUEST: &str = "vulcan.hook.on_before_provider_request";
    pub const HOOK_ON_CONTEXT: &str = "vulcan.hook.on_context";
    pub const HOOK_ON_INPUT: &str = "vulcan.hook.on_input";
    pub const HOOK_ON_MESSAGE_END: &str = "vulcan.hook.on_message_end";
    pub const HOOK_ON_MESSAGE_START: &str = "vulcan.hook.on_message_start";
    pub const HOOK_ON_MESSAGE_UPDATE: &str = "vulcan.hook.on_message_update";
    pub const HOOK_ON_SESSION_BEFORE_COMPACT: &str = "vulcan.hook.on_session_before_compact";
    pub const HOOK_ON_SESSION_BEFORE_FORK: &str = "vulcan.hook.on_session_before_fork";
    pub const HOOK_ON_SESSION_COMPACT: &str = "vulcan.hook.on_session_compact";
    pub const HOOK_ON_SESSION_SHUTDOWN: &str = "vulcan.hook.on_session_shutdown";
    pub const HOOK_ON_TOOL_EXECUTION_END: &str = "vulcan.hook.on_tool_execution_end";
    pub const HOOK_ON_TOOL_EXECUTION_START: &str = "vulcan.hook.on_tool_execution_start";
    pub const HOOK_ON_TOOL_EXECUTION_UPDATE: &str = "vulcan.hook.on_tool_execution_update";
    pub const HOOK_ON_TURN_END: &str = "vulcan.hook.on_turn_end";
    pub const HOOK_ON_TURN_START: &str = "vulcan.hook.on_turn_start";
    pub const HOOK_SESSION_END: &str = "vulcan.hook.session_end";
    pub const HOOK_SESSION_START: &str = "vulcan.hook.session_start";
    pub const TOOL_CALL: &str = "vulcan.tool.call";
    pub const PROVIDER_REQUEST: &str = "vulcan.provider.request";
    pub const PROVIDER_BUFFERED: &str = "vulcan.provider.buffered";
    pub const PROVIDER_COMPACTION: &str = "vulcan.provider.compaction";
    pub const PROVIDER_STREAMING: &str = "vulcan.provider.streaming";
    pub const DAEMON_REQUEST: &str = "vulcan.daemon.request";
    pub const GATEWAY_MESSAGE: &str = "vulcan.gateway.message";
    pub const TASK_ORCHESTRATION: &str = "vulcan.task.orchestration";
}

pub mod metric {
    pub const DAEMON_REQUEST_DURATION_MS: &str = "vulcan.daemon.request.duration_ms";
    pub const PROVIDER_REQUEST_DURATION_MS: &str = "vulcan.provider.request.duration_ms";
    pub const TOOL_CALL_DURATION_MS: &str = "vulcan.tool.call.duration_ms";
    pub const HOOK_EVENT_DURATION_MS: &str = "vulcan.hook.event.duration_ms";
    pub const HOOK_ERRORS: &str = "vulcan.hooks.errors";
    pub const HOOK_TIMEOUTS: &str = "vulcan.hooks.timeouts";
    pub const HOOK_HANDLERS: &str = "vulcan.hooks.handlers";
    pub const TOKENS_INPUT: &str = "vulcan.tokens.input";
    pub const TOKENS_OUTPUT: &str = "vulcan.tokens.output";
    pub const TOKENS_TOTAL: &str = "vulcan.tokens.total";
    pub const ERRORS_TOTAL: &str = "vulcan.errors.total";
    pub const TUI_FRAME_DRAW_MS: &str = "vulcan.tui.frame.draw_ms";
    pub const TUI_FRAME_INTERVAL_MS: &str = "vulcan.tui.frame.interval_ms";
    pub const TUI_FRAMES_TOTAL: &str = "vulcan.tui.frames.total";
    pub const TUI_FPS: &str = "vulcan.tui.fps";
    pub const TUI_SURFACE_COUNT: &str = "vulcan.tui.surface.count";
    pub const PROCESS_MEMORY_RSS_BYTES: &str = "vulcan.process.memory.rss_bytes";
    pub const PROCESS_CPU_PERCENT: &str = "vulcan.process.cpu.percent";
    pub const PROCESS_THREADS: &str = "vulcan.process.threads";
    pub const RUNTIME_SECONDS: &str = "vulcan.runtime.seconds";

    pub const RUNTIME_PERFORMANCE: &[&str] = &[
        DAEMON_REQUEST_DURATION_MS,
        PROVIDER_REQUEST_DURATION_MS,
        TOOL_CALL_DURATION_MS,
        HOOK_EVENT_DURATION_MS,
        TOKENS_INPUT,
        TOKENS_OUTPUT,
        TOKENS_TOTAL,
        ERRORS_TOTAL,
        TUI_FRAME_DRAW_MS,
        TUI_FRAME_INTERVAL_MS,
        TUI_FRAMES_TOTAL,
        TUI_FPS,
        TUI_SURFACE_COUNT,
        PROCESS_MEMORY_RSS_BYTES,
        PROCESS_CPU_PERCENT,
        PROCESS_THREADS,
    ];
}

struct RuntimeMetricInstruments {
    daemon_request_duration_ms: Histogram<f64>,
    provider_request_duration_ms: Histogram<f64>,
    tool_call_duration_ms: Histogram<f64>,
    hook_event_duration_ms: Histogram<f64>,
    tokens_input: Counter<u64>,
    tokens_output: Counter<u64>,
    tokens_total: Counter<u64>,
    errors_total: Counter<u64>,
    hook_errors: Counter<u64>,
    hook_timeouts: Counter<u64>,
}

static RUNTIME_METRICS: LazyLock<RuntimeMetricInstruments> = LazyLock::new(|| {
    let meter = global::meter("vulcan");
    RuntimeMetricInstruments {
        daemon_request_duration_ms: meter
            .f64_histogram(metric::DAEMON_REQUEST_DURATION_MS)
            .with_unit("ms")
            .with_description("Daemon request latency.")
            .build(),
        provider_request_duration_ms: meter
            .f64_histogram(metric::PROVIDER_REQUEST_DURATION_MS)
            .with_unit("ms")
            .with_description("Provider request latency.")
            .build(),
        tool_call_duration_ms: meter
            .f64_histogram(metric::TOOL_CALL_DURATION_MS)
            .with_unit("ms")
            .with_description("Tool call latency.")
            .build(),
        hook_event_duration_ms: meter
            .f64_histogram(metric::HOOK_EVENT_DURATION_MS)
            .with_unit("ms")
            .with_description("Hook handler event latency.")
            .build(),
        tokens_input: meter
            .u64_counter(metric::TOKENS_INPUT)
            .with_unit("tokens")
            .with_description("Provider prompt tokens.")
            .build(),
        tokens_output: meter
            .u64_counter(metric::TOKENS_OUTPUT)
            .with_unit("tokens")
            .with_description("Provider completion tokens.")
            .build(),
        tokens_total: meter
            .u64_counter(metric::TOKENS_TOTAL)
            .with_unit("tokens")
            .with_description("Provider total tokens.")
            .build(),
        errors_total: meter
            .u64_counter(metric::ERRORS_TOTAL)
            .with_description("Runtime errors by surface and kind.")
            .build(),
        hook_errors: meter
            .u64_counter(metric::HOOK_ERRORS)
            .with_description("Hook handler errors.")
            .build(),
        hook_timeouts: meter
            .u64_counter(metric::HOOK_TIMEOUTS)
            .with_description("Hook handler timeouts.")
            .build(),
    }
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderSpanMode {
    Buffered,
    Compaction,
    Streaming,
}

impl ProviderSpanMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Buffered => "buffered",
            Self::Compaction => "compaction",
            Self::Streaming => "streaming",
        }
    }

    pub fn operation(self) -> &'static str {
        match self {
            Self::Buffered => "provider.buffered",
            Self::Compaction => "provider.compaction",
            Self::Streaming => "provider.streaming",
        }
    }

    pub fn streaming(self) -> bool {
        matches!(self, Self::Streaming)
    }
}

pub fn daemon_request_operation(method: &str) -> String {
    format!("request.{method}")
}

pub fn daemon_request_span(method: &str, session_id: &str, request_id: &str) -> tracing::Span {
    let operation = daemon_request_operation(method);
    tracing::info_span!(
        span::DAEMON_REQUEST,
        surface = "daemon",
        operation = operation.as_str(),
        rpc_method = method,
        session_id,
        request_id,
        outcome = tracing::field::Empty,
        error_kind = tracing::field::Empty
    )
}

pub fn tool_call_span(tool_name: &str) -> tracing::Span {
    let operation = format!("tool.{tool_name}");
    tracing::info_span!(
        span::TOOL_CALL,
        surface = "tools",
        operation = operation.as_str(),
        tool_name,
        outcome = tracing::field::Empty,
        error_kind = tracing::field::Empty
    )
}

pub fn provider_request_span(
    provider: &str,
    model: &str,
    mode: ProviderSpanMode,
    message_count: usize,
    tool_count: usize,
) -> tracing::Span {
    macro_rules! span {
        ($name:expr) => {
            tracing::info_span!(
                $name,
                surface = "provider",
                operation = mode.operation(),
                provider,
                provider_mode = mode.as_str(),
                model,
                streaming = mode.streaming(),
                message_count = message_count as u64,
                tool_count = tool_count as u64,
                outcome = tracing::field::Empty,
                error_kind = tracing::field::Empty,
                prompt_tokens = tracing::field::Empty,
                completion_tokens = tracing::field::Empty,
                total_tokens = tracing::field::Empty
            )
        };
    }

    match mode {
        ProviderSpanMode::Buffered => span!(span::PROVIDER_BUFFERED),
        ProviderSpanMode::Compaction => span!(span::PROVIDER_COMPACTION),
        ProviderSpanMode::Streaming => span!(span::PROVIDER_STREAMING),
    }
}

fn duration_millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn outcome_metric_attrs(
    surface: &'static str,
    outcome: &str,
    error_kind: Option<&str>,
) -> Vec<KeyValue> {
    let mut attrs = vec![
        KeyValue::new(attr::SURFACE, surface),
        KeyValue::new(attr::OUTCOME, outcome.to_string()),
    ];
    if let Some(kind) = error_kind {
        attrs.push(KeyValue::new(attr::ERROR_KIND, kind.to_string()));
    }
    attrs
}

fn push_outcome_attrs(attrs: &mut Vec<KeyValue>, outcome: &str, error_kind: Option<&str>) {
    attrs.push(KeyValue::new(attr::OUTCOME, outcome.to_string()));
    if let Some(kind) = error_kind {
        attrs.push(KeyValue::new(attr::ERROR_KIND, kind.to_string()));
    }
}

fn provider_metric_attrs(provider: &str, model: &str, mode: ProviderSpanMode) -> Vec<KeyValue> {
    vec![
        KeyValue::new(attr::SURFACE, "provider"),
        KeyValue::new(attr::PROVIDER, provider.to_string()),
        KeyValue::new(attr::MODEL, model.to_string()),
        KeyValue::new(attr::PROVIDER_MODE, mode.as_str()),
        KeyValue::new(attr::STREAMING, mode.streaming()),
    ]
}

pub fn record_provider_request_metrics(
    provider: &str,
    model: &str,
    mode: ProviderSpanMode,
    duration: Duration,
    outcome: &str,
    error_kind: Option<&str>,
) {
    let mut attrs = provider_metric_attrs(provider, model, mode);
    push_outcome_attrs(&mut attrs, outcome, error_kind);
    RUNTIME_METRICS
        .provider_request_duration_ms
        .record(duration_millis(duration), &attrs);
    if let Some(kind) = error_kind {
        record_error_metric("provider", kind);
    }
}

pub fn record_provider_token_metrics(
    provider: &str,
    model: &str,
    mode: ProviderSpanMode,
    prompt_tokens: usize,
    completion_tokens: usize,
    total_tokens: usize,
) {
    let attrs = provider_metric_attrs(provider, model, mode);
    RUNTIME_METRICS
        .tokens_input
        .add(prompt_tokens as u64, &attrs);
    RUNTIME_METRICS
        .tokens_output
        .add(completion_tokens as u64, &attrs);
    RUNTIME_METRICS
        .tokens_total
        .add(total_tokens as u64, &attrs);
}

pub fn record_tool_call_metrics(
    tool_name: &str,
    duration: Duration,
    outcome: &str,
    error_kind: Option<&str>,
) {
    let mut attrs = vec![
        KeyValue::new(attr::SURFACE, "tools"),
        KeyValue::new(attr::TOOL_NAME, tool_name.to_string()),
    ];
    push_outcome_attrs(&mut attrs, outcome, error_kind);
    RUNTIME_METRICS
        .tool_call_duration_ms
        .record(duration_millis(duration), &attrs);
    if let Some(kind) = error_kind {
        record_error_metric("tools", kind);
    }
}

pub fn record_hook_event_metrics(
    event: &'static str,
    handler: &str,
    duration: Duration,
    outcome: &str,
    error_kind: Option<&str>,
) {
    let mut attrs = vec![
        KeyValue::new(attr::SURFACE, "hooks"),
        KeyValue::new(attr::HOOK_EVENT, event),
        KeyValue::new(attr::HOOK_HANDLER, handler.to_string()),
    ];
    push_outcome_attrs(&mut attrs, outcome, error_kind);
    RUNTIME_METRICS
        .hook_event_duration_ms
        .record(duration_millis(duration), &attrs);
    match error_kind {
        Some("handler_timeout") => {
            RUNTIME_METRICS.hook_timeouts.add(1, &attrs);
            record_error_metric("hooks", "handler_timeout");
        }
        Some(kind) => {
            RUNTIME_METRICS.hook_errors.add(1, &attrs);
            record_error_metric("hooks", kind);
        }
        None => {}
    }
}

pub fn record_daemon_request_metrics(
    method: &str,
    duration: Duration,
    outcome: &str,
    error_kind: Option<&str>,
) {
    let operation = daemon_request_operation(method);
    let mut attrs = vec![
        KeyValue::new(attr::SURFACE, "daemon"),
        KeyValue::new(attr::RPC_METHOD, method.to_string()),
        KeyValue::new(attr::OPERATION, operation),
    ];
    push_outcome_attrs(&mut attrs, outcome, error_kind);
    RUNTIME_METRICS
        .daemon_request_duration_ms
        .record(duration_millis(duration), &attrs);
    if let Some(kind) = error_kind {
        record_error_metric("daemon", kind);
    }
}

pub fn record_error_metric(surface: &'static str, error_kind: &str) {
    let attrs = outcome_metric_attrs(surface, "error", Some(error_kind));
    RUNTIME_METRICS.errors_total.add(1, &attrs);
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
        global::set_meter_provider(provider.clone());
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
            attr::OPERATION,
            attr::REQUEST_ID,
            attr::SESSION_ID,
            attr::RUN_ID,
            attr::TASK_ID,
            attr::TASK_IDENTIFIER,
            attr::TOOL_NAME,
            attr::PROVIDER,
            attr::PROVIDER_MODE,
            attr::RPC_METHOD,
            attr::MODEL,
            attr::STREAMING,
            attr::MESSAGE_COUNT,
            attr::TOOL_COUNT,
            attr::PROMPT_TOKENS,
            attr::COMPLETION_TOKENS,
            attr::TOTAL_TOKENS,
            attr::HOOK_HANDLER,
            attr::HOOK_EVENT,
            attr::OUTCOME,
            attr::ERROR_KIND,
            attr::SURFACE,
        ];
        assert!(attrs.iter().all(|name| !name.is_empty()));
        assert!(attrs.iter().all(|name| !name.contains('.')));
        assert_eq!(span::PROCESS, "vulcan.process");
        assert_eq!(span::AGENT_SESSION, "vulcan.agent.session");
        assert_eq!(span::HOOK_ON_CONTEXT, "vulcan.hook.on_context");
        assert_eq!(
            span::HOOK_ON_BEFORE_PROVIDER_REQUEST,
            "vulcan.hook.on_before_provider_request"
        );
        assert_eq!(span::TOOL_CALL, "vulcan.tool.call");
        assert_eq!(span::DAEMON_REQUEST, "vulcan.daemon.request");
        assert_eq!(span::PROVIDER_BUFFERED, "vulcan.provider.buffered");
        assert_eq!(span::PROVIDER_COMPACTION, "vulcan.provider.compaction");
        assert_eq!(span::PROVIDER_STREAMING, "vulcan.provider.streaming");
        assert_eq!(span::TASK_ORCHESTRATION, "vulcan.task.orchestration");
    }

    #[test]
    fn stable_metric_names_cover_runtime_performance() {
        assert!(
            metric::RUNTIME_PERFORMANCE
                .iter()
                .all(|name| name.starts_with("vulcan."))
        );
        assert!(
            metric::RUNTIME_PERFORMANCE
                .iter()
                .all(|name| !name.is_empty())
        );
        assert_eq!(
            metric::DAEMON_REQUEST_DURATION_MS,
            "vulcan.daemon.request.duration_ms"
        );
        assert_eq!(metric::TUI_FRAME_DRAW_MS, "vulcan.tui.frame.draw_ms");
        assert_eq!(
            metric::PROCESS_MEMORY_RSS_BYTES,
            "vulcan.process.memory.rss_bytes"
        );
    }

    #[test]
    fn provider_metric_attributes_stay_low_cardinality() {
        let attrs = provider_metric_attrs("openai", "gpt-5.4", ProviderSpanMode::Streaming);
        assert!(attrs.iter().any(|kv| kv.key.as_str() == attr::SURFACE));
        assert!(attrs.iter().any(|kv| kv.key.as_str() == attr::PROVIDER));
        assert!(
            attrs
                .iter()
                .any(|kv| kv.key.as_str() == attr::PROVIDER_MODE)
        );
        assert!(attrs.iter().any(|kv| kv.key.as_str() == attr::STREAMING));
        assert!(
            attrs.iter().all(
                |kv| kv.key.as_str() != attr::REQUEST_ID && kv.key.as_str() != attr::SESSION_ID
            )
        );
    }

    #[test]
    fn outcome_metric_attributes_include_error_kind_only_for_errors() {
        let ok_attrs = outcome_metric_attrs("tools", "ok", None);
        assert!(ok_attrs.iter().any(|kv| kv.key.as_str() == attr::OUTCOME));
        assert!(
            !ok_attrs
                .iter()
                .any(|kv| kv.key.as_str() == attr::ERROR_KIND)
        );

        let error_attrs = outcome_metric_attrs("provider", "error", Some("provider_error"));
        assert!(
            error_attrs
                .iter()
                .any(|kv| kv.key.as_str() == attr::ERROR_KIND)
        );
    }

    #[test]
    fn provider_span_modes_are_stable_for_explorer_grouping() {
        assert_eq!(ProviderSpanMode::Buffered.as_str(), "buffered");
        assert_eq!(ProviderSpanMode::Buffered.operation(), "provider.buffered");
        assert!(!ProviderSpanMode::Buffered.streaming());

        assert_eq!(ProviderSpanMode::Compaction.as_str(), "compaction");
        assert_eq!(
            ProviderSpanMode::Compaction.operation(),
            "provider.compaction"
        );
        assert!(!ProviderSpanMode::Compaction.streaming());

        assert_eq!(ProviderSpanMode::Streaming.as_str(), "streaming");
        assert_eq!(
            ProviderSpanMode::Streaming.operation(),
            "provider.streaming"
        );
        assert!(ProviderSpanMode::Streaming.streaming());
    }

    #[test]
    fn daemon_request_operations_are_stable_for_explorer_grouping() {
        assert_eq!(
            daemon_request_operation("prompt.stream"),
            "request.prompt.stream"
        );
        assert_eq!(
            daemon_request_operation("daemon.status"),
            "request.daemon.status"
        );
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
