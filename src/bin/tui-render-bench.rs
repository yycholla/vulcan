use std::time::{Duration, Instant};

use vulcan::tui::{
    chat_render::{ChatRenderOptions, ChatRenderStore},
    state::{ChatMessage, ChatRole},
};

const MESSAGE_COUNT: usize = 50_000;
const WINDOW_HEIGHT: u16 = 40;
const WIDTH: u16 = 100;
const WARM_RUNS: usize = 3;
const MEASURED_RUNS: usize = 25;

fn main() {
    let mut messages = synthetic_transcript(MESSAGE_COUNT);
    let options = ChatRenderOptions {
        show_reasoning: true,
        dense: false,
        width: WIDTH,
        muted_style: ratatui::style::Style::default(),
    };
    let mut store = ChatRenderStore::default();

    let first = measure_once(&mut store, &messages, options, 0, WINDOW_HEIGHT);
    let cached_tail_scroll = first.total_lines.saturating_sub(usize::from(WINDOW_HEIGHT));
    let cached_tail = measure_many(
        &mut store,
        &messages,
        options,
        cached_tail_scroll,
        WINDOW_HEIGHT,
    );

    let before_mutation_renders = store.render_count();
    if let Some(last) = messages.last_mut() {
        last.append_text("\nFinal streamed tail chunk with **markdown**.");
    }
    let mutated_tail = measure_once(
        &mut store,
        &messages,
        options,
        cached_tail_scroll,
        WINDOW_HEIGHT,
    );
    let mutation_rerenders = store.render_count().saturating_sub(before_mutation_renders);

    println!("tui render benchmark");
    println!("messages: {MESSAGE_COUNT}");
    println!("window: {WINDOW_HEIGHT} lines @ {WIDTH} columns");
    println!(
        "first visible_lines: {:?} total_lines={} rendered_blocks={} materialized_lines={}",
        first.elapsed, first.total_lines, first.rendered_blocks, first.materialized_lines
    );
    println!(
        "cached tail visible_lines: avg={:?} min={:?} max={:?} materialized_lines={}",
        cached_tail.avg, cached_tail.min, cached_tail.max, cached_tail.materialized_lines
    );
    println!(
        "mutated tail visible_lines: {:?} rerendered_blocks={} materialized_lines={}",
        mutated_tail.elapsed, mutation_rerenders, mutated_tail.materialized_lines
    );
}

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

fn measure_once(
    store: &mut ChatRenderStore,
    messages: &[ChatMessage],
    options: ChatRenderOptions,
    scroll: usize,
    height: u16,
) -> SingleRun {
    let before = store.render_count();
    let start = Instant::now();
    let window = store.visible_lines_at(messages, options, scroll, usize::from(height));
    let elapsed = start.elapsed();

    SingleRun {
        elapsed,
        total_lines: window.total_lines,
        rendered_blocks: store.render_count().saturating_sub(before),
        materialized_lines: store.materialized_line_count(),
    }
}

fn measure_many(
    store: &mut ChatRenderStore,
    messages: &[ChatMessage],
    options: ChatRenderOptions,
    scroll: usize,
    height: u16,
) -> RunSummary {
    for _ in 0..WARM_RUNS {
        let _ = measure_once(store, messages, options, scroll, height);
    }

    let mut total = Duration::ZERO;
    let mut min = Duration::MAX;
    let mut max = Duration::ZERO;
    let mut materialized_lines = 0;

    for _ in 0..MEASURED_RUNS {
        let run = measure_once(store, messages, options, scroll, height);
        total += run.elapsed;
        min = min.min(run.elapsed);
        max = max.max(run.elapsed);
        materialized_lines = run.materialized_lines;
    }

    RunSummary {
        avg: total / MEASURED_RUNS as u32,
        min,
        max,
        materialized_lines,
    }
}

struct SingleRun {
    elapsed: Duration,
    total_lines: usize,
    rendered_blocks: usize,
    materialized_lines: usize,
}

struct RunSummary {
    avg: Duration,
    min: Duration,
    max: Duration,
    materialized_lines: usize,
}
