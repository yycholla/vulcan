//! TUI chat-render benchmarks (YYC-83).
//!
//! Measures `ChatRenderStore::visible_lines_at` over message counts
//! {100, 1000, 10000} so virtualization regressions surface here before
//! they bite in production. The hand-rolled bench at
//! `src/bin/tui-render-bench.rs` covers cache-invalidation realism on a
//! single 50_000-message corpus; this divan bench is the regression gate
//! that watches scaling vs. message count. Both are intentional — keep both.
//!
//! After divan prints its operator-facing table, headline metrics are
//! mirrored into `target/bench-results.json` via `benches/common/results.rs`
//! so `scripts/bench-diff.py` (Task 8) has a stable input.

#[path = "common/mod.rs"]
mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;

use divan::{AllocProfiler, Bencher};

use vulcan::tui::{
    chat_render::{ChatRenderOptions, ChatRenderStore},
    state::{ChatMessage, ChatRole},
    theme::Theme,
};

use common::results::{Measurement, append};

#[global_allocator]
static ALLOC: AllocProfiler = AllocProfiler::system();

const ARGS: [usize; 3] = [100, 1000, 10000];
const WINDOW_HEIGHT: u16 = 40;
const WIDTH: u16 = 100;

fn out_path() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "target".into()))
        .join("bench-results.json")
}

/// Inline copy of `src/bin/tui-render-bench.rs::synthetic_transcript`.
///
/// Bin targets aren't importable from bench targets, so we duplicate the
/// fixture here. ~30 lines, lives only in benches — acceptable trade-off
/// vs. expanding the public surface to share it.
fn synthetic_transcript(count: usize) -> Vec<ChatMessage> {
    (0..count)
        .map(|i| {
            if i % 2 == 0 {
                ChatMessage::new(
                    ChatRole::User,
                    format!("User message {i}: inspect `src/tui/views.rs` and summarize."),
                )
            } else {
                let mut message = ChatMessage::new(ChatRole::Agent, "");
                message.append_reasoning("checking renderer cache\n");
                message.push_tool_start_with("read_file", Some("src/tui/views.rs".to_string()));
                message.finish_tool_with(
                    "read_file",
                    true,
                    Some("fn build_chat_window(...) { ... }".to_string()),
                    Some("240 lines".to_string()),
                    0,
                    Some(7),
                );
                message.append_text(&format!(
                    "Agent response {i}: retained blocks avoid rebuilding offscreen markdown."
                ));
                message
            }
        })
        .collect()
}

#[divan::bench(args = ARGS)]
fn visible_lines_first_render(b: Bencher, n: usize) {
    let messages = synthetic_transcript(n);
    let options = ChatRenderOptions {
        show_reasoning: true,
        dense: false,
        width: WIDTH,
        muted_style: ratatui::style::Style::default(),
    };
    // Cold store per iter: each iter measures the no-cache path. `with_inputs`
    // builds the store outside the timed region.
    b.with_inputs(ChatRenderStore::default)
        .bench_refs(|store| {
            store.visible_lines_at(&messages, options, &Theme::system(), 0, usize::from(WINDOW_HEIGHT))
        });
}

#[divan::bench(args = ARGS)]
fn visible_lines_cached_tail(b: Bencher, n: usize) {
    let messages = synthetic_transcript(n);
    let options = ChatRenderOptions {
        show_reasoning: true,
        dense: false,
        width: WIDTH,
        muted_style: ratatui::style::Style::default(),
    };
    let mut store = ChatRenderStore::default();
    // Prime cache with one full render so subsequent iters hit warm paths.
    let first = store.visible_lines_at(&messages, options, &Theme::system(), 0, usize::from(WINDOW_HEIGHT));
    let scroll = first.total_lines.saturating_sub(usize::from(WINDOW_HEIGHT));
    // `bench_local` accepts `FnMut`, which lets us keep the warm cache alive
    // across iters without paying a `Sync` tax (and without per-iter cloning).
    b.bench_local(move || {
        store.visible_lines_at(&messages, options, &Theme::system(), scroll, usize::from(WINDOW_HEIGHT))
    });
}

fn main() {
    divan::main();

    // Mirror headline metrics into the shared JSON artifact.
    //
    // Divan owns the operator-facing table; this hand-rolled pass exists
    // purely so `scripts/bench-diff.py` (Task 8) has a stable input. Coarse
    // `Instant`-based mean over a small fixed iter count — fine for the
    // 20 %-regression threshold the harness is calibrated against.
    let mut groups: BTreeMap<String, Vec<Measurement>> = BTreeMap::new();

    for n in ARGS {
        const ITERS: u32 = 50;
        let messages = synthetic_transcript(n);
        let options = ChatRenderOptions {
            show_reasoning: true,
            dense: false,
            width: WIDTH,
            muted_style: ratatui::style::Style::default(),
        };

        // first render — cold store per iter.
        let start = std::time::Instant::now();
        for _ in 0..ITERS {
            let mut store = ChatRenderStore::default();
            let _ = store.visible_lines_at(&messages, options, &Theme::system(), 0, usize::from(WINDOW_HEIGHT));
        }
        let ns = (start.elapsed().as_nanos() as f64) / f64::from(ITERS);
        groups
            .entry("tui_render".into())
            .or_default()
            .push(Measurement {
                name: format!("visible_lines_first_render,n={n}"),
                ns_per_iter: Some(ns),
                ..Default::default()
            });

        // cached tail — store kept warm across iters.
        let mut store = ChatRenderStore::default();
        let first = store.visible_lines_at(&messages, options, &Theme::system(), 0, usize::from(WINDOW_HEIGHT));
        let scroll = first.total_lines.saturating_sub(usize::from(WINDOW_HEIGHT));
        let start = std::time::Instant::now();
        for _ in 0..ITERS {
            let _ = store.visible_lines_at(&messages, options, &Theme::system(), scroll, usize::from(WINDOW_HEIGHT));
        }
        let ns = (start.elapsed().as_nanos() as f64) / f64::from(ITERS);
        groups
            .entry("tui_render".into())
            .or_default()
            .push(Measurement {
                name: format!("visible_lines_cached_tail,n={n}"),
                ns_per_iter: Some(ns),
                ..Default::default()
            });
    }

    if let Err(e) = append(&out_path(), groups) {
        eprintln!("bench-results write failed: {e}");
    }
}
