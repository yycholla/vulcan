# TUI Virtualized Chat Renderer Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make the TUI smooth during streaming and scalable for large transcripts by replacing full transcript line rebuilding with a retained, virtualized chat render store.

**Architecture:** Keep Ratatui as the terminal backend and layout engine. Add a retained `ChatRenderStore` that caches rendered message blocks by message/version/render options, updates the active streaming message incrementally, and exposes only the visible line window to Ratatui. This avoids rebuilding and cloning the full transcript on every frame while preserving the existing views.

**Tech Stack:** Rust 2024, Ratatui 0.30, Crossterm 0.29, existing `src/tui/{state,views,markdown,widgets}.rs`.

---

## Constraints And Current Context

- `src/tui/views.rs` currently builds the primary chat as `Vec<Line<'static>>` in `build_chat_lines_w`.
- `src/tui/state.rs` currently stores messages in `AppState.messages: Vec<ChatMessage>`.
- A previous incremental cache exists: `AppState.chat_lines_dirty` and `AppState.chat_lines_cache`.
- Ratatui already diffs terminal buffers, so this plan does not replace `Terminal` or `CrosstermBackend`.
- Ratatui requires full frame rendering each draw; the optimization is to make each widget render cheap, not partial-frame unsafe.
- The first implementation should optimize `SingleStack`, `SplitSessions`, and `TradingFloor` chat panes. Other panes can stay as-is unless tests show they regress.
- Avoid changing conversation persistence, provider streaming, hook semantics, or `Agent` behavior.

---

### Task 1: Add Stable Message Render Versions

**Files:**
- Modify: `src/tui/state.rs`

**Step 1: Write the failing tests**

Add tests under the existing `#[cfg(test)] mod tests` in `src/tui/state.rs`:

```rust
#[test]
fn chat_message_render_version_bumps_on_mutation() {
    let mut m = ChatMessage::new(ChatRole::Agent, "");
    let initial = m.render_version();

    m.append_text("hello");
    assert!(m.render_version() > initial);
    let after_text = m.render_version();

    m.append_reasoning("thinking");
    assert!(m.render_version() > after_text);
    let after_reasoning = m.render_version();

    m.push_tool_start("bash");
    assert!(m.render_version() > after_reasoning);
    let after_tool_start = m.render_version();

    m.finish_tool("bash", true);
    assert!(m.render_version() > after_tool_start);
}

#[test]
fn chat_message_new_starts_at_zero_render_version() {
    let m = ChatMessage::new(ChatRole::User, "hello");
    assert_eq!(m.render_version(), 0);
}
```

**Step 2: Run tests to verify they fail**

Run:

```bash
cargo test chat_message_render_version --all-targets
```

Expected: fails because `render_version` does not exist.

**Step 3: Implement minimal version tracking**

In `ChatMessage`, add a private field:

```rust
render_version: u64,
```

Update constructors and struct literals in TUI code so `Default::default()` still works. Add methods:

```rust
impl ChatMessage {
    pub fn render_version(&self) -> u64 {
        self.render_version
    }

    fn bump_render_version(&mut self) {
        self.render_version = self.render_version.wrapping_add(1);
    }
}
```

Call `bump_render_version()` at the end of:

- `append_text`
- `append_reasoning`
- `push_tool_start_with`
- successful mutation in `finish_tool_with`

For direct assignment sites in `src/tui/mod.rs` that mutate `last.content`, either:

- Prefer adding methods on `ChatMessage` such as `set_content`, or
- Set content and call a public `mark_render_dirty()` method.

Preferred minimal API:

```rust
pub fn set_content(&mut self, content: impl Into<String>) {
    self.content = content.into();
    self.bump_render_version();
}
```

Use `last.set_content(format!("⚠ Error: {e}"));` instead of assigning `last.content = ...`.

**Step 4: Run tests to verify they pass**

Run:

```bash
cargo test chat_message_render_version --all-targets
```

Expected: both tests pass.

**Step 5: Commit**

Do not commit unrelated dirty files. Commit only the files touched by this task:

```bash
git add src/tui/state.rs src/tui/mod.rs
git commit -m "perf(tui): track chat message render versions"
```

---

### Task 2: Introduce Chat Render Store Types

**Files:**
- Create: `src/tui/chat_render.rs`
- Modify: `src/tui/mod.rs`
- Modify: `src/lib.rs` if module visibility requires it, otherwise no change

**Step 1: Write the failing tests**

Create `src/tui/chat_render.rs` with test scaffolding first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::state::{ChatMessage, ChatRole};

    #[test]
    fn render_store_returns_only_visible_window() {
        let mut store = ChatRenderStore::default();
        let messages = (0..100)
            .map(|i| ChatMessage::new(ChatRole::User, format!("message {i}")))
            .collect::<Vec<_>>();

        let options = ChatRenderOptions {
            show_reasoning: true,
            dense: false,
            width: 80,
        };

        let window = store.visible_lines(&messages, options, 10, 5, None, 0);
        assert_eq!(window.lines.len(), 5);
        assert!(window.total_lines > 5);
    }

    #[test]
    fn render_store_cache_key_includes_render_options() {
        let mut store = ChatRenderStore::default();
        let messages = vec![ChatMessage::new(ChatRole::User, "hello")];

        let wide = ChatRenderOptions {
            show_reasoning: true,
            dense: false,
            width: 80,
        };
        let narrow = ChatRenderOptions { width: 20, ..wide };

        let _ = store.visible_lines(&messages, wide, 0, 10, None, 0);
        let renders_after_wide = store.render_count_for_tests();
        let _ = store.visible_lines(&messages, wide, 0, 10, None, 0);
        assert_eq!(store.render_count_for_tests(), renders_after_wide);
        let _ = store.visible_lines(&messages, narrow, 0, 10, None, 0);
        assert!(store.render_count_for_tests() > renders_after_wide);
    }
}
```

**Step 2: Run tests to verify they fail**

Run:

```bash
cargo test render_store --all-targets
```

Expected: fails because `chat_render` module/types do not exist.

**Step 3: Add initial types and module wiring**

In `src/tui/mod.rs`, add:

```rust
pub mod chat_render;
```

In `src/tui/chat_render.rs`, define:

```rust
use std::collections::HashMap;

use ratatui::text::Line;

use super::state::{ChatMessage, ChatRole, MessageSegment};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ChatRenderOptions {
    pub show_reasoning: bool,
    pub dense: bool,
    pub width: u16,
}

#[derive(Clone, Debug, Default)]
pub struct VisibleChatLines {
    pub lines: Vec<Line<'static>>,
    pub total_lines: usize,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct MessageRenderKey {
    index: usize,
    version: u64,
    role: ChatRole,
    options: ChatRenderOptions,
}

#[derive(Clone, Debug)]
struct RenderedMessageBlock {
    lines: Vec<Line<'static>>,
}

#[derive(Default)]
pub struct ChatRenderStore {
    blocks: HashMap<MessageRenderKey, RenderedMessageBlock>,
    render_count_for_tests: usize,
}
```

If `ChatRole` does not derive `Hash`, update its derive list in `src/tui/state.rs` to include `PartialEq`, `Eq`, and `Hash`.

Add the minimal public API:

```rust
impl ChatRenderStore {
    pub fn visible_lines(
        &mut self,
        messages: &[ChatMessage],
        options: ChatRenderOptions,
        scroll: u16,
        height: u16,
        pending_pause: Option<&crate::pause::AgentPause>,
        queue_len: usize,
    ) -> VisibleChatLines {
        let all = self.render_all_for_now(messages, options, pending_pause, queue_len);
        let total_lines = all.len();
        let start = usize::from(scroll).min(total_lines);
        let end = start.saturating_add(usize::from(height)).min(total_lines);
        VisibleChatLines {
            lines: all[start..end].to_vec(),
            total_lines,
        }
    }

    #[cfg(test)]
    pub fn render_count_for_tests(&self) -> usize {
        self.render_count_for_tests
    }
}
```

For this task, `render_all_for_now` may still concatenate all blocks. Later tasks remove the full-concat dependency.

**Step 4: Run tests to verify they pass**

Run:

```bash
cargo test render_store --all-targets
```

Expected: tests pass.

**Step 5: Commit**

```bash
git add src/tui/chat_render.rs src/tui/mod.rs src/tui/state.rs
git commit -m "perf(tui): add retained chat render store"
```

---

### Task 3: Move Message Block Rendering Out Of `views.rs`

**Files:**
- Modify: `src/tui/chat_render.rs`
- Modify: `src/tui/views.rs`

**Step 1: Write the failing parity test**

In `src/tui/chat_render.rs`, add tests that encode existing behavior at the block level:

```rust
#[test]
fn render_user_message_block_includes_header_and_body() {
    let mut store = ChatRenderStore::default();
    let msg = ChatMessage::new(ChatRole::User, "hello **world**");
    let options = ChatRenderOptions {
        show_reasoning: true,
        dense: false,
        width: 80,
    };

    let block = store.render_message_block_for_tests(0, &msg, options);
    let text = block
        .lines
        .iter()
        .flat_map(|line| line.spans.iter().map(|s| s.content.as_ref()))
        .collect::<Vec<_>>()
        .join("");

    assert!(text.contains("YOU"));
    assert!(text.contains("hello "));
    assert!(text.contains("world"));
}

#[test]
fn render_agent_segment_block_preserves_tool_and_text_order() {
    let mut store = ChatRenderStore::default();
    let mut msg = ChatMessage::new(ChatRole::Agent, "");
    msg.append_reasoning("thinking");
    msg.push_tool_start_with("read_file", Some("src/main.rs".into()));
    msg.finish_tool("read_file", true);
    msg.append_text("done");

    let options = ChatRenderOptions {
        show_reasoning: true,
        dense: false,
        width: 80,
    };

    let block = store.render_message_block_for_tests(0, &msg, options);
    let text = block
        .lines
        .iter()
        .flat_map(|line| line.spans.iter().map(|s| s.content.as_ref()))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(text.find("THINKING").unwrap() < text.find("read_file").unwrap());
    assert!(text.find("read_file").unwrap() < text.find("done").unwrap());
}
```

**Step 2: Run tests to verify they fail**

Run:

```bash
cargo test render_message_block --all-targets
```

Expected: fails until rendering logic is moved/exposed.

**Step 3: Move rendering logic**

Move the message rendering logic from `build_chat_lines_w` in `src/tui/views.rs` into `ChatRenderStore::render_message_block`.

Implementation notes:

- Reuse `super::markdown::render_markdown`.
- Reuse `super::widgets::{message_header, reasoning_lines, tool_card, pill}`.
- Keep session header, pending pause pills, and queue preview as separate store methods for now.
- Cache completed blocks by `MessageRenderKey`.
- For `MessageRenderKey.index`, use the message slice index for now. This is good enough because messages are append-only except `/clear`; cache is cleared on reset in later tasks.

Add test-only helper:

```rust
#[cfg(test)]
pub fn render_message_block_for_tests(
    &mut self,
    index: usize,
    message: &ChatMessage,
    options: ChatRenderOptions,
) -> RenderedMessageBlock {
    self.render_message_block(index, message, options).clone()
}
```

**Step 4: Run tests to verify they pass**

Run:

```bash
cargo test render_message_block --all-targets
```

Expected: tests pass.

**Step 5: Commit**

```bash
git add src/tui/chat_render.rs src/tui/views.rs
git commit -m "perf(tui): retain rendered chat message blocks"
```

---

### Task 4: Add True Visible-Window Materialization

**Files:**
- Modify: `src/tui/chat_render.rs`

**Step 1: Write the failing test**

Add:

```rust
#[test]
fn visible_lines_does_not_clone_offscreen_message_lines() {
    let mut store = ChatRenderStore::default();
    let messages = (0..100)
        .map(|i| ChatMessage::new(ChatRole::User, format!("message {i}")))
        .collect::<Vec<_>>();
    let options = ChatRenderOptions {
        show_reasoning: true,
        dense: false,
        width: 80,
    };

    let window = store.visible_lines(&messages, options, 90, 3, None, 0);
    assert_eq!(window.lines.len(), 3);
    assert!(window.total_lines > window.lines.len());
    assert!(store.materialized_line_count_for_tests() <= 3);
}
```

**Step 2: Run tests to verify they fail**

Run:

```bash
cargo test visible_lines_does_not_clone_offscreen_message_lines --all-targets
```

Expected: fails because the initial implementation concatenates all lines.

**Step 3: Implement windowed materialization**

Change `visible_lines` so it:

1. Iterates rendered message blocks in order.
2. Tracks cumulative line count.
3. Skips blocks entirely before `scroll`.
4. Copies only lines that intersect `[scroll, scroll + height)`.
5. Stops once the visible window is full.
6. Still computes `total_lines`.

Add a test-only counter:

```rust
#[cfg(test)]
materialized_line_count_for_tests: usize,
```

Reset it at the start of `visible_lines`, increment only when a line is pushed into the returned `VisibleChatLines`.

**Step 4: Run tests to verify they pass**

Run:

```bash
cargo test visible_lines --all-targets
```

Expected: visible-window tests pass.

**Step 5: Commit**

```bash
git add src/tui/chat_render.rs
git commit -m "perf(tui): virtualize chat line materialization"
```

---

### Task 5: Wire `ChatRenderStore` Into `AppState`

**Files:**
- Modify: `src/tui/state.rs`
- Modify: `src/tui/views.rs`

**Step 1: Write the failing integration-style test**

In `src/tui/views.rs`, replace the current cache-key test with one that uses the store through `AppState`:

```rust
#[test]
fn build_chat_window_uses_visible_height() {
    let mut app = AppState::new("test-model".into(), 128_000);
    for i in 0..100 {
        app.messages
            .push(ChatMessage::new(ChatRole::User, format!("message {i}")));
    }

    let lines = build_chat_window_for_tests(&mut app, true, false, 80, 5);
    assert_eq!(lines.lines.len(), 5);
    assert!(lines.total_lines > 5);
}
```

**Step 2: Run test to verify it fails**

Run:

```bash
cargo test build_chat_window_uses_visible_height --all-targets
```

Expected: fails because `AppState` has no render store and helper does not exist.

**Step 3: Add store to `AppState`**

In `src/tui/state.rs`, add:

```rust
pub chat_render_store: RefCell<crate::tui::chat_render::ChatRenderStore>,
```

Initialize in `AppState::new`:

```rust
chat_render_store: RefCell::new(crate::tui::chat_render::ChatRenderStore::default()),
```

Remove or deprecate these fields after wiring succeeds:

- `chat_lines_dirty`
- `chat_lines_cache`

Do not remove all call sites in the same sub-step unless compilation requires it. Prefer keeping them temporarily as no-ops until Task 7 cleanup.

**Step 4: Add view helper**

In `src/tui/views.rs`, replace `build_chat_lines_w` usage with:

```rust
fn build_chat_window(
    app: &AppState,
    show_reasoning: bool,
    dense: bool,
    width: u16,
    height: u16,
) -> crate::tui::chat_render::VisibleChatLines {
    let options = crate::tui::chat_render::ChatRenderOptions {
        show_reasoning,
        dense,
        width,
    };
    app.chat_render_store.borrow_mut().visible_lines(
        &app.messages,
        options,
        app.scroll,
        height,
        app.pending_pause.as_ref(),
        app.queue.len(),
    )
}
```

Add test-only wrapper if needed:

```rust
#[cfg(test)]
fn build_chat_window_for_tests(
    app: &mut AppState,
    show_reasoning: bool,
    dense: bool,
    width: u16,
    height: u16,
) -> crate::tui::chat_render::VisibleChatLines {
    build_chat_window(app, show_reasoning, dense, width, height)
}
```

**Step 5: Run test to verify it passes**

Run:

```bash
cargo test build_chat_window_uses_visible_height --all-targets
```

Expected: passes.

**Step 6: Commit**

```bash
git add src/tui/state.rs src/tui/views.rs
git commit -m "perf(tui): wire virtualized chat store into app state"
```

---

### Task 6: Replace Chat Pane Rendering In Views

**Files:**
- Modify: `src/tui/views.rs`

**Step 1: Update `single_stack`**

Replace:

```rust
let lines = build_chat_lines_w(app, app.show_reasoning, false, chat_w);
publish_chat_max_scroll(app, lines.len(), inner.height);
Paragraph::new(lines)
```

With:

```rust
let window = build_chat_window(app, app.show_reasoning, false, chat_w, inner.height);
publish_chat_max_scroll(app, window.total_lines, inner.height);
Paragraph::new(window.lines)
```

Remove `.scroll((app.scroll, 0))` from the chat paragraph because the store already returns the scrolled window.

**Step 2: Update `split_sessions`**

Use actual body width instead of the old default width `80`:

```rust
let chat_width = body_area.width.saturating_sub(2);
let chat_height = body_area.height;
let window = build_chat_window(app, app.show_reasoning, false, chat_width, chat_height);
publish_chat_max_scroll(app, window.total_lines, chat_height.saturating_sub(1));
Paragraph::new(window.lines)
```

Remove `.scroll((app.scroll, 0))`.

**Step 3: Update `trading_floor`**

Use:

```rust
let chat_width = primary_inner.width.saturating_sub(2);
let chat_height = primary_inner.height;
let window = build_chat_window(app, app.show_reasoning, true, chat_width, chat_height);
publish_chat_max_scroll(app, window.total_lines, chat_height);
Paragraph::new(window.lines)
```

Remove `.scroll((app.scroll, 0))`.

**Step 4: Run focused tests**

Run:

```bash
cargo test tui::views --all-targets
cargo check --all-targets
```

Expected: tests and check pass.

**Step 5: Commit**

```bash
git add src/tui/views.rs
git commit -m "perf(tui): render chat panes from visible windows"
```

---

### Task 7: Replace Dirty Flags With Store Invalidation

**Files:**
- Modify: `src/tui/mod.rs`
- Modify: `src/tui/state.rs`
- Modify: `src/tui/views.rs`

**Step 1: Remove old fields**

Remove from `AppState`:

- `chat_lines_dirty`
- `chat_lines_cache`
- `ChatLinesCache`
- `ChatLinesCacheKey`

**Step 2: Replace call sites**

Remove all:

```rust
app.chat_lines_dirty.set(true);
```

For `/clear` and any future non-append mutation that can invalidate message indices, add:

```rust
app.chat_render_store.borrow_mut().clear();
```

Implement:

```rust
impl ChatRenderStore {
    pub fn clear(&mut self) {
        self.blocks.clear();
    }
}
```

For message mutation, rely on `ChatMessage::render_version`.

**Step 3: Run search to ensure old cache is gone**

Run:

```bash
rg "chat_lines_dirty|chat_lines_cache|ChatLinesCache" src/tui
```

Expected: no matches.

**Step 4: Run checks**

Run:

```bash
cargo check --all-targets
cargo test render_store --all-targets
cargo test tui::views --all-targets
```

Expected: all pass.

**Step 5: Commit**

```bash
git add src/tui/mod.rs src/tui/state.rs src/tui/views.rs src/tui/chat_render.rs
git commit -m "perf(tui): remove full transcript line cache"
```

---

### Task 8: Add Streaming Smoothness Throttle

**Files:**
- Modify: `src/tui/mod.rs`

**Step 1: Write the failing unit test for batching policy**

Extract batching policy into a small pure function in `src/tui/mod.rs` or a new `src/tui/render_tick.rs` module:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RenderWake {
    Now,
    Wait(std::time::Duration),
}

fn render_wake_for_stream_batch(
    last_draw: std::time::Instant,
    now: std::time::Instant,
    is_terminal_event: bool,
) -> RenderWake {
    todo!()
}
```

Add tests:

```rust
#[test]
fn stream_batching_caps_stream_redraws_to_frame_budget() {
    let start = std::time::Instant::now();
    assert_eq!(
        render_wake_for_stream_batch(start, start + std::time::Duration::from_millis(1), false),
        RenderWake::Wait(std::time::Duration::from_millis(15))
    );
}

#[test]
fn input_events_render_immediately() {
    let start = std::time::Instant::now();
    assert_eq!(
        render_wake_for_stream_batch(start, start + std::time::Duration::from_millis(1), true),
        RenderWake::Now
    );
}
```

**Step 2: Run tests to verify they fail**

Run:

```bash
cargo test stream_batching --all-targets
```

Expected: fails because helper does not exist.

**Step 3: Implement frame budget**

Set a stream frame budget around 60 FPS:

```rust
const STREAM_FRAME_BUDGET: std::time::Duration = std::time::Duration::from_millis(16);
```

Implementation:

```rust
fn render_wake_for_stream_batch(
    last_draw: std::time::Instant,
    now: std::time::Instant,
    is_terminal_event: bool,
) -> RenderWake {
    if is_terminal_event {
        return RenderWake::Now;
    }
    let elapsed = now.saturating_duration_since(last_draw);
    if elapsed >= STREAM_FRAME_BUDGET {
        RenderWake::Now
    } else {
        RenderWake::Wait(STREAM_FRAME_BUDGET - elapsed)
    }
}
```

Integrate into the main loop after stream batches:

- Key events and pause events render immediately.
- Stream events drain the channel but may sleep until the frame budget expires before drawing.
- Do not delay `Done`, `Error`, or pause events; they should force immediate redraw.

**Step 4: Run focused tests and manual check**

Run:

```bash
cargo test stream_batching --all-targets
cargo check --all-targets
```

Manual check:

```bash
cargo run -- prompt "Write a long answer with 100 numbered lines."
```

For TUI manual check, launch:

```bash
cargo run
```

Then submit a prompt that streams a long response. Expected: typing remains responsive and the stream updates smoothly without per-token redraw churn.

**Step 5: Commit**

```bash
git add src/tui/mod.rs
git commit -m "perf(tui): cap stream redraw cadence"
```

---

### Task 9: Add Synthetic Performance Tests

**Files:**
- Modify: `src/tui/chat_render.rs`

**Step 1: Add large transcript test**

Add:

```rust
#[test]
fn large_transcript_visible_window_stays_small() {
    let mut store = ChatRenderStore::default();
    let messages = (0..5_000)
        .map(|i| ChatMessage::new(ChatRole::User, format!("message {i}")))
        .collect::<Vec<_>>();
    let options = ChatRenderOptions {
        show_reasoning: true,
        dense: false,
        width: 100,
    };

    let window = store.visible_lines(&messages, options, 4_900, 20, None, 0);
    assert_eq!(window.lines.len(), 20);
    assert!(window.total_lines > 5_000);
    assert!(store.materialized_line_count_for_tests() <= 20);
}
```

**Step 2: Add active message invalidation test**

Add:

```rust
#[test]
fn mutating_one_message_only_rerenders_that_block() {
    let mut store = ChatRenderStore::default();
    let mut messages = vec![
        ChatMessage::new(ChatRole::User, "one"),
        ChatMessage::new(ChatRole::Agent, ""),
    ];
    let options = ChatRenderOptions {
        show_reasoning: true,
        dense: false,
        width: 80,
    };

    let _ = store.visible_lines(&messages, options, 0, 20, None, 0);
    let first_count = store.render_count_for_tests();

    messages[1].append_text("hello");
    let _ = store.visible_lines(&messages, options, 0, 20, None, 0);
    assert_eq!(store.render_count_for_tests(), first_count + 1);
}
```

**Step 3: Run tests**

Run:

```bash
cargo test large_transcript_visible_window_stays_small mutating_one_message_only_rerenders_that_block --all-targets
```

Expected: both pass.

**Step 4: Commit**

```bash
git add src/tui/chat_render.rs
git commit -m "test(tui): cover virtualized chat performance behavior"
```

---

### Task 10: Final Verification

**Files:**
- No code changes unless verification exposes failures.

**Step 1: Run formatting**

Run:

```bash
cargo fmt --check
```

Expected: pass. If unrelated pre-existing formatting diffs remain, document them and do not rewrite unrelated files.

**Step 2: Run full checks**

Run:

```bash
cargo check --all-targets
cargo test --all-targets
```

Expected: pass. If sandbox blocks tests that write outside the workspace, rerun with the approved `cargo test` escalation path.

**Step 3: Manual TUI verification**

Run:

```bash
cargo run
```

Manual checks:

- Submit a prompt that streams a long response.
- Confirm the UI updates smoothly during streaming.
- Type during streaming and confirm input remains responsive.
- Build a long transcript, scroll up/down, and confirm scroll latency stays low.
- Toggle `Ctrl+R` and switch views with `Ctrl+1..5`; confirm chat content updates correctly.
- Queue prompts while busy; confirm queue preview appears and updates.

**Step 4: Inspect final diff**

Run:

```bash
git diff --stat
git diff -- src/tui
```

Expected:

- New `src/tui/chat_render.rs`.
- Focused changes in `src/tui/state.rs`, `src/tui/views.rs`, and `src/tui/mod.rs`.
- No provider, memory, agent, or tool behavior changes.

**Step 5: Commit final cleanup if needed**

If Task 10 required cleanup changes:

```bash
git add src/tui
git commit -m "chore(tui): finalize virtualized chat renderer"
```

---

## Execution Notes

- Prefer one task per commit.
- Preserve existing TUI visual language unless a test proves the old rendering path is the bottleneck.
- Do not introduce a bespoke terminal backend in this plan. Revisit only after virtualized chat rendering is measured and still insufficient.
- Use `rg "build_chat_lines|chat_lines_dirty|chat_lines_cache"` after Task 7 to verify the old full-transcript cache path is gone.
- If a test requires exact Ratatui styled output, prefer checking text content/order and structural counts instead of brittle full `Debug` comparisons.
