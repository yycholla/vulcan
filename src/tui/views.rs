use figlet_rs::FIGlet;
use ratatui::{
    Frame as TuiFrame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};

use super::chat_render::{ChatRenderOptions, VisibleChatLines};
use super::markdown::render_markdown;
use super::state::{AppState, DiffStyle, SessionStatus};
use super::theme::{Palette, body, faint_bg};
use super::widgets::{fill, frame, message_header, section_header, ticker};

/// Public dispatch: render the active view inside `area`. The view writes
/// its desired cursor position into `app` via `cursor_set`.
pub fn render_view(f: &mut TuiFrame, area: Rect, app: &AppState) {
    fill(f, area, body());
    if let Some(frame_data) = app.active_canvas_frame() {
        render_canvas(f, area, app, frame_data);
        return;
    }
    match app.view {
        View::TradingFloor => trading_floor(f, area, app),
        View::SingleStack => single_stack(f, area, app),
        View::SplitSessions => split_sessions(f, area, app),
        View::TiledMesh => tiled_mesh(f, area, app),
        View::TreeOfThought => tree_of_thought(f, area, app),
    }
}

fn render_canvas(
    f: &mut TuiFrame,
    area: Rect,
    app: &AppState,
    frame_data: vulcan_frontend_api::CanvasFrame,
) {
    let title = if frame_data.title.is_empty() {
        "extension canvas"
    } else {
        frame_data.title.as_str()
    };
    let inner = frame(f, area, title, Some("Esc/Ctrl+C exits"), None, &app.theme);
    let lines = frame_data
        .lines
        .into_iter()
        .map(Line::from)
        .collect::<Vec<_>>();
    f.render_widget(
        Paragraph::new(lines)
            .style(body())
            .wrap(Wrap { trim: false }),
        inner,
    );
    app.cursor_set(inner.x, inner.y);
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

fn build_chat_window(
    app: &AppState,
    show_reasoning: bool,
    dense: bool,
    width: u16,
    height: u16,
    include_prefix: bool,
    include_welcome: bool,
) -> VisibleChatLines {
    let window_start = usize::from(app.scroll);
    let window_end = window_start.saturating_add(usize::from(height));
    let mut total_lines = 0usize;
    let mut lines = Vec::with_capacity(usize::from(height));

    if include_prefix {
        let prefix = build_chat_prefix_lines(app);
        push_visible_fixed_lines(
            &mut lines,
            &prefix,
            &mut total_lines,
            window_start,
            window_end,
        );
    }

    if include_welcome {
        let welcome = build_chat_welcome_lines(width, &app.theme);
        push_visible_fixed_lines(
            &mut lines,
            &welcome,
            &mut total_lines,
            window_start,
            window_end,
        );
    }

    if !app.messages.is_empty() {
        let message_start = total_lines;
        let options = ChatRenderOptions {
            show_reasoning,
            dense,
            width,
            muted_style: app.theme.muted,
        };
        let message_scroll = window_start.saturating_sub(message_start) as u16;
        let remaining_height = height.saturating_sub(lines.len() as u16);
        let message_window = app.chat_render_store.borrow_mut().visible_lines(
            &app.messages,
            options,
            &app.theme,
            message_scroll,
            remaining_height,
            app.pending_pause.as_ref(),
            app.queue.len(),
        );
        total_lines = total_lines.saturating_add(message_window.total_lines);
        lines.extend(message_window.lines);
        if dense {
            push_visible_fixed_lines(
                &mut lines,
                &[Line::from("")],
                &mut total_lines,
                window_start,
                window_end,
            );
        }
    }

    let suffix = build_chat_suffix_lines(app);
    push_visible_fixed_lines(
        &mut lines,
        &suffix,
        &mut total_lines,
        window_start,
        window_end,
    );

    VisibleChatLines { lines, total_lines }
}

fn build_chat_prefix_lines(app: &AppState) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled(
            format!("── session · {} ──", app.session_label),
            app.theme.muted.add_modifier(Modifier::DIM),
        )),
        Line::from(""),
    ];

    if app.messages.is_empty() {
        lines.push(Line::from(Span::styled(
            "Type a message below to start. Press Ctrl+1..5 to switch views.",
            app.theme.muted,
        )));
    }

    lines
}

fn build_chat_welcome_lines(width: u16, theme: &super::theme::Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.extend(figlet_welcome_banner("VULCAN", width, theme));

    lines.push(Line::from(Span::styled(
        "VULCAN · local agent workbench",
        theme.muted.add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        "Command AI like a tool, not a conversation.",
        theme.muted.add_modifier(Modifier::ITALIC),
    )));
    lines.push(Line::from(vec![
        Span::styled("[Enter]", Style::default().fg(theme.body_fg)),
        Span::styled(" send  ", theme.muted),
        Span::styled("[/]", Style::default().fg(theme.body_fg)),
        Span::styled(" commands  ", theme.muted),
        Span::styled("[Ctrl+K]", Style::default().fg(theme.body_fg)),
        Span::styled(" sessions", theme.muted),
    ]));
    lines.push(Line::from(""));
    lines
}

fn figlet_welcome_banner(
    text: &str,
    width: u16,
    theme: &super::theme::Theme,
) -> Vec<Line<'static>> {
    let width = usize::from(width);
    for font in [FIGlet::standard, FIGlet::small] {
        let Ok(font) = font() else {
            continue;
        };
        let Some(figure) = font.convert(text) else {
            continue;
        };
        let rows = figure
            .as_str()
            .lines()
            .map(|line| line.trim_end().to_string())
            .collect::<Vec<_>>();
        if !rows.is_empty() && rows.iter().all(|line| line.chars().count() <= width) {
            return rows
                .into_iter()
                .map(|line| {
                    Line::from(Span::styled(
                        line,
                        theme.accent.add_modifier(Modifier::BOLD),
                    ))
                })
                .collect();
        }
    }

    vec![Line::from(Span::styled(
        text.to_string(),
        theme.accent.add_modifier(Modifier::BOLD),
    ))]
}

fn build_chat_suffix_lines(app: &AppState) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    if let Some(p) = app.pending_pause.as_ref()
        && !p.options.is_empty()
    {
        let mut pill_spans: Vec<Span<'static>> = Vec::new();
        pill_spans.push(Span::styled("▎ ", app.theme.user));
        for (i, opt) in p.options.iter().enumerate() {
            if i > 0 {
                pill_spans.push(Span::raw("  "));
            }
            let color = match opt.kind {
                crate::pause::OptionKind::Primary => Palette::BLUE,
                crate::pause::OptionKind::Neutral => app.theme.body_fg,
                crate::pause::OptionKind::Destructive => Palette::RED,
            };
            let filled = matches!(opt.kind, crate::pause::OptionKind::Primary);
            let label = format!("[{}] {}", opt.key, opt.label);
            pill_spans.push(super::widgets::pill(&label, color, filled));
        }
        lines.push(Line::from(pill_spans));
        lines.push(Line::from(""));
    }

    if !app.queue.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("── queue · {} pending ──", app.queue.len()),
            app.theme.muted.add_modifier(Modifier::DIM),
        )));
        for (idx, qmsg) in app.queue.iter().enumerate() {
            let preview: String = qmsg.chars().take(120).collect();
            lines.push(Line::from(vec![
                Span::styled(
                    format!("▸ #{:<2} ", idx + 1),
                    app.theme.muted.add_modifier(Modifier::DIM),
                ),
                Span::styled(preview, app.theme.muted.add_modifier(Modifier::DIM)),
            ]));
        }
        lines.push(Line::from(""));
    }

    lines
}

fn push_visible_fixed_lines(
    out: &mut Vec<Line<'static>>,
    segment: &[Line<'static>],
    total_lines: &mut usize,
    window_start: usize,
    window_end: usize,
) {
    let segment_start = *total_lines;
    let segment_end = segment_start.saturating_add(segment.len());
    *total_lines = segment_end;

    if segment_end <= window_start || segment_start >= window_end {
        return;
    }

    let start = window_start.saturating_sub(segment_start);
    let end = segment.len().min(window_end.saturating_sub(segment_start));
    out.extend(segment[start..end].iter().cloned());
}

// ─── 01 SINGLE STACK ────────────────────────────────────────────────────

fn single_stack(f: &mut TuiFrame, area: Rect, app: &AppState) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),
            Constraint::Length(super::widgets::prompt_row_height(
                &app.input,
                area.width,
                app.mode_label(),
            )),
        ])
        .split(area);

    let inner = frame(
        f,
        layout[0],
        "vulcan · single stack",
        Some("focus"),
        app.theme.accent.fg,
        &app.theme,
    );

    if inner.height == 0 {
        return;
    }

    let header = Rect {
        x: inner.x + 1,
        y: inner.y,
        width: inner.width.saturating_sub(2),
        height: 1,
    };
    render_single_stack_status(f, header, app);

    let chat_area = Rect {
        x: inner.x + 1,
        y: inner.y + 1,
        width: inner.width.saturating_sub(2),
        height: inner.height.saturating_sub(1),
    };
    let chat_w = chat_area.width;
    let window = build_chat_window(
        app,
        app.show_reasoning,
        false,
        chat_w,
        chat_area.height,
        false,
        true,
    );
    publish_chat_max_scroll(app, window.total_lines, chat_area.height);
    f.render_widget(
        Paragraph::new(window.lines)
            .style(body())
            .wrap(Wrap { trim: false }),
        chat_area,
    );

    let (cx, cy) = super::widgets::prompt_row(
        f,
        layout[1],
        app.mode_label(),
        app.prompt_editor.textarea(),
        app.prompt_hints(),
        &app.model_status(),
        app.context_ratio(),
        app.thinking,
        &app.theme,
    );
    app.cursor_set(cx, cy);
}

fn render_single_stack_status(f: &mut TuiFrame, area: Rect, app: &AppState) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let session = format!("[SESSION] {}", app.session_label);
    let live = if app.thinking { "[BUSY]" } else { "[READY]" };
    let queue = if app.queue.is_empty() {
        String::new()
    } else {
        format!(" · {} queued", app.queue.len())
    };
    let reasoning = if app.show_reasoning {
        " · reasoning on"
    } else {
        " · reasoning hidden"
    };
    let tape = single_stack_activity_tape(app);
    let text = format!(" {tape}  {session}   {live}{queue}{reasoning}");
    let style = if app.thinking {
        Style::default()
            .fg(Palette::YELLOW)
            .add_modifier(Modifier::BOLD)
    } else {
        app.theme.muted.add_modifier(Modifier::BOLD)
    };
    f.render_widget(
        Paragraph::new(truncate_to_width(&text, area.width)).style(style),
        area,
    );
}

fn single_stack_activity_tape(app: &AppState) -> &'static str {
    if !app.thinking {
        return "[..::]";
    }
    const FRAMES: [&str; 8] = [
        "[>   ]", "[=>  ]", "[==> ]", "[===>]", "[ <==]", "[  <=]", "[   <]", "[.  .]",
    ];
    let frame = (app.session_started.elapsed().as_millis() / 180) as usize % FRAMES.len();
    FRAMES[frame]
}

fn truncate_to_width(text: &str, width: u16) -> String {
    let width = usize::from(width);
    let mut out = String::new();
    for ch in text.chars().take(width) {
        out.push(ch);
    }
    out
}

// ─── 02 SPLIT SESSIONS ──────────────────────────────────────────────────

fn split_sessions(f: &mut TuiFrame, area: Rect, app: &AppState) {
    let outer = frame(
        f,
        area,
        app.view.title(),
        Some("5 sess · 2 live"),
        None,
        &app.theme,
    );
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),
            Constraint::Length(super::widgets::prompt_row_height(
                &app.input,
                area.width,
                app.mode_label(),
            )),
        ])
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
    let accent_bar = Style::default()
        .fg(app.theme.accent.fg.unwrap_or(Color::Reset))
        .add_modifier(Modifier::BOLD);
    let mut spans = vec![Span::styled(header_text, accent_bar)];
    let used = spans
        .iter()
        .map(|s| s.content.chars().count() as u16)
        .sum::<u16>();
    let live_label = " ● LIVE ";
    if used + live_label.len() as u16 + 1 < main.width {
        let pad = " ".repeat((main.width - used - live_label.len() as u16) as usize);
        spans.push(Span::raw(pad));
        spans.push(Span::styled(live_label.to_string(), accent_bar));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), header);

    let body_area = Rect {
        x: main.x,
        y: main.y + 1,
        width: main.width,
        height: main.height.saturating_sub(1),
    };
    let chat_width = body_area.width.saturating_sub(2);
    let chat_height = body_area.height;
    let window = build_chat_window(
        app,
        app.show_reasoning,
        false,
        chat_width,
        chat_height,
        true,
        false,
    );
    publish_chat_max_scroll(app, window.total_lines, chat_height.saturating_sub(1));
    f.render_widget(
        Paragraph::new(window.lines)
            .style(body())
            .wrap(Wrap { trim: false }),
        Rect {
            x: body_area.x + 1,
            y: body_area.y,
            width: chat_width,
            height: body_area.height,
        },
    );

    let (cx, cy) = super::widgets::prompt_row(
        f,
        v[1],
        app.mode_label(),
        app.prompt_editor.textarea(),
        app.prompt_hints(),
        &app.model_status(),
        app.context_ratio(),
        app.thinking,
        &app.theme,
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
            app.theme.muted,
        )));
        lines.push(Line::from(Span::styled(
            " The active session will appear after hydration.",
            app.theme.muted.add_modifier(Modifier::DIM),
        )));
    }
    let accent_fg = app.theme.accent.fg.unwrap_or(Color::Reset);
    let muted_fg = app.theme.muted.fg.unwrap_or(Color::Reset);
    for s in &app.sessions {
        let status_style = match s.status {
            SessionStatus::Live => app.theme.success,
            SessionStatus::Saved => app.theme.accent,
        };
        let bar = if s.is_active { "▌" } else { " " };
        let bar_color = if s.is_active { accent_fg } else { muted_fg };
        let row_style = if s.is_active {
            app.theme.assistant.add_modifier(Modifier::BOLD)
        } else {
            app.theme.assistant
        };
        lines.push(Line::from(vec![
            Span::styled(
                bar,
                Style::default().fg(bar_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" █ ", status_style),
            Span::styled(s.label.clone(), row_style.add_modifier(Modifier::BOLD)),
        ]));
        lines.push(Line::from(vec![
            Span::styled(bar, Style::default().fg(bar_color)),
            Span::styled(
                format!(
                    " {:<6}    {:>4}m",
                    s.status.label().to_uppercase(),
                    s.message_count
                ),
                app.theme.muted,
            ),
        ]));
        if let Some(lineage) = &s.lineage_label {
            lines.push(Line::from(vec![
                Span::styled(bar, Style::default().fg(bar_color)),
                Span::styled(
                    format!(" {}", lineage),
                    app.theme.muted.add_modifier(Modifier::DIM),
                ),
            ]));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " + NEW SESSION",
        app.theme.assistant.add_modifier(Modifier::BOLD),
    )));
    f.render_widget(Paragraph::new(lines).style(faint_bg()), area);
}

// ─── 03 TILED MESH ──────────────────────────────────────────────────────

fn tiled_mesh(f: &mut TuiFrame, area: Rect, app: &AppState) {
    let status = format!(
        "1 orchestrator · {} delegated",
        app.delegated_worker_count()
    );
    let outer = frame(f, area, app.view.title(), Some(&status), None, &app.theme);
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),
            Constraint::Length(super::widgets::prompt_row_height(
                &app.input,
                area.width,
                app.mode_label(),
            )),
        ])
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
            Span::styled("STATE ", app.theme.muted.add_modifier(Modifier::DIM)),
            Span::styled(
                tile.state.to_uppercase(),
                app.theme.assistant.add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::from(""));
        for (n, l) in tile.log.iter().enumerate() {
            lines.push(Line::from(vec![
                Span::styled(format!("{:>02} ", n + 1), app.theme.muted),
                Span::styled(l.clone(), app.theme.assistant),
            ]));
        }
        if i == 0 && app.show_reasoning {
            lines.push(Line::from(""));
            if let Some(reasoning) = app.latest_reasoning() {
                for l in super::widgets::reasoning_lines(reasoning, false, &app.theme, area.width) {
                    lines.push(l);
                }
            } else {
                lines.push(Line::from(Span::styled(
                    "No live reasoning trace yet.",
                    app.theme.muted,
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
        app.prompt_editor.textarea(),
        app.prompt_hints(),
        &app.model_status(),
        app.context_ratio(),
        app.thinking,
        &app.theme,
    );
    app.cursor_set(cx, cy);
}

// ─── 04 TREE OF THOUGHT ─────────────────────────────────────────────────

fn tree_of_thought(f: &mut TuiFrame, area: Rect, app: &AppState) {
    let tree_nodes = app.tree_nodes();
    let summary = format!("{} branch runtime slots", app.branch_count());
    let outer = frame(f, area, app.view.title(), Some(&summary), None, &app.theme);
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),
            Constraint::Length(super::widgets::prompt_row_height(
                &app.input,
                area.width,
                app.mode_label(),
            )),
        ])
        .split(outer);
    let h = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(36), Constraint::Min(40)])
        .split(v[0]);

    // Left: tree
    fill(f, h[0], faint_bg());
    let inner = section_header(f, h[0], "decision tree", None);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let tree_accent_fg = app.theme.accent.fg.unwrap_or(Color::Reset);
    let tree_muted_fg = app.theme.muted.fg.unwrap_or(Color::Reset);
    for n in &tree_nodes {
        let bar = if n.active { "▌" } else { " " };
        let bar_color = if n.active {
            tree_accent_fg
        } else {
            tree_muted_fg
        };
        lines.push(Line::from(vec![
            Span::styled(
                bar,
                Style::default().fg(bar_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {} ", n.state),
                Style::default().fg(n.color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                n.label.clone(),
                app.theme.assistant.add_modifier(if n.depth == 0 {
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
        app.theme.muted,
    )));
    lines.push(Line::from(Span::styled(
        " This pane currently shows the root orchestrator state only.",
        app.theme.muted,
    )));
    f.render_widget(Paragraph::new(lines), inner);

    // Right: focused stream
    let focus_inner = section_header(f, h[1], "focused stream", Some(Palette::YELLOW));
    let mut lines = Vec::new();
    lines.push(message_header(
        "orchestrator",
        tree_accent_fg,
        Some("root"),
        &app.theme,
    ));
    lines.push(Line::from(Span::styled(
        format!("▎ {}", app.orchestration.active_task),
        app.theme.assistant,
    )));
    if app.show_reasoning
        && let Some(reasoning) = app.latest_reasoning()
    {
        lines.push(Line::from(""));
        for l in super::widgets::reasoning_lines(reasoning, false, &app.theme, area.width) {
            lines.push(l);
        }
    }
    if let Some(content) = app.latest_agent_content() {
        lines.push(Line::from(""));
        for line in render_markdown(content, &app.theme) {
            lines.push(Line::from(line.spans));
        }
    } else {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "No focused branch output yet.",
            app.theme.muted,
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
        app.prompt_editor.textarea(),
        app.prompt_hints(),
        &app.model_status(),
        app.context_ratio(),
        app.thinking,
        &app.theme,
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
    let outer = frame(f, area, app.view.title(), Some(&status), None, &app.theme);

    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(8),    // grid
            Constraint::Length(1), // ticker
            Constraint::Length(super::widgets::prompt_row_height(
                &app.input,
                area.width,
                app.mode_label(),
            )), // prompt row (YYC-104: wraps with input)
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
    let chat_width = primary_inner.width.saturating_sub(2);
    let chat_height = primary_inner.height;
    let window = build_chat_window(
        app,
        app.show_reasoning,
        true,
        chat_width,
        chat_height,
        true,
        false,
    );
    publish_chat_max_scroll(app, window.total_lines, chat_height);
    f.render_widget(
        Paragraph::new(window.lines)
            .style(body())
            .wrap(Wrap { trim: false }),
        Rect {
            x: primary_inner.x + 1,
            y: primary_inner.y,
            width: chat_width,
            height: chat_height,
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
                app.theme.assistant.add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" · {}", tile.role), app.theme.muted),
        ]));
        for (n, l) in tile.log.iter().enumerate() {
            let style = if n + 1 == tile.log.len() {
                app.theme.assistant
            } else {
                app.theme.muted
            };
            lines.push(Line::from(Span::styled(format!("  {}", l), style)));
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
            app.theme.muted,
        )));
    }
    let floor_accent_fg = app.theme.accent.fg.unwrap_or(Color::Reset);
    for s in &app.sessions {
        let bar = if s.is_active { "▌" } else { " " };
        let status_style = match s.status {
            SessionStatus::Live => app.theme.success,
            SessionStatus::Saved => app.theme.accent,
        };
        lines.push(Line::from(vec![
            Span::styled(
                bar,
                Style::default()
                    .fg(floor_accent_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" █ ", status_style),
            Span::styled(format!("{:<14}", s.label), app.theme.assistant),
            Span::styled(format!(" {}m", s.message_count), app.theme.muted),
        ]));
    }
    f.render_widget(Paragraph::new(lines), ses_inner);

    // ── tool log (bot-left) — live audit data when available
    let tl_inner = section_header(f, bot[0], "tool log", None);
    let log_rows = app.tool_log_view(tl_inner.height as usize);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let row_count = log_rows.len();
    for (i, row) in log_rows.iter().enumerate() {
        let last = i + 1 == row_count;
        let row_style = if last {
            app.theme.assistant
        } else {
            app.theme.muted
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{:>8} ", row.time), row_style),
            Span::styled(
                format!("{:<5} ", row.actor),
                app.theme.assistant.add_modifier(Modifier::BOLD),
            ),
            Span::styled(row.msg.clone(), row_style),
        ]));
    }
    f.render_widget(Paragraph::new(lines).style(body()), tl_inner);
    draw_v_divider(f, bot[0]);

    // ── diff (bot-mid). YYC-66: prefer the live edit sink; fall back to
    // demo data only if no real edit has happened yet AND the sink isn't
    // wired up (sink wired but empty → render an honest empty state).
    let live_diff = app.latest_diff();
    let diff_title: String = match (&live_diff, app.diff_sink.is_some()) {
        (Some(d), _) => format!("diff · {} · {}", d.path, app.diff_style.label()),
        (None, true) => "diff · no edits this session".into(),
        (None, false) => "diff · users/auth.rs".into(),
    };
    let diff_inner = section_header(f, bot[1], &diff_title, None);
    let mut lines: Vec<Line<'static>> = Vec::new();
    if let Some(d) = live_diff {
        lines.push(Line::from(Span::styled(
            format!("@@ {} · {}", d.tool, d.at.format("%H:%M:%S")),
            Style::default()
                .fg(Palette::MUTED)
                .add_modifier(Modifier::DIM),
        )));
        match app.diff_style {
            DiffStyle::Unified => {
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
            }
            DiffStyle::SideBySide => {
                let before_lines: Vec<&str> = d.before.lines().collect();
                let after_lines: Vec<&str> = d.after.lines().collect();
                let n = before_lines.len().max(after_lines.len());
                let half = (diff_inner.width.saturating_sub(3) / 2) as usize;
                for i in 0..n {
                    let l = before_lines.get(i).copied().unwrap_or("");
                    let r = after_lines.get(i).copied().unwrap_or("");
                    let l_pad = format!("{l:<half$}", half = half)
                        .chars()
                        .take(half)
                        .collect::<String>();
                    let r_pad = format!("{r:<half$}", half = half)
                        .chars()
                        .take(half)
                        .collect::<String>();
                    lines.push(Line::from(vec![
                        Span::styled(l_pad, Style::default().fg(Palette::RED)),
                        Span::styled(" │ ", Style::default().fg(Palette::MUTED)),
                        Span::styled(r_pad, Style::default().fg(Palette::GREEN)),
                    ]));
                }
            }
            DiffStyle::Inline => {
                // Render the new line(s) with the old text highlighted
                // in red strikethrough at the start, then the new text.
                for line in d.before.lines() {
                    lines.push(Line::from(vec![Span::styled(
                        line.to_string(),
                        Style::default()
                            .fg(Palette::RED)
                            .add_modifier(Modifier::CROSSED_OUT),
                    )]));
                }
                for line in d.after.lines() {
                    lines.push(Line::from(Span::styled(
                        line.to_string(),
                        Style::default()
                            .fg(Palette::GREEN)
                            .add_modifier(Modifier::BOLD),
                    )));
                }
            }
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
                DiffKind::Ctx => Style::default(),
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
            app.theme.body_fg,
        ),
        (
            "output",
            super::state::format_thousands(app.completion_tokens_total),
            app.theme.body_fg,
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
            Span::raw(middle),
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
            Paragraph::new(div).style(app.theme.border),
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
        app.prompt_editor.textarea(),
        app.prompt_hints(),
        &app.model_status(),
        app.context_ratio(),
        app.thinking,
        &app.theme,
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
        Line::from(Span::raw(msg.to_string())),
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
        .map(|_| Line::from(Span::raw("│")))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::state::ChatRole;
    use ratatui::{
        Terminal,
        backend::{Backend, TestBackend},
    };

    #[test]
    fn build_chat_window_uses_visible_height() {
        let mut app = AppState::new("test-model".into(), 128_000);
        for i in 0..100 {
            app.messages.push(crate::tui::state::ChatMessage::new(
                ChatRole::User,
                format!("message {i}"),
            ));
        }

        let window = build_chat_window(&app, true, false, 80, 5, true, false);

        assert_eq!(window.lines.len(), 5);
        assert!(window.total_lines > 5);
    }

    #[test]
    fn single_stack_renders_focus_header_and_copy_friendly_body() {
        let backend = TestBackend::new(140, 22);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let mut app = AppState::new("test-model".into(), 128_000);
        app.view = View::SingleStack;
        app.session_label = "main".into();
        app.messages.push(crate::tui::state::ChatMessage::new(
            ChatRole::User,
            "copy this line",
        ));

        terminal
            .draw(|f| render_view(f, f.area(), &app))
            .expect("draw single stack");

        let body = terminal_buffer_text(terminal.backend());
        assert!(body.contains("VULCAN · SINGLE STACK"));
        assert!(body.contains("Command AI like a tool"));
        assert!(body.contains("[SESSION] main"));
        assert!(body.contains("[INSERT]"));
        assert!(body.contains("test-model · 0 / 128,000"));
        assert!(body.contains("VULCAN · local agent workbench"));
        assert!(!body.contains("── session"));
        assert!(!body.contains("▎ copy this line"));
        assert_eq!(app.messages.len(), 1);
    }

    #[test]
    fn chat_welcome_is_render_only_prefix() {
        let app = AppState::new("test-model".into(), 128_000);
        let window = build_chat_window(&app, true, false, 120, 12, false, true);
        let body = window
            .lines
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(body.contains("VULCAN · local agent workbench"));
        assert!(body.contains("Command AI like a tool"));
        assert!(app.messages.is_empty());
    }

    #[test]
    fn figlet_welcome_uses_natural_line_count() {
        let app = AppState::new("test-model".into(), 128_000);
        let expected = FIGlet::standard()
            .expect("standard figlet font")
            .convert("VULCAN")
            .expect("figlet conversion")
            .as_str()
            .lines()
            .count();

        let banner = figlet_welcome_banner("VULCAN", 120, &app.theme);

        assert_eq!(banner.len(), expected);
    }

    fn terminal_buffer_text(backend: &TestBackend) -> String {
        let area = backend.size().expect("backend size");
        let buffer = backend.buffer();
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn line_text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>()
    }
}
