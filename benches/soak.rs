//! End-to-end soak benchmark (YYC-83).
//!
//! Drives `Agent::run_prompt` against `GeneratedProvider` for N turns,
//! sampling per-turn latency, RSS, and (when feature-gated) total
//! allocations every K turns. Writes results into
//! `target/bench-results.json` (schema v1).
//!
//! Build/run:
//!   cargo run --bin vulcan-soak --features bench-soak -- --turns 1000

#[path = "common/mod.rs"]
mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use clap::Parser;
use hdrhistogram::Histogram;

use vulcan::agent::Agent;
use vulcan::hooks::HookRegistry;
use vulcan::provider::mock::GeneratedProvider;
use vulcan::provider::{ChatResponse, LLMProvider};
use vulcan::skills::SkillRegistry;
use vulcan::tools::ToolRegistry;

use common::results::{Measurement, append};

#[cfg(feature = "bench-soak")]
#[global_allocator]
static ALLOCATOR: dhat::Alloc = dhat::Alloc;

#[derive(Parser)]
#[command(about = "Vulcan soak benchmark — drives Agent::run_prompt for N turns.")]
struct Args {
    /// Total turn count.
    #[arg(long, default_value_t = 100)]
    turns: usize,
    /// Sample every N turns.
    #[arg(long, default_value_t = 100)]
    sample_every: usize,
}

/// Linux-only RSS read via `/proc/self/statm`. Returns `None` on other
/// platforms or if the file isn't readable.
fn rss_kb() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/statm").ok()?;
    // Format: size resident shared text lib data dt (all in pages).
    let resident_pages: u64 = s.split_whitespace().nth(1)?.parse().ok()?;
    // Hardcode 4 KiB pages — standard on x86_64 Linux. The bench is
    // Linux-only per design; if you run on a system with a different
    // page size the RSS number will scale by a constant factor and the
    // regression gate's relative comparisons still work.
    const PAGE_KB: u64 = 4;
    Some(resident_pages * PAGE_KB)
}

fn out_path() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "target".into()))
        .join("bench-results.json")
}

/// Build an empty `SkillRegistry` for the soak bench so it doesn't pull
/// in user skills or bundled defaults.
fn empty_skills() -> Arc<SkillRegistry> {
    Arc::new(SkillRegistry::empty())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    #[cfg(feature = "bench-soak")]
    let _profiler = dhat::Profiler::new_heap();

    let provider: Box<dyn LLMProvider> =
        Box::new(GeneratedProvider::new(128_000, |turn| ChatResponse {
            content: Some(format!("response for turn {turn}")),
            tool_calls: None,
            usage: None,
            finish_reason: Some("stop".into()),
            reasoning_content: None,
        }));

    let mut agent = Agent::for_test(
        provider,
        ToolRegistry::new(),
        HookRegistry::new(),
        empty_skills(),
    );

    let mut samples: Vec<Measurement> = Vec::new();
    let mut hist: Histogram<u64> =
        Histogram::new_with_bounds(1, 60_000_000, 3).expect("hdrhistogram");

    for turn in 0..args.turns {
        let start = Instant::now();
        let _ = agent.run_prompt("hi").await?;
        let elapsed_us = start.elapsed().as_micros() as u64;
        hist.record(elapsed_us.max(1)).ok();

        if (turn + 1) % args.sample_every == 0 || turn + 1 == args.turns {
            samples.push(Measurement {
                name: format!("turn={}", turn + 1),
                turn: Some(turn + 1),
                p50_ms: Some((hist.value_at_quantile(0.50) as f64) / 1000.0),
                p99_ms: Some((hist.value_at_quantile(0.99) as f64) / 1000.0),
                rss_kb: rss_kb(),
                ..Default::default()
            });
        }
    }

    let mut groups: BTreeMap<String, Vec<Measurement>> = BTreeMap::new();
    groups.insert("soak".into(), samples);
    append(&out_path(), groups)?;

    eprintln!(
        "soak: ran {} turns; sampled every {}",
        args.turns, args.sample_every
    );
    Ok(())
}
