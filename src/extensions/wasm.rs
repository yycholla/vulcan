//! GH issue #273: optional Wasmtime-backed extension runtime.
//!
//! The host ABI is deliberately tiny for v1:
//!
//! - `vulcan_host.log(ptr, len)`
//! - `vulcan_host.register_tool(ptr, len)`
//!
//! Both read UTF-8 from the module's exported `memory`. Tool
//! registration is policy-gated before the host records it.

use std::sync::Arc;

use parking_lot::Mutex;
use wasmtime::{
    Caller, Config, Engine, Error as WasmtimeError, Linker, Module, Result as WasmtimeResult,
    Store, StoreLimits, StoreLimitsBuilder, TypedFunc,
};

use super::runtime::{
    ExtensionRuntime, ExtensionRuntimeCapability, ExtensionRuntimeCtx, ExtensionRuntimeDecision,
    ExtensionRuntimeError, ExtensionRuntimeInit, ExtensionRuntimeKind, ExtensionRuntimeLimits,
};

pub struct WasmExtensionRuntime {
    module_bytes: Arc<[u8]>,
    limits: ExtensionRuntimeLimits,
}

impl WasmExtensionRuntime {
    pub fn from_bytes(bytes: impl Into<Vec<u8>>, limits: ExtensionRuntimeLimits) -> Self {
        Self {
            module_bytes: Arc::<[u8]>::from(bytes.into()),
            limits,
        }
    }
}

#[async_trait::async_trait]
impl ExtensionRuntime for WasmExtensionRuntime {
    fn kind(&self) -> ExtensionRuntimeKind {
        ExtensionRuntimeKind::Wasm
    }

    fn limits(&self) -> &ExtensionRuntimeLimits {
        &self.limits
    }

    async fn initialize(
        &self,
        ctx: ExtensionRuntimeCtx,
    ) -> Result<ExtensionRuntimeInit, ExtensionRuntimeError> {
        initialize_sync(self.module_bytes.clone(), self.limits.clone(), ctx)
    }
}

fn initialize_sync(
    module_bytes: Arc<[u8]>,
    limits: ExtensionRuntimeLimits,
    ctx: ExtensionRuntimeCtx,
) -> Result<ExtensionRuntimeInit, ExtensionRuntimeError> {
    let extension_id = ctx.extension_id.clone();
    let mut config = Config::new();
    config.consume_fuel(true);
    config.epoch_interruption(true);
    let engine = Engine::new(&config).map_err(|err| ExtensionRuntimeError::LoadFailed {
        extension_id: extension_id.clone(),
        reason: err.to_string(),
    })?;
    let module =
        Module::new(&engine, &module_bytes).map_err(|err| ExtensionRuntimeError::LoadFailed {
            extension_id: extension_id.clone(),
            reason: err.to_string(),
        })?;

    let mut store = Store::new(
        &engine,
        WasmHostState::new(
            ctx,
            limits.clone(),
            StoreLimitsBuilder::new()
                .memory_size(limits.max_memory_bytes)
                .build(),
        ),
    );
    store
        .set_fuel(limits.fuel)
        .map_err(|err| ExtensionRuntimeError::LimitExceeded {
            extension_id: extension_id.clone(),
            limit: format!("fuel unavailable: {err}"),
        })?;
    store.set_epoch_deadline(1);
    store.limiter(|state| &mut state.store_limits);

    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "vulcan_host",
            "log",
            |mut caller: Caller<'_, WasmHostState>, ptr: i32, len: i32| -> WasmtimeResult<()> {
                let _message = read_guest_string(&mut caller, ptr, len)?;
                caller
                    .data_mut()
                    .record_host_call(ExtensionRuntimeCapability::Log)
            },
        )
        .map_err(|err| ExtensionRuntimeError::LoadFailed {
            extension_id: extension_id.clone(),
            reason: err.to_string(),
        })?;
    linker
        .func_wrap(
            "vulcan_host",
            "register_tool",
            |mut caller: Caller<'_, WasmHostState>, ptr: i32, len: i32| -> WasmtimeResult<()> {
                let tool_name = read_guest_string(&mut caller, ptr, len)?;
                caller
                    .data_mut()
                    .record_host_call(ExtensionRuntimeCapability::RegisterTool)?;
                caller.data_mut().registered_tools.push(tool_name);
                Ok(())
            },
        )
        .map_err(|err| ExtensionRuntimeError::LoadFailed {
            extension_id: extension_id.clone(),
            reason: err.to_string(),
        })?;

    let instance = linker.instantiate(&mut store, &module).map_err(|err| {
        ExtensionRuntimeError::LoadFailed {
            extension_id: extension_id.clone(),
            reason: err.to_string(),
        }
    })?;
    let init: TypedFunc<(), ()> = instance
        .get_typed_func(&mut store, "_vulcan_init")
        .map_err(|_| ExtensionRuntimeError::MissingExport {
            extension_id: extension_id.clone(),
            export: "_vulcan_init",
        })?;

    let engine_for_timeout = engine.clone();
    let timeout = limits.call_timeout;
    std::thread::spawn(move || {
        std::thread::sleep(timeout);
        engine_for_timeout.increment_epoch();
    });

    init.call(&mut store, ()).map_err(|err| {
        let err_display = err.to_string();
        let err_debug = format!("{err:?}");
        if err_display.contains("all fuel consumed") || err_debug.contains("all fuel consumed") {
            ExtensionRuntimeError::LimitExceeded {
                extension_id: extension_id.clone(),
                limit: "fuel".into(),
            }
        } else if err_display.contains("interrupt")
            || err_display.contains("epoch deadline")
            || err_debug.contains("interrupt")
            || err_debug.contains("epoch deadline")
        {
            ExtensionRuntimeError::LimitExceeded {
                extension_id: extension_id.clone(),
                limit: "timeout".into(),
            }
        } else if let Some(decision) = store.data().last_denied_decision() {
            ExtensionRuntimeError::CapabilityDenied {
                extension_id: extension_id.clone(),
                capability: decision.capability,
                reason: decision
                    .failure_reason
                    .clone()
                    .unwrap_or_else(|| err.to_string()),
            }
        } else {
            ExtensionRuntimeError::Trap {
                extension_id: extension_id.clone(),
                reason: err_display,
            }
        }
    })?;

    let data = store.into_data();
    Ok(ExtensionRuntimeInit {
        extension_id,
        registered_tools: data.registered_tools,
        decisions: data.decisions.into_inner(),
    })
}

struct WasmHostState {
    ctx: ExtensionRuntimeCtx,
    limits: ExtensionRuntimeLimits,
    store_limits: StoreLimits,
    host_call_depth: u32,
    registered_tools: Vec<String>,
    decisions: Mutex<Vec<ExtensionRuntimeDecision>>,
}

impl WasmHostState {
    fn new(
        ctx: ExtensionRuntimeCtx,
        limits: ExtensionRuntimeLimits,
        store_limits: StoreLimits,
    ) -> Self {
        Self {
            ctx,
            limits,
            store_limits,
            host_call_depth: 0,
            registered_tools: Vec::new(),
            decisions: Mutex::new(Vec::new()),
        }
    }

    fn record_host_call(&mut self, capability: ExtensionRuntimeCapability) -> WasmtimeResult<()> {
        if self.host_call_depth >= self.limits.max_host_call_depth {
            return Err(WasmtimeError::msg("max host-call depth exceeded"));
        }
        self.host_call_depth += 1;
        let decision = self.ctx.decide(capability);
        self.decisions.lock().push(decision.clone());
        self.host_call_depth -= 1;
        if decision.allowed {
            Ok(())
        } else {
            Err(WasmtimeError::msg(
                decision
                    .failure_reason
                    .unwrap_or_else(|| "capability denied".into()),
            ))
        }
    }

    fn last_denied_decision(&self) -> Option<ExtensionRuntimeDecision> {
        self.decisions
            .lock()
            .iter()
            .rev()
            .find(|decision| !decision.allowed)
            .cloned()
    }
}

fn read_guest_string(
    caller: &mut Caller<'_, WasmHostState>,
    ptr: i32,
    len: i32,
) -> WasmtimeResult<String> {
    if ptr < 0 || len < 0 {
        return Err(WasmtimeError::msg(
            "negative guest string pointer or length",
        ));
    }
    let memory = caller
        .get_export("memory")
        .and_then(|export| export.into_memory())
        .ok_or_else(|| WasmtimeError::msg("missing exported memory"))?;
    let ptr = ptr as usize;
    let len = len as usize;
    let end = ptr
        .checked_add(len)
        .ok_or_else(|| WasmtimeError::msg("guest string range overflow"))?;
    let data = memory.data(&*caller);
    let bytes = data
        .get(ptr..end)
        .ok_or_else(|| WasmtimeError::msg("guest string range out of bounds"))?;
    std::str::from_utf8(bytes)
        .map(|value| value.to_string())
        .map_err(|err| WasmtimeError::msg(err.to_string()))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::extensions::policy::{ExtensionPermission, ExtensionPolicyEngine, PolicyDecision};

    fn ctx(declared: &[ExtensionPermission]) -> ExtensionRuntimeCtx {
        let mut engine = ExtensionPolicyEngine::new();
        engine.set_override(
            "wasm-helper",
            ExtensionPermission::ToolRegistration,
            PolicyDecision::Allow,
        );
        ExtensionRuntimeCtx::new(
            "wasm-helper",
            declared.iter().copied().collect::<BTreeSet<_>>(),
            Arc::new(engine),
        )
    }

    fn limits() -> ExtensionRuntimeLimits {
        ExtensionRuntimeLimits {
            max_memory_bytes: 64 * 1024,
            fuel: 100_000,
            call_timeout: Duration::from_millis(50),
            max_host_call_depth: 4,
        }
    }

    #[tokio::test]
    async fn wasm_extension_initializes_and_registers_tool() {
        let bytes = wat::parse_str(
            r#"
            (module
              (import "vulcan_host" "log" (func $log (param i32 i32)))
              (import "vulcan_host" "register_tool" (func $register_tool (param i32 i32)))
              (memory (export "memory") 1)
              (data (i32.const 0) "ping")
              (func (export "_vulcan_init")
                i32.const 0
                i32.const 4
                call $log
                i32.const 0
                i32.const 4
                call $register_tool))
            "#,
        )
        .unwrap();
        let runtime = WasmExtensionRuntime::from_bytes(bytes, limits());
        let init = runtime
            .initialize(ctx(&[ExtensionPermission::ToolRegistration]))
            .await
            .unwrap();
        assert_eq!(init.registered_tools, vec!["ping"]);
        assert_eq!(init.decisions.len(), 2);
        assert!(init.decisions.iter().all(|decision| decision.allowed));
    }

    #[tokio::test]
    async fn denied_host_call_fails_extension_operation() {
        let bytes = wat::parse_str(
            r#"
            (module
              (import "vulcan_host" "register_tool" (func $register_tool (param i32 i32)))
              (memory (export "memory") 1)
              (data (i32.const 0) "ping")
              (func (export "_vulcan_init")
                i32.const 0
                i32.const 4
                call $register_tool))
            "#,
        )
        .unwrap();
        let runtime = WasmExtensionRuntime::from_bytes(bytes, limits());
        let err = runtime.initialize(ctx(&[])).await.unwrap_err();
        assert!(matches!(
            err,
            ExtensionRuntimeError::CapabilityDenied {
                capability: ExtensionRuntimeCapability::RegisterTool,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn missing_init_export_is_load_error() {
        let bytes = wat::parse_str(r#"(module (memory (export "memory") 1))"#).unwrap();
        let runtime = WasmExtensionRuntime::from_bytes(bytes, limits());
        let err = runtime
            .initialize(ctx(&[ExtensionPermission::ToolRegistration]))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ExtensionRuntimeError::MissingExport {
                export: "_vulcan_init",
                ..
            }
        ));
    }

    #[tokio::test]
    async fn fuel_limit_stops_runaway_module() {
        let bytes = wat::parse_str(
            r#"
            (module
              (func (export "_vulcan_init")
                (loop $again
                  br $again)))
            "#,
        )
        .unwrap();
        let runtime = WasmExtensionRuntime::from_bytes(
            bytes,
            ExtensionRuntimeLimits {
                fuel: 10,
                ..limits()
            },
        );
        let err = runtime.initialize(ctx(&[])).await.unwrap_err();
        assert!(matches!(
            err,
            ExtensionRuntimeError::LimitExceeded { limit, .. } if limit == "fuel"
        ));
    }

    #[tokio::test]
    async fn memory_limit_blocks_large_initial_memory() {
        let bytes = wat::parse_str(
            r#"
            (module
              (memory (export "memory") 2)
              (func (export "_vulcan_init")))
            "#,
        )
        .unwrap();
        let runtime = WasmExtensionRuntime::from_bytes(
            bytes,
            ExtensionRuntimeLimits {
                max_memory_bytes: 64 * 1024,
                ..limits()
            },
        );
        let err = runtime.initialize(ctx(&[])).await.unwrap_err();
        assert!(matches!(err, ExtensionRuntimeError::LoadFailed { .. }));
    }
}
