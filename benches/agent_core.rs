//! Agent-core micro benchmarks (YYC-83).
//!
//! Covers two hot paths:
//!   1. `PromptBuilder::build_system_prompt` (sync) — runs once per outgoing
//!      LLM call to render the system prompt from the live tool registry.
//!   2. `HookRegistry::apply_before_prompt` (async) — runs once per outgoing
//!      LLM call to apply transient hook injections to the message slice.
//!
//! Each bench varies `messages.len()` ∈ {10, 100, 1000} (the prompt-builder
//! bench ignores the parameter since it doesn't take messages, but we keep
//! the matrix consistent so divan groups them adjacent in the output table).
//!
//! After divan prints its operator-facing table, we mirror headline metrics
//! into the shared JSON artifact at `target/bench-results.json`. Divan's
//! Rust API doesn't expose stable per-bench aggregates, so we re-time a
//! representative invocation per arg with a hand-rolled loop. Sufficient for
//! regression detection at the 20 % threshold; future task can swap to
//! divan's JSON output once stable.

#[path = "common/mod.rs"]
mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use divan::{AllocProfiler, Bencher};
use tokio::runtime::Runtime;
use tokio_util::sync::CancellationToken;

use vulcan::hooks::HookRegistry;
use vulcan::prompt_builder::PromptBuilder;
use vulcan::provider::Message;
use vulcan::tools::ToolRegistry;

use common::results::{Measurement, append};

#[global_allocator]
static ALLOC: AllocProfiler = AllocProfiler::system();

const ARGS: [usize; 3] = [10, 100, 1000];

fn out_path() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "target".into()))
        .join("bench-results.json")
}

/// Build a synthetic message history of length `n + 1` (one leading System,
/// alternating User / Assistant turns thereafter). The Assistant turns are
/// intentionally bare — `tool_calls` and `reasoning_content` are `None` —
/// because we want to measure the steady-state cost of the registry walk,
/// not provider-specific serialization branches.
fn fixture_messages(n: usize) -> Vec<Message> {
    let mut out = Vec::with_capacity(n + 1);
    out.push(Message::System {
        content: "you are vulcan, a helpful agent".into(),
    });
    for i in 0..n {
        if i % 2 == 0 {
            out.push(Message::User {
                content: format!("user turn {i}"),
            });
        } else {
            out.push(Message::Assistant {
                content: Some(format!("assistant turn {i}")),
                tool_calls: None,
                reasoning_content: None,
            });
        }
    }
    out
}

#[divan::bench(args = ARGS)]
fn build_system_prompt(b: Bencher, _msgs: usize) {
    let registry = ToolRegistry::new();
    let pb = PromptBuilder;
    b.bench(|| pb.build_system_prompt(&registry));
}

#[divan::bench(args = ARGS)]
fn apply_before_prompt_no_hooks(b: Bencher, n: usize) {
    let rt = Runtime::new().unwrap();
    let registry = Arc::new(HookRegistry::new());
    let msgs = fixture_messages(n);
    b.bench(|| {
        rt.block_on(async {
            registry
                .apply_before_prompt(&msgs, CancellationToken::new())
                .await
        })
    });
}

fn main() {
    divan::main();

    // Mirror headline metrics into the shared JSON artifact.
    //
    // Divan owns the operator-facing table; this hand-rolled pass exists
    // purely so `scripts/bench-diff.py` (Task 8) has a stable input. Each
    // measurement is a coarse `Instant`-based mean over a small fixed
    // iteration count — fine for the 20 %-regression threshold the harness
    // is calibrated against.
    let mut groups: BTreeMap<String, Vec<Measurement>> = BTreeMap::new();

    // build_system_prompt — value of `n` is ignored by the function, but we
    // record one measurement per arg so the JSON shape mirrors divan's
    // table.
    let registry = ToolRegistry::new();
    let pb = PromptBuilder;
    for n in ARGS {
        const ITERS: u32 = 1_000;
        let start = std::time::Instant::now();
        for _ in 0..ITERS {
            let _ = pb.build_system_prompt(&registry);
        }
        let ns = (start.elapsed().as_nanos() as f64) / f64::from(ITERS);
        groups
            .entry("agent_core".into())
            .or_default()
            .push(Measurement {
                name: format!("build_system_prompt,n={n}"),
                ns_per_iter: Some(ns),
                ..Default::default()
            });
    }

    // apply_before_prompt_no_hooks — empty registry is the realistic baseline
    // for hot-path overhead; per-handler cost is left to integration benches.
    let rt = Runtime::new().unwrap();
    let hook_registry = Arc::new(HookRegistry::new());
    for n in ARGS {
        const ITERS: u32 = 200;
        let msgs = fixture_messages(n);
        let start = std::time::Instant::now();
        for _ in 0..ITERS {
            rt.block_on(async {
                let _ = hook_registry
                    .apply_before_prompt(&msgs, CancellationToken::new())
                    .await;
            });
        }
        let ns = (start.elapsed().as_nanos() as f64) / f64::from(ITERS);
        groups
            .entry("agent_core".into())
            .or_default()
            .push(Measurement {
                name: format!("apply_before_prompt_no_hooks,n={n}"),
                ns_per_iter: Some(ns),
                ..Default::default()
            });
    }

    if let Err(e) = append(&out_path(), groups) {
        eprintln!("bench-results write failed: {e}");
    }
}
