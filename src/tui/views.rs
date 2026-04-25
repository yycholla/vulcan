use ratatui::{
    Frame as TuiFrame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};

use super::markdown::render_markdown;
use super::state::{AppState, ChatRole};
use super::theme::{Palette, body, faint_bg};
use super::widgets::{
    ToolStatus, fill, frame, message_header, section_header, sparkline, ticker, tool_call_lines,
};

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

/// Build flat lines for the primary chat — user/agent messages with markdown.
/// `show_reasoning` toggles a stylized reasoning block before the first agent
/// message (used by views that show a reasoning trace).
fn build_chat_lines(app: &AppState, show_reasoning: bool, dense: bool) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    out.push(Line::from(Span::styled(
        format!("── session · {} ──", app.session_label),
        Style::default().fg(Palette::MUTED).add_modifier(Modifier::DIM),
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

        // Reasoning trace from a thinking-mode model, when present and toggle is on.
        // Rendered before the agent's content so the visual order matches the
        // model's natural output (think → answer).
        if show_reasoning && matches!(m.role, ChatRole::Agent) && !m.reasoning.is_empty() {
            for l in super::widgets::reasoning_lines(&m.reasoning, false) {
                out.push(l);
            }
        }

        if matches!(m.role, ChatRole::Agent) && m.content.is_empty() {
            // Streaming and content hasn't started yet — show "thinking…" hint.
            // If reasoning is already streaming above, this still tells the user
            // they're waiting on the answer (not lost).
            let label = if m.reasoning.is_empty() { "▎ Thinking…" } else { "▎ Answering…" };
            out.push(Line::from(Span::styled(
                label,
                Style::default().fg(Palette::MUTED).add_modifier(Modifier::SLOW_BLINK),
            )));
        } else {
            // Render body with a left accent bar, inline.
            let rendered = render_markdown(&m.content);
            for line in rendered {
                let mut spans = vec![Span::styled("▎ ", Style::default().fg(accent))];
                spans.extend(line.spans.into_iter());
                out.push(Line::from(spans));
            }
        }
        if !dense || i + 1 == app.messages.len() {
            out.push(Line::from(""));
        }
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
        &[("↵", "send"), ("⌃T", "tools"), ("⌃K", "sessions"), ("/", "cmds")],
        &app.model_status(),
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
    let header = Rect { x: main.x, y: main.y, width: main.width, height: 1 };
    let active_name = app
        .sessions
        .iter()
        .find(|s| s.active)
        .map(|s| s.name.clone())
        .unwrap_or_else(|| app.session_label.clone());
    let header_text = format!(
        " {active_name}   · branched from main · {} turns",
        app.messages.len() / 2
    );
    let mut spans = vec![Span::styled(
        header_text,
        Style::default().fg(Palette::PAPER).bg(Palette::RED).add_modifier(Modifier::BOLD),
    )];
    let used = spans.iter().map(|s| s.content.chars().count() as u16).sum::<u16>();
    let live_label = " ● LIVE ";
    if used + live_label.len() as u16 + 1 < main.width {
        let pad = " ".repeat((main.width - used - live_label.len() as u16) as usize);
        spans.push(Span::styled(
            pad,
            Style::default().fg(Palette::PAPER).bg(Palette::RED),
        ));
        spans.push(Span::styled(
            live_label.to_string(),
            Style::default().fg(Palette::PAPER).bg(Palette::RED).add_modifier(Modifier::BOLD),
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
    f.render_widget(
        Paragraph::new(lines).style(body()).wrap(Wrap { trim: false }).scroll((app.scroll, 0)),
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
        &[("↵", "send"), ("⌃K", "sessions"), ("⌃N", "new"), ("/", "cmds")],
        &app.model_status(),
        app.thinking,
    );
    app.cursor_set(cx, cy);
}

fn render_session_rail(f: &mut TuiFrame, area: Rect, app: &AppState) {
    if area.height == 0 {
        return;
    }
    let mut lines: Vec<Line<'static>> = Vec::new();
    for s in &app.sessions {
        let status_color = match s.status.as_str() {
            "live" => Palette::GREEN,
            "wait" => Palette::YELLOW,
            "done" => Palette::MUTED,
            _ => Palette::FAINT,
        };
        let bar = if s.active { "▌" } else { " " };
        let bar_color = if s.active { Palette::RED } else { Palette::FAINT };
        let bg = if s.active { Palette::PAPER } else { Palette::FAINT };
        let style = Style::default().fg(Palette::INK).bg(bg);
        lines.push(Line::from(vec![
            Span::styled(bar, Style::default().fg(bar_color).bg(bg).add_modifier(Modifier::BOLD)),
            Span::styled(" █ ", Style::default().fg(status_color).bg(bg)),
            Span::styled(format!("{}", s.name), style.add_modifier(Modifier::BOLD)),
        ]));
        lines.push(Line::from(vec![
            Span::styled(bar, Style::default().fg(bar_color).bg(bg)),
            Span::styled(
                format!(" {:<6}    {:>4}", s.status.to_uppercase(), s.tokens),
                Style::default().fg(Palette::MUTED).bg(bg),
            ),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " + NEW SESSION",
        Style::default().fg(Palette::INK).add_modifier(Modifier::BOLD),
    )));
    f.render_widget(Paragraph::new(lines).style(faint_bg()), area);
}

// ─── 03 TILED MESH ──────────────────────────────────────────────────────

fn tiled_mesh(f: &mut TuiFrame, area: Rect, app: &AppState) {
    let outer = frame(f, area, app.view.title(), Some("1 + 3 sub-agents"), None);
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

    for (i, cell) in cells.iter().enumerate() {
        let tile = &app.subagents[i.min(app.subagents.len() - 1)];
        let inner = section_header(f, *cell, &format!("{} · {}", tile.name, tile.role), Some(tile.color));
        let mut lines: Vec<Line<'static>> = Vec::new();
        let spark = sparkline(&tile.cpu);
        lines.push(Line::from(vec![
            Span::styled("STATE ", Style::default().fg(Palette::MUTED).add_modifier(Modifier::DIM)),
            Span::styled(
                tile.state.to_uppercase(),
                Style::default().fg(Palette::INK).add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled("CPU ", Style::default().fg(Palette::MUTED).add_modifier(Modifier::DIM)),
            Span::styled(spark, Style::default().fg(tile.color).add_modifier(Modifier::BOLD)),
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
            for l in super::widgets::reasoning_lines("holding diff merge until billing-svc clears tests", false) {
                lines.push(l);
            }
        }
        f.render_widget(
            Paragraph::new(lines).style(body()).wrap(Wrap { trim: false }),
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
        "CMD",
        &app.input,
        &[("↵", "route to main"), ("⌃J", "switch tile"), ("/", "cmds")],
        &app.model_status(),
        app.thinking,
    );
    app.cursor_set(cx, cy);
}

// ─── 04 TREE OF THOUGHT ─────────────────────────────────────────────────

fn tree_of_thought(f: &mut TuiFrame, area: Rect, app: &AppState) {
    let outer = frame(f, area, app.view.title(), Some("3 branches · 1 active"), None);
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
    for n in &app.tree {
        let bg = if n.active { Palette::PAPER } else { Palette::FAINT };
        let bar = if n.active { "▌" } else { " " };
        let bar_color = if n.active { Palette::RED } else { Palette::FAINT };
        lines.push(Line::from(vec![
            Span::styled(
                bar,
                Style::default().fg(bar_color).bg(bg).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {} ", n.state),
                Style::default().fg(n.color).bg(bg).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                n.label.clone(),
                Style::default().fg(Palette::INK).bg(bg).add_modifier(if n.depth == 0 {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
            ),
        ]));
    }
    lines.push(Line::from(""));
    for hint in [
        " j/k navigate · ↵ focus",
        " b new branch · x kill",
        " m merge into parent",
    ] {
        lines.push(Line::from(Span::styled(
            hint,
            Style::default().fg(Palette::MUTED).bg(Palette::FAINT),
        )));
    }
    f.render_widget(Paragraph::new(lines).style(faint_bg()), inner);

    // Right: focused stream
    let focus_header = Rect {
        x: h[1].x,
        y: h[1].y,
        width: h[1].width,
        height: 1,
    };
    let mut text = " ▶ FOCUSED: PLAN A → BILLING-SVC".to_string();
    if (text.chars().count() as u16) < focus_header.width {
        text.push_str(&" ".repeat((focus_header.width as usize) - text.chars().count()));
    }
    f.render_widget(
        Paragraph::new(text).style(
            Style::default().fg(Palette::INK).bg(Palette::YELLOW).add_modifier(Modifier::BOLD),
        ),
        focus_header,
    );
    let body_area = Rect {
        x: h[1].x,
        y: h[1].y + 1,
        width: h[1].width,
        height: h[1].height.saturating_sub(1),
    };
    let mut lines = Vec::new();
    lines.push(message_header("orchestrator", Palette::RED, Some("root")));
    lines.push(Line::from(Span::styled(
        "▎ I forked three plans. A wins on test coverage; C is exploratory.",
        Style::default().fg(Palette::INK),
    )));
    lines.push(Line::from(""));
    lines.push(message_header("billing-svc", Palette::YELLOW, Some("a.2")));
    lines.push(Line::from(Span::styled(
        "▎ Found 6 callsites. Patching AuthMiddleware::verify.",
        Style::default().fg(Palette::INK),
    )));
    if app.show_reasoning {
        lines.push(Line::from(""));
        for l in super::widgets::reasoning_lines(
            "Trait bound on Verify needs lifetime relaxation — propagating up.",
            false,
        ) {
            lines.push(l);
        }
    }
    lines.push(Line::from(""));
    for l in tool_call_lines(
        "edit_file",
        "services/billing/src/auth.rs L88-L142",
        ToolStatus::Run,
        None,
    ) {
        lines.push(l);
    }
    f.render_widget(
        Paragraph::new(lines).style(body()).wrap(Wrap { trim: false }),
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
        "ASK",
        &app.input,
        &[("↵", "send"), ("M", "merge"), ("X", "kill"), ("B", "branch")],
        &app.model_status(),
        app.thinking,
    );
    app.cursor_set(cx, cy);
}

// ─── 05 TRADING FLOOR ───────────────────────────────────────────────────

fn trading_floor(f: &mut TuiFrame, area: Rect, app: &AppState) {
    let status = format!(
        "{} sess · {} sub · ⌬ 38k tok/m",
        app.sessions.len(),
        app.subagents.len()
    );
    let outer = frame(f, area, app.view.title(), Some(&status), None);

    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(8),       // grid
            Constraint::Length(1),    // ticker
            Constraint::Length(3),    // prompt row
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
        let tile = &app.subagents[i.min(app.subagents.len() - 1)];
        let mut lines: Vec<Line<'static>> = Vec::new();
        let spark = sparkline(&tile.cpu);
        lines.push(Line::from(vec![
            Span::styled("█ ", Style::default().fg(tile.color).add_modifier(Modifier::BOLD)),
            Span::styled(
                tile.name.clone(),
                Style::default().fg(Palette::INK).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" · {}", tile.role),
                Style::default().fg(Palette::MUTED),
            ),
            Span::styled(
                format!("   {}", spark),
                Style::default().fg(tile.color),
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
            Paragraph::new(lines).style(body()).wrap(Wrap { trim: false }),
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
    for s in &app.sessions {
        let bg = if s.active { Palette::FAINT } else { Palette::PAPER };
        let bar = if s.active { "▌" } else { " " };
        lines.push(Line::from(vec![
            Span::styled(
                bar,
                Style::default().fg(Palette::RED).bg(bg).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" █ ", Style::default().fg(s.color).bg(bg)),
            Span::styled(
                format!("{:<14}", s.name),
                Style::default().fg(Palette::INK).bg(bg),
            ),
            Span::styled(
                format!(" {}", s.tokens),
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
            Span::styled(
                format!("{:>8} ", row.time),
                Style::default().fg(color),
            ),
            Span::styled(
                format!("{:<5} ", row.actor),
                Style::default().fg(Palette::INK).add_modifier(Modifier::BOLD),
            ),
            Span::styled(row.msg.clone(), Style::default().fg(color)),
        ]));
    }
    f.render_widget(Paragraph::new(lines).style(body()), tl_inner);
    draw_v_divider(f, bot[0]);

    // ── diff (bot-mid)
    let diff_inner = section_header(f, bot[1], "diff · users/auth.rs", None);
    let mut lines: Vec<Line<'static>> = Vec::new();
    for ln in &app.diff_lines {
        let style = match ln.kind {
            DiffKind::Hunk => Style::default().fg(Palette::MUTED).add_modifier(Modifier::DIM),
            DiffKind::Added => Style::default().fg(Palette::GREEN),
            DiffKind::Removed => Style::default().fg(Palette::RED),
            DiffKind::Ctx => Style::default().fg(Palette::INK),
        };
        lines.push(Line::from(Span::styled(ln.text.clone(), style)));
    }
    f.render_widget(
        Paragraph::new(lines).style(body()).wrap(Wrap { trim: false }),
        Rect {
            x: diff_inner.x + 1,
            y: diff_inner.y,
            width: diff_inner.width.saturating_sub(2),
            height: diff_inner.height,
        },
    );
    draw_v_divider(f, bot[1]);

    // ── metrics (bot-right)
    let m_inner = section_header(f, bot[2], "metrics", None);
    let metrics: Vec<(&str, Vec<u16>, ratatui::style::Color)> = vec![
        ("tok/m", vec![2, 4, 3, 5, 7, 8, 6, 9, 7, 8], Palette::RED),
        ("tools", vec![1, 1, 2, 3, 2, 4, 5, 3, 4, 6], Palette::BLUE),
        ("cost ", vec![1, 2, 2, 3, 4, 4, 5, 6, 7, 8], Palette::YELLOW),
        ("err  ", vec![0, 0, 0, 1, 0, 0, 0, 0, 1, 0], Palette::MUTED),
    ];
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (label, vals, color) in metrics {
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {label}  "),
                Style::default().fg(Palette::MUTED).add_modifier(Modifier::BOLD),
            ),
            Span::styled(sparkline(&vals), Style::default().fg(color).add_modifier(Modifier::BOLD)),
        ]));
    }
    lines.push(Line::from(Span::styled(
        format!(" {}", "─".repeat(m_inner.width.saturating_sub(2) as usize)),
        Style::default().fg(Palette::INK),
    )));
    let pad = m_inner.width.saturating_sub(2) as usize;
    for (k, v) in [("session", "$0.84"), ("today", "$12.40"), ("cap", "$50.00")] {
        let used = k.len() + v.len() + 2;
        let middle = if pad > used { " ".repeat(pad - used) } else { String::new() };
        let color = if k == "cap" { Palette::RED } else { Palette::INK };
        lines.push(Line::from(vec![
            Span::styled(format!(" {k}"), Style::default().fg(Palette::MUTED)),
            Span::styled(middle, Style::default().fg(Palette::PAPER)),
            Span::styled(
                v.to_string(),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
        ]));
    }
    f.render_widget(Paragraph::new(lines).style(body()), m_inner);

    // horizontal divider between rows
    if rows[0].height > 0 {
        let div_y = rows[0].y + rows[0].height - 1;
        let div = "─".repeat(v[0].width as usize);
        f.render_widget(
            Paragraph::new(div).style(Style::default().fg(Palette::INK).bg(Palette::PAPER)),
            Rect { x: v[0].x, y: div_y, width: v[0].width, height: 1 },
        );
    }

    // ── ticker
    let ticker_cells: Vec<(String, String, ratatui::style::Color)> = app
        .ticker
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
        &[("↵", "send"), ("⌃1-5", "view"), ("/", "cmds")],
        &app.model_status(),
        app.thinking,
    );
    app.cursor_set(cx, cy);
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
        Rect { x, y: area.y, width: 1, height: area.height },
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

