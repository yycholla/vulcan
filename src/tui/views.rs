use ratatui::{
    Frame as TuiFrame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};

use super::markdown::render_markdown;
use super::state::{AppState, ChatRole, MessageSegment, SessionStatus};
use super::theme::{Palette, body, faint_bg};
use super::widgets::{fill, frame, message_header, section_header, ticker};

/// Public dispatch: render the active view inside `area`. The view writes
/// its desired cursor position into `app` via `cursor_set`.
pub fn render_view(f: &mut TuiFrame, area: Rect, app: &AppState) {
    fill(f, area, body());
    match app.view {
        View::TradingFloor => trading_floor(f, area, app),
        View::SingleStack => single_stack(f, area, app),
        View::SplitSessions => split_sessions(f, area, app),
        View::TiledMesh => tiled_mesh(f, area, app),
        View::TreeOfThought => tree_of_thought(f, area, app),
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum View {
    TradingFloor,
    SingleStack,
    SplitSessions,
    TiledMesh,
    TreeOfThought,
}

impl View {
    pub fn title(&self) -> &'static str {
        match self {
            View::TradingFloor => "agent · floor",
            View::SingleStack => "agent · single stack",
            View::SplitSessions => "agent · split sessions",
            View::TiledMesh => "agent · tiled mesh",
            View::TreeOfThought => "agent · tree of thought",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            View::TradingFloor => "05 trading floor",
            View::SingleStack => "01 single stack",
            View::SplitSessions => "02 split sessions",
            View::TiledMesh => "03 tiled mesh",
            View::TreeOfThought => "04 tree of thought",
        }
    }

    pub fn from_index(i: u8) -> Option<View> {
        match i {
            1 => Some(View::SingleStack),
            2 => Some(View::SplitSessions),
            3 => Some(View::TiledMesh),
            4 => Some(View::TreeOfThought),
            5 => Some(View::TradingFloor),
            _ => None,
        }
    }
}

// ─── shared chat rendering ──────────────────────────────────────────────

/// Compute and stash the max scroll offset for the chat viewport given the
/// pre-wrap line count and viewport height. Used by the auto-follow logic in
/// mod.rs to pin scroll to the bottom while `at_bottom` is true (YYC-69).
/// Note: the count is pre-wrap, so very long wrapped lines under-count the
/// real bottom by a few rows. Acceptable for follow-bottom UX; the user can
/// always nudge with Down to land exactly at the tail.
fn publish_chat_max_scroll(app: &AppState, line_count: usize, viewport_height: u16) {
    let max = (line_count as u16).saturating_sub(viewport_height);
    app.chat_max_scroll.set(max);
}

/// Build flat lines for the primary chat — user/agent messages with markdown.
/// `show_reasoning` toggles a stylized reasoning block before the first agent
/// message (used by views that show a reasoning trace).
fn build_chat_lines(app: &AppState, show_reasoning: bool, dense: bool) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    out.push(Line::from(Span::styled(
        format!("── session · {} ──", app.session_label),
        Style::default()
            .fg(Palette::MUTED)
            .add_modifier(Modifier::DIM),
    )));
    out.push(Line::from(""));

    if app.messages.is_empty() {
        out.push(Line::from(Span::styled(
            "Type a message below to start. Press Ctrl+1..5 to switch views.",
            Style::default().fg(Palette::MUTED),
        )));
        return out;
    }

    for (i, m) in app.messages.iter().enumerate() {
        let (role_label, accent) = match m.role {
            ChatRole::User => ("you", Palette::RED),
            ChatRole::Agent => ("agent", Palette::INK),
            ChatRole::System => ("system", Palette::YELLOW),
        };
        out.push(message_header(role_label, accent, None));

        let is_agent = matches!(m.role, ChatRole::Agent);

        if is_agent && !m.segments.is_empty() {
            // Live timeline (YYC-71): render reasoning, tool calls, and text
            // in arrival order. Reasoning fragments only emit when the
            // toggle is on; tool calls always emit.
            let mut text_emitted = false;
            for seg in &m.segments {
                match seg {
                    MessageSegment::Reasoning(r) if show_reasoning && !r.is_empty() => {
                        for l in super::widgets::reasoning_lines(r, false) {
                            out.push(l);
                        }
                    }
                    MessageSegment::Reasoning(_) => {}
                    MessageSegment::ToolCall {
                        name,
                        status,
                        params_summary,
                        output_preview,
                        elapsed_ms,
                    } => {
                        // YYC-74: structured tool-call card.
                        for line in super::widgets::tool_card(
                            name,
                            *status,
                            params_summary.as_deref(),
                            output_preview.as_deref(),
                            *elapsed_ms,
                            accent,
                        ) {
                            out.push(line);
                        }
                    }
                    MessageSegment::Text(t) if !t.is_empty() => {
                        text_emitted = true;
                        let rendered = render_markdown(t);
                        for line in rendered {
                            let mut spans =
                                vec![Span::styled("▎ ", Style::default().fg(accent))];
                            spans.extend(line.spans.into_iter());
                            out.push(Line::from(spans));
                        }
                    }
                    MessageSegment::Text(_) => {}
                }
            }
            // Show waiting placeholder when only reasoning has streamed and
            // no body text has appeared yet.
            if !text_emitted {
                let label = if m.has_reasoning() {
                    "▎ Answering…"
                } else {
                    "▎ Thinking…"
                };
                out.push(Line::from(Span::styled(
                    label,
                    Style::default()
                        .fg(Palette::MUTED)
                        .add_modifier(Modifier::SLOW_BLINK),
                )));
            }
        } else {
            // Hydrated history (no segment timeline) — fall back to the
            // legacy reasoning-then-content layout.
            if show_reasoning && is_agent && !m.reasoning.is_empty() {
                for l in super::widgets::reasoning_lines(&m.reasoning, false) {
                    out.push(l);
                }
            }
            if is_agent && m.content.is_empty() {
                let label = if m.reasoning.is_empty() {
                    "▎ Thinking…"
                } else {
                    "▎ Answering…"
                };
                out.push(Line::from(Span::styled(
                    label,
                    Style::default()
                        .fg(Palette::MUTED)
                        .add_modifier(Modifier::SLOW_BLINK),
                )));
            } else {
                let rendered = render_markdown(&m.content);
                for line in rendered {
                    let mut spans = vec![Span::styled("▎ ", Style::default().fg(accent))];
                    spans.extend(line.spans.into_iter());
                    out.push(Line::from(spans));
                }
            }
        }
        if !dense || i + 1 == app.messages.len() {
            out.push(Line::from(""));
        }
    }

    // YYC-59: inline action pills for an active AgentPause. Rendered
    // beneath the latest assistant message so the user sees what
    // they're being asked to choose.
    if let Some(p) = app.pending_pause.as_ref() {
        if !p.options.is_empty() {
            let mut pill_spans: Vec<Span<'static>> = Vec::new();
            pill_spans.push(Span::styled(
                "▎ ",
                Style::default().fg(Palette::INK),
            ));
            for (i, opt) in p.options.iter().enumerate() {
                if i > 0 {
                    pill_spans.push(Span::raw("  "));
                }
                let color = match opt.kind {
                    crate::pause::OptionKind::Primary => Palette::BLUE,
                    crate::pause::OptionKind::Neutral => Palette::INK,
                    crate::pause::OptionKind::Destructive => Palette::RED,
                };
                let filled = matches!(opt.kind, crate::pause::OptionKind::Primary);
                let label = format!("[{}] {}", opt.key, opt.label);
                pill_spans.push(super::widgets::pill(&label, color, filled));
            }
            out.push(Line::from(pill_spans));
            out.push(Line::from(""));
        }
    }

    // YYC-61: ghosted preview of pending queued submissions, rendered
    // beneath the latest agent message so the user sees what's been
    // staged behind the in-flight turn.
    if !app.queue.is_empty() {
        out.push(Line::from(Span::styled(
            format!("── queue · {} pending ──", app.queue.len()),
            Style::default()
                .fg(Palette::MUTED)
                .add_modifier(Modifier::DIM),
        )));
        for (idx, qmsg) in app.queue.iter().enumerate() {
            let preview: String = qmsg.chars().take(120).collect();
            out.push(Line::from(vec![
                Span::styled(
                    format!("▸ #{:<2} ", idx + 1),
                    Style::default()
                        .fg(Palette::MUTED)
                        .add_modifier(Modifier::DIM),
                ),
                Span::styled(
                    preview,
                    Style::default()
                        .fg(Palette::MUTED)
                        .add_modifier(Modifier::DIM),
                ),
            ]));
        }
        out.push(Line::from(""));
    }
    out
}

// ─── 01 SINGLE STACK ────────────────────────────────────────────────────

fn single_stack(f: &mut TuiFrame, area: Rect, app: &AppState) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(3)])
        .split(area);

    let inner = frame(
        f,
        layout[0],
        app.view.title(),
        Some("ses 1/1 · ●live"),
        None,
    );
    let lines = build_chat_lines(app, app.show_reasoning, false);
    publish_chat_max_scroll(app, lines.len(), inner.height);
    f.render_widget(
        Paragraph::new(lines)
            .style(body())
            .wrap(Wrap { trim: false })
            .scroll((app.scroll, 0)),
        Rect {
            x: inner.x + 1,
            y: inner.y,
            width: inner.width.saturating_sub(2),
            height: inner.height,
        },
    );

    let (cx, cy) = super::widgets::prompt_row(
        f,
        layout[1],
        app.mode_label(),
        &app.input,
        app.prompt_hints(),
        &app.model_status(),
        app.context_ratio(),
        app.thinking,
    );
    app.cursor_set(cx, cy);
}

// ─── 02 SPLIT SESSIONS ──────────────────────────────────────────────────

fn split_sessions(f: &mut TuiFrame, area: Rect, app: &AppState) {
    let outer = frame(f, area, app.view.title(), Some("5 sess · 2 live"), None);
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(3)])
        .split(outer);

    let h = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(28), Constraint::Min(40)])
        .split(v[0]);

    // Left rail
    fill(f, h[0], faint_bg());
    let rail_inner = section_header(f, h[0], "sessions", None);
    render_session_rail(f, rail_inner, app);

    // Right pane
    let main = h[1];
    // Active session header bar
    let header = Rect {
        x: main.x,
        y: main.y,
        width: main.width,
        height: 1,
    };
    let active_name = app
        .active_session()
        .map(|s| s.label.clone())
        .unwrap_or_else(|| app.session_label.clone());
    let lineage = app
        .active_session()
        .and_then(|s| s.lineage_label.clone())
        .or_else(|| {
            app.active_session()
                .and_then(|s| s.parent_session_id.as_deref())
                .map(|id| format!("branched from {}", id.chars().take(8).collect::<String>()))
        })
        .unwrap_or_else(|| "no lineage".into());
    let turn_count = app
        .active_session()
        .map(|s| s.message_count)
        .unwrap_or(app.messages.len());
    let header_text = format!(" {active_name}   · {lineage} · {turn_count} msgs",);
    let mut spans = vec![Span::styled(
        header_text,
        Style::default()
            .fg(Palette::PAPER)
            .bg(Palette::RED)
            .add_modifier(Modifier::BOLD),
    )];
    let used = spans
        .iter()
        .map(|s| s.content.chars().count() as u16)
        .sum::<u16>();
    let live_label = " ● LIVE ";
    if used + live_label.len() as u16 + 1 < main.width {
        let pad = " ".repeat((main.width - used - live_label.len() as u16) as usize);
        spans.push(Span::styled(
            pad,
            Style::default().fg(Palette::PAPER).bg(Palette::RED),
        ));
        spans.push(Span::styled(
            live_label.to_string(),
            Style::default()
                .fg(Palette::PAPER)
                .bg(Palette::RED)
                .add_modifier(Modifier::BOLD),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), header);

    let body_area = Rect {
        x: main.x,
        y: main.y + 1,
        width: main.width,
        height: main.height.saturating_sub(1),
    };
    let lines = build_chat_lines(app, app.show_reasoning, false);
    publish_chat_max_scroll(app, lines.len(), body_area.height.saturating_sub(1));
    f.render_widget(
        Paragraph::new(lines)
            .style(body())
            .wrap(Wrap { trim: false })
            .scroll((app.scroll, 0)),
        Rect {
            x: body_area.x + 1,
            y: body_area.y,
            width: body_area.width.saturating_sub(2),
            height: body_area.height,
        },
    );

    let (cx, cy) = super::widgets::prompt_row(
        f,
        v[1],
        app.mode_label(),
        &app.input,
        app.prompt_hints(),
        &app.model_status(),
        app.context_ratio(),
        app.thinking,
    );
    app.cursor_set(cx, cy);
}

fn render_session_rail(f: &mut TuiFrame, area: Rect, app: &AppState) {
    if area.height == 0 {
        return;
    }
    let mut lines: Vec<Line<'static>> = Vec::new();
    if app.sessions.is_empty() {
        lines.push(Line::from(Span::styled(
            " No saved sessions yet.",
            Style::default().fg(Palette::MUTED),
        )));
        lines.push(Line::from(Span::styled(
            " The active session will appear after hydration.",
            Style::default().fg(Palette::FAINT),
        )));
    }
    for s in &app.sessions {
        let status_color = match s.status {
            SessionStatus::Live => Palette::GREEN,
            SessionStatus::Saved => Palette::BLUE,
        };
        let bar = if s.is_active { "▌" } else { " " };
        let bar_color = if s.is_active {
            Palette::RED
        } else {
            Palette::FAINT
        };
        let bg = if s.is_active {
            Palette::PAPER
        } else {
            Palette::FAINT
        };
        let style = Style::default().fg(Palette::INK).bg(bg);
        lines.push(Line::from(vec![
            Span::styled(
                bar,
                Style::default()
                    .fg(bar_color)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" █ ", Style::default().fg(status_color).bg(bg)),
            Span::styled(s.label.clone(), style.add_modifier(Modifier::BOLD)),
        ]));
        lines.push(Line::from(vec![
            Span::styled(bar, Style::default().fg(bar_color).bg(bg)),
            Span::styled(
                format!(
                    " {:<6}    {:>4}m",
                    s.status.label().to_uppercase(),
                    s.message_count
                ),
                Style::default().fg(Palette::MUTED).bg(bg),
            ),
        ]));
        if let Some(lineage) = &s.lineage_label {
            lines.push(Line::from(vec![
                Span::styled(bar, Style::default().fg(bar_color).bg(bg)),
                Span::styled(
                    format!(" {}", lineage),
                    Style::default().fg(Palette::FAINT).bg(bg),
                ),
            ]));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " + NEW SESSION",
        Style::default()
            .fg(Palette::INK)
            .add_modifier(Modifier::BOLD),
    )));
    f.render_widget(Paragraph::new(lines).style(faint_bg()), area);
}

// ─── 03 TILED MESH ──────────────────────────────────────────────────────

fn tiled_mesh(f: &mut TuiFrame, area: Rect, app: &AppState) {
    let status = format!(
        "1 orchestrator · {} delegated",
        app.delegated_worker_count()
    );
    let outer = frame(f, area, app.view.title(), Some(&status), None);
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(3)])
        .split(outer);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(v[0]);
    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[0]);
    let bot = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[1]);
    let cells = [top[0], top[1], bot[0], bot[1]];
    let tiles = app.subagent_tiles();

    for (i, cell) in cells.iter().enumerate() {
        let Some(tile) = tiles.get(i) else {
            render_empty_activity_tile(f, *cell, "worker slot", "No delegated worker");
            continue;
        };
        let inner = section_header(
            f,
            *cell,
            &format!("{} · {}", tile.name, tile.role),
            Some(tile.color),
        );
        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::from(vec![
            Span::styled(
                "STATE ",
                Style::default()
                    .fg(Palette::MUTED)
                    .add_modifier(Modifier::DIM),
            ),
            Span::styled(
                tile.state.to_uppercase(),
                Style::default()
                    .fg(Palette::INK)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::from(""));
        for (n, l) in tile.log.iter().enumerate() {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:>02} ", n + 1),
                    Style::default().fg(Palette::MUTED),
                ),
                Span::styled(l.clone(), Style::default().fg(Palette::INK)),
            ]));
        }
        if i == 0 && app.show_reasoning {
            lines.push(Line::from(""));
            if let Some(reasoning) = app.latest_reasoning() {
                for l in super::widgets::reasoning_lines(reasoning, false) {
                    lines.push(l);
                }
            } else {
                lines.push(Line::from(Span::styled(
                    "No live reasoning trace yet.",
                    Style::default().fg(Palette::MUTED),
                )));
            }
        }
        f.render_widget(
            Paragraph::new(lines)
                .style(body())
                .wrap(Wrap { trim: false }),
            Rect {
                x: inner.x + 1,
                y: inner.y,
                width: inner.width.saturating_sub(2),
                height: inner.height,
            },
        );
    }

    let (cx, cy) = super::widgets::prompt_row(
        f,
        v[1],
        app.mode_label(),
        &app.input,
        app.prompt_hints(),
        &app.model_status(),
        app.context_ratio(),
        app.thinking,
    );
    app.cursor_set(cx, cy);
}

// ─── 04 TREE OF THOUGHT ─────────────────────────────────────────────────

fn tree_of_thought(f: &mut TuiFrame, area: Rect, app: &AppState) {
    let tree_nodes = app.tree_nodes();
    let summary = format!("{} branch runtime slots", app.branch_count());
    let outer = frame(f, area, app.view.title(), Some(&summary), None);
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(3)])
        .split(outer);
    let h = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(36), Constraint::Min(40)])
        .split(v[0]);

    // Left: tree
    fill(f, h[0], faint_bg());
    let inner = section_header(f, h[0], "decision tree", None);
    let mut lines: Vec<Line<'static>> = Vec::new();
    for n in &tree_nodes {
        let bg = if n.active {
            Palette::PAPER
        } else {
            Palette::FAINT
        };
        let bar = if n.active { "▌" } else { " " };
        let bar_color = if n.active {
            Palette::RED
        } else {
            Palette::FAINT
        };
        lines.push(Line::from(vec![
            Span::styled(
                bar,
                Style::default()
                    .fg(bar_color)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {} ", n.state),
                Style::default()
                    .fg(n.color)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                n.label.clone(),
                Style::default()
                    .fg(Palette::INK)
                    .bg(bg)
                    .add_modifier(if n.depth == 0 {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " Delegated branch runtime not enabled yet.",
        Style::default().fg(Palette::MUTED).bg(Palette::FAINT),
    )));
    lines.push(Line::from(Span::styled(
        " This pane currently shows the root orchestrator state only.",
        Style::default().fg(Palette::MUTED).bg(Palette::FAINT),
    )));
    f.render_widget(Paragraph::new(lines).style(faint_bg()), inner);

    // Right: focused stream
    let focus_inner = section_header(f, h[1], "focused stream", Some(Palette::YELLOW));
    let mut lines = Vec::new();
    lines.push(message_header("orchestrator", Palette::RED, Some("root")));
    lines.push(Line::from(Span::styled(
        format!("▎ {}", app.orchestration.active_task),
        Style::default().fg(Palette::INK),
    )));
    if app.show_reasoning {
        if let Some(reasoning) = app.latest_reasoning() {
            lines.push(Line::from(""));
            for l in super::widgets::reasoning_lines(reasoning, false) {
                lines.push(l);
            }
        }
    }
    if let Some(content) = app.latest_agent_content() {
        lines.push(Line::from(""));
        for line in render_markdown(content) {
            lines.push(Line::from(line.spans));
        }
    } else {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "No focused branch output yet.",
            Style::default().fg(Palette::MUTED),
        )));
    }
    f.render_widget(
        Paragraph::new(lines)
            .style(body())
            .wrap(Wrap { trim: false }),
        focus_inner,
    );

    let (cx, cy) = super::widgets::prompt_row(
        f,
        v[1],
        app.mode_label(),
        &app.input,
        app.prompt_hints(),
        &app.model_status(),
        app.context_ratio(),
        app.thinking,
    );
    app.cursor_set(cx, cy);
}

// ─── 05 TRADING FLOOR ───────────────────────────────────────────────────

fn trading_floor(f: &mut TuiFrame, area: Rect, app: &AppState) {
    let subagents = app.subagent_tiles();
    let status = format!(
        "{} sess · {} sub · ⌬ 38k tok/m",
        app.sessions.len(),
        subagents.len()
    );
    let outer = frame(f, area, app.view.title(), Some(&status), None);

    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(8),    // grid
            Constraint::Length(1), // ticker
            Constraint::Length(3), // prompt row
        ])
        .split(outer);

    // 3x2 grid
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(v[0]);
    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Ratio(12, 38),
            Constraint::Ratio(16, 38),
            Constraint::Ratio(10, 38),
        ])
        .split(rows[0]);
    let bot = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Ratio(12, 38),
            Constraint::Ratio(16, 38),
            Constraint::Ratio(10, 38),
        ])
        .split(rows[1]);

    // ── primary chat (top-left, red header)
    let primary_inner = section_header(f, top[0], "primary · auth-refactor", Some(Palette::RED));
    let lines = build_chat_lines(app, app.show_reasoning, true);
    publish_chat_max_scroll(app, lines.len(), primary_inner.height);
    f.render_widget(
        Paragraph::new(lines)
            .style(body())
            .wrap(Wrap { trim: false })
            .scroll((app.scroll, 0)),
        Rect {
            x: primary_inner.x + 1,
            y: primary_inner.y,
            width: primary_inner.width.saturating_sub(2),
            height: primary_inner.height,
        },
    );
    // visual divider on the right edge
    draw_v_divider(f, top[0]);

    // ── sub-agent feeds (top-mid)
    let sub_inner = section_header(f, top[1], "sub-agent feeds", None);
    let sub_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Ratio(1, 3),
            Constraint::Ratio(1, 3),
            Constraint::Ratio(1, 3),
        ])
        .split(sub_inner);
    for (i, cell) in sub_rows.iter().enumerate() {
        let Some(tile) = subagents.get(i) else {
            render_empty_activity_tile(f, *cell, "worker slot", "No delegated worker");
            continue;
        };
        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::from(vec![
            Span::styled(
                "█ ",
                Style::default().fg(tile.color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                tile.name.clone(),
                Style::default()
                    .fg(Palette::INK)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" · {}", tile.role),
                Style::default().fg(Palette::MUTED),
            ),
        ]));
        for (n, l) in tile.log.iter().enumerate() {
            let color = if n + 1 == tile.log.len() {
                Palette::INK
            } else {
                Palette::MUTED
            };
            lines.push(Line::from(Span::styled(
                format!("  {}", l),
                Style::default().fg(color),
            )));
        }
        f.render_widget(
            Paragraph::new(lines)
                .style(body())
                .wrap(Wrap { trim: false }),
            Rect {
                x: cell.x + 1,
                y: cell.y,
                width: cell.width.saturating_sub(2),
                height: cell.height,
            },
        );
    }
    draw_v_divider(f, top[1]);

    // ── sessions list (top-right)
    let ses_inner = section_header(f, top[2], "sessions", None);
    let mut lines: Vec<Line<'static>> = Vec::new();
    if app.sessions.is_empty() {
        lines.push(Line::from(Span::styled(
            " No saved sessions yet.",
            Style::default().fg(Palette::MUTED),
        )));
    }
    for s in &app.sessions {
        let bg = if s.is_active {
            Palette::FAINT
        } else {
            Palette::PAPER
        };
        let bar = if s.is_active { "▌" } else { " " };
        let accent = match s.status {
            SessionStatus::Live => Palette::GREEN,
            SessionStatus::Saved => Palette::BLUE,
        };
        lines.push(Line::from(vec![
            Span::styled(
                bar,
                Style::default()
                    .fg(Palette::RED)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" █ ", Style::default().fg(accent).bg(bg)),
            Span::styled(
                format!("{:<14}", s.label),
                Style::default().fg(Palette::INK).bg(bg),
            ),
            Span::styled(
                format!(" {}m", s.message_count),
                Style::default().fg(Palette::MUTED).bg(bg),
            ),
        ]));
    }
    f.render_widget(Paragraph::new(lines).style(body()), ses_inner);

    // ── tool log (bot-left) — live audit data when available
    let tl_inner = section_header(f, bot[0], "tool log", None);
    let log_rows = app.tool_log_view(tl_inner.height as usize);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let row_count = log_rows.len();
    for (i, row) in log_rows.iter().enumerate() {
        let last = i + 1 == row_count;
        let color = if last { Palette::INK } else { Palette::MUTED };
        lines.push(Line::from(vec![
            Span::styled(format!("{:>8} ", row.time), Style::default().fg(color)),
            Span::styled(
                format!("{:<5} ", row.actor),
                Style::default()
                    .fg(Palette::INK)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(row.msg.clone(), Style::default().fg(color)),
        ]));
    }
    f.render_widget(Paragraph::new(lines).style(body()), tl_inner);
    draw_v_divider(f, bot[0]);

    // ── diff (bot-mid). YYC-66: prefer the live edit sink; fall back to
    // demo data only if no real edit has happened yet AND the sink isn't
    // wired up (sink wired but empty → render an honest empty state).
    let live_diff = app.latest_diff();
    let diff_title: String = match (&live_diff, app.diff_sink.is_some()) {
        (Some(d), _) => format!("diff · {}", d.path),
        (None, true) => "diff · no edits this session".into(),
        (None, false) => "diff · users/auth.rs".into(),
    };
    let diff_inner = section_header(f, bot[1], &diff_title, None);
    let mut lines: Vec<Line<'static>> = Vec::new();
    if let Some(d) = live_diff {
        // Compact two-block render: prefix old lines with "-" in red,
        // new lines with "+" in green. Header shows tool + timestamp.
        lines.push(Line::from(Span::styled(
            format!("@@ {} · {}", d.tool, d.at.format("%H:%M:%S")),
            Style::default()
                .fg(Palette::MUTED)
                .add_modifier(Modifier::DIM),
        )));
        for line in d.before.lines() {
            lines.push(Line::from(Span::styled(
                format!("- {line}"),
                Style::default().fg(Palette::RED),
            )));
        }
        for line in d.after.lines() {
            lines.push(Line::from(Span::styled(
                format!("+ {line}"),
                Style::default().fg(Palette::GREEN),
            )));
        }
    } else if app.diff_sink.is_some() {
        // Sink wired but empty — be honest, no fake diff.
        lines.push(Line::from(Span::styled(
            "  No file edits captured yet. Run an edit_file or write_file tool.",
            Style::default().fg(Palette::MUTED),
        )));
    } else {
        // No sink at all — fall back to the demo data so the layout
        // still looks alive (e.g. headless preview environments).
        for ln in &app.diff_lines {
            let style = match ln.kind {
                DiffKind::Hunk => Style::default()
                    .fg(Palette::MUTED)
                    .add_modifier(Modifier::DIM),
                DiffKind::Added => Style::default().fg(Palette::GREEN),
                DiffKind::Removed => Style::default().fg(Palette::RED),
                DiffKind::Ctx => Style::default().fg(Palette::INK),
            };
            lines.push(Line::from(Span::styled(ln.text.clone(), style)));
        }
    }
    f.render_widget(
        Paragraph::new(lines)
            .style(body())
            .wrap(Wrap { trim: false }),
        Rect {
            x: diff_inner.x + 1,
            y: diff_inner.y,
            width: diff_inner.width.saturating_sub(2),
            height: diff_inner.height,
        },
    );
    draw_v_divider(f, bot[1]);

    // ── metrics (bot-right). YYC-67: replaced demo sparklines with real
    // session counters. Cost shows "—" when pricing isn't available
    // rather than inventing a number.
    let m_inner = section_header(f, bot[2], "metrics", None);
    let pad = m_inner.width.saturating_sub(2) as usize;

    let cost_str = match app.estimated_cost() {
        Some(c) => format!("${:.4}", c),
        None => "—".to_string(),
    };
    let elapsed_secs = app.session_started.elapsed().as_secs();
    let elapsed_str = if elapsed_secs >= 3600 {
        format!("{}h{:02}m", elapsed_secs / 3600, (elapsed_secs % 3600) / 60)
    } else if elapsed_secs >= 60 {
        format!("{}m{:02}s", elapsed_secs / 60, elapsed_secs % 60)
    } else {
        format!("{}s", elapsed_secs)
    };

    let entries: Vec<(&str, String, ratatui::style::Color)> = vec![
        (
            "input",
            super::state::format_thousands(app.prompt_tokens_total),
            Palette::INK,
        ),
        (
            "output",
            super::state::format_thousands(app.completion_tokens_total),
            Palette::INK,
        ),
        (
            "tools",
            app.tool_calls_total.to_string(),
            if app.tool_calls_total == 0 {
                Palette::MUTED
            } else {
                Palette::BLUE
            },
        ),
        (
            "errors",
            format!(
                "{} prov · {} tool",
                app.provider_errors_total, app.tool_errors_total
            ),
            if app.provider_errors_total + app.tool_errors_total == 0 {
                Palette::MUTED
            } else {
                Palette::RED
            },
        ),
        ("cost", cost_str, Palette::YELLOW),
        ("uptime", elapsed_str, Palette::MUTED),
    ];

    let mut lines: Vec<Line<'static>> = Vec::new();
    for (k, v, color) in entries {
        let used = k.len() + v.chars().count() + 2;
        let middle = if pad > used {
            " ".repeat(pad - used)
        } else {
            String::new()
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {k}"), Style::default().fg(Palette::MUTED)),
            Span::styled(middle, Style::default().fg(Palette::PAPER)),
            Span::styled(v, Style::default().fg(color).add_modifier(Modifier::BOLD)),
        ]));
    }
    if app.pricing.is_none() {
        lines.push(Line::from(Span::styled(
            " (catalog has no pricing for this model — cost shown as —)",
            Style::default()
                .fg(Palette::MUTED)
                .add_modifier(Modifier::DIM),
        )));
    }
    f.render_widget(Paragraph::new(lines).style(body()), m_inner);

    // horizontal divider between rows
    if rows[0].height > 0 {
        let div_y = rows[0].y + rows[0].height - 1;
        let div = "─".repeat(v[0].width as usize);
        f.render_widget(
            Paragraph::new(div).style(Style::default().fg(Palette::INK).bg(Palette::PAPER)),
            Rect {
                x: v[0].x,
                y: div_y,
                width: v[0].width,
                height: 1,
            },
        );
    }

    // ── ticker
    let ticker_cells: Vec<(String, String, ratatui::style::Color)> = app
        .ticker_cells()
        .iter()
        .map(|t| (t.sub.clone(), t.msg.clone(), t.color))
        .collect();
    ticker(f, v[1], &ticker_cells);

    // ── prompt row
    let (cx, cy) = super::widgets::prompt_row(
        f,
        v[2],
        app.mode_label(),
        &app.input,
        app.prompt_hints(),
        &app.model_status(),
        app.context_ratio(),
        app.thinking,
    );
    app.cursor_set(cx, cy);
}

fn render_empty_activity_tile(f: &mut TuiFrame, area: Rect, label: &str, msg: &str) {
    let inner = section_header(f, area, label, Some(Palette::MUTED));
    let lines = vec![
        Line::from(Span::styled(
            "STATE EMPTY",
            Style::default()
                .fg(Palette::MUTED)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(msg, Style::default().fg(Palette::INK))),
        Line::from(Span::styled(
            "Delegation/runtime wiring has not been enabled yet.",
            Style::default().fg(Palette::MUTED),
        )),
    ];
    f.render_widget(
        Paragraph::new(lines)
            .style(body())
            .wrap(Wrap { trim: false }),
        Rect {
            x: inner.x + 1,
            y: inner.y,
            width: inner.width.saturating_sub(2),
            height: inner.height,
        },
    );
}

fn draw_v_divider(f: &mut TuiFrame, area: Rect) {
    if area.width < 2 || area.height == 0 {
        return;
    }
    let x = area.x + area.width - 1;
    let lines: Vec<Line<'static>> = (0..area.height)
        .map(|_| {
            Line::from(Span::styled(
                "│",
                Style::default().fg(Palette::INK).bg(Palette::PAPER),
            ))
        })
        .collect();
    f.render_widget(
        Paragraph::new(lines),
        Rect {
            x,
            y: area.y,
            width: 1,
            height: area.height,
        },
    );
}

// ─── diff types live with the trading floor (kept here for view-local data) ──

#[derive(Copy, Clone)]
pub enum DiffKind {
    Hunk,
    Added,
    Removed,
    Ctx,
}

pub struct DiffLine {
    pub text: String,
    pub kind: DiffKind,
}
