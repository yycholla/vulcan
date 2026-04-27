//! Overlay rendering helpers extracted from `tui/mod.rs` (YYC-108).
//! Every `draw_*` overlay (slash palette, session picker, model
//! miller-columns picker, provider picker, diff scrubber) lives here,
//! along with the small primitives (`render_picker_column`,
//! `build_picker_details`, `trim_to_width`, `picker_current_leaf`,
//! `draw_picker_border`) the overlays share.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};

use super::commands::SlashCommand;
use super::state::{AppState, SessionStatus};
use super::theme::{Theme, body};

pub(super) fn draw_palette(
    f: &mut ratatui::Frame,
    area: Rect,
    cmds: &[&SlashCommand],
    selected: usize,
    theme: &Theme,
) {
    if area.height == 0 {
        return;
    }
    // Title bar
    let bar = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: 1,
    };
    let mut header_text = " ▓▓ COMMANDS".to_string();
    if (header_text.chars().count() as u16) < bar.width {
        header_text.push_str(&" ".repeat(bar.width as usize - header_text.chars().count()));
    }
    f.render_widget(
        Paragraph::new(header_text).style(theme.accent.add_modifier(Modifier::BOLD)),
        bar,
    );
    let inner = Rect {
        x: area.x,
        y: area.y + 1,
        width: area.width,
        height: area.height.saturating_sub(1),
    };
    let mut lines = Vec::new();
    if cmds.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No matching commands",
            theme.muted,
        )));
    } else {
        let active = selected.min(cmds.len().saturating_sub(1));
        for (i, cmd) in cmds.iter().enumerate() {
            let is_active = i == active;
            // YYC-70: highlight the active row by swapping fg/bg of accent
            // (gives a visible selection bar regardless of active theme).
            let (prefix, name_style, desc_style) = if is_active {
                let active_style = theme.accent.add_modifier(Modifier::BOLD);
                ("▸ ", active_style, active_style)
            } else {
                (
                    "  ",
                    theme.accent.add_modifier(Modifier::BOLD),
                    theme.assistant,
                )
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{prefix}/{:<12}", cmd.name), name_style),
                Span::styled(cmd.description, desc_style),
            ]));
        }
    }
    f.render_widget(Paragraph::new(lines).style(body()), inner);
}

pub(super) fn draw_session_picker(f: &mut ratatui::Frame, area: Rect, app: &AppState) {
    let theme = &app.theme;
    let width = area.width.min(56);
    let height = (app.sessions.len() as u16 + 6).min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;

    let box_area = Rect {
        x,
        y,
        width,
        height,
    };
    if box_area.height < 4 {
        return;
    }

    // Title bar
    let bar = Rect {
        x: box_area.x,
        y: box_area.y,
        width: box_area.width,
        height: 1,
    };
    let mut title = "  Resume a Session  ".to_string();
    if (title.chars().count() as u16) < bar.width {
        let pad = bar.width as usize - title.chars().count();
        title = format!(
            "{}{}{}",
            " ".repeat(pad / 2),
            title.trim(),
            " ".repeat(pad - pad / 2)
        );
    }
    f.render_widget(
        Paragraph::new(title).style(theme.accent.add_modifier(Modifier::BOLD)),
        bar,
    );

    // Session list
    let list_area = Rect {
        x: box_area.x,
        y: box_area.y + 1,
        width: box_area.width,
        height: box_area.height.saturating_sub(2),
    };
    let mut lines: Vec<Line<'static>> = Vec::new();

    if app.sessions.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No saved sessions found.",
            theme.muted,
        )));
        lines.push(Line::from(Span::styled(
            "  Start a conversation and sessions will appear here.",
            theme.muted,
        )));
    } else {
        let active = app
            .session_picker_selection
            .min(app.sessions.len().saturating_sub(1));
        for (i, s) in app.sessions.iter().enumerate() {
            let is_active = i == active;
            let marker = if is_active { "▸ " } else { "  " };
            let status_style = match s.status {
                SessionStatus::Live => theme.success,
                SessionStatus::Saved => theme.system,
            };
            let status_label = match s.status {
                SessionStatus::Live => "LIVE",
                SessionStatus::Saved => "saved",
            };

            let dt = chrono::DateTime::from_timestamp(s.last_active, 0)
                .map(|d| {
                    d.with_timezone(&chrono::Local)
                        .format("%b %d %H:%M")
                        .to_string()
                })
                .unwrap_or_default();

            let name_style = Style::default()
                .fg(theme.body_fg)
                .add_modifier(if is_active {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                });

            lines.push(Line::from(vec![
                Span::styled(marker, name_style.add_modifier(Modifier::BOLD)),
                Span::styled("█ ", status_style),
                Span::styled(format!("{:<12}", super::short_id(&s.id)), name_style),
                Span::styled(format!("{:>4}m", s.message_count), theme.muted),
                Span::styled(format!("  {} ", dt), theme.muted),
                Span::styled(status_label, status_style.add_modifier(Modifier::BOLD)),
            ]));

            if let Some(preview) = &s.preview {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        format!("  {}", preview),
                        theme.muted.add_modifier(Modifier::DIM),
                    ),
                ]));
            }
        }
    }

    // Footer hint
    let hint = "  ↑↓ navigate · Enter select · Esc cancel  ";
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(hint, theme.muted)));

    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme.body_bg)),
        list_area,
    );

    // Draw a border around the whole thing
    draw_picker_border(f, box_area, theme);
}

pub(super) fn draw_model_picker(f: &mut ratatui::Frame, area: Rect, app: &AppState) {
    let theme = &app.theme;
    let width = area.width.min(72);
    let rows = (app.model_picker_items.len() as u16).min(20);
    let height = (rows + 5).min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let box_area = Rect {
        x,
        y,
        width,
        height,
    };
    if box_area.height < 4 {
        return;
    }

    let bar = Rect {
        x: box_area.x,
        y: box_area.y,
        width: box_area.width,
        height: 1,
    };
    let mut title = "  Switch Model  ".to_string();
    if (title.chars().count() as u16) < bar.width {
        let pad = bar.width as usize - title.chars().count();
        title = format!(
            "{}{}{}",
            " ".repeat(pad / 2),
            title.trim(),
            " ".repeat(pad - pad / 2)
        );
    }
    f.render_widget(
        Paragraph::new(title).style(theme.accent.add_modifier(Modifier::BOLD)),
        bar,
    );

    let list_area = Rect {
        x: box_area.x,
        y: box_area.y + 1,
        width: box_area.width,
        height: box_area.height.saturating_sub(2),
    };

    if app.model_picker_items.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  No models in provider catalog.",
                theme.muted,
            ))),
            list_area,
        );
        draw_picker_border(f, box_area, theme);
        return;
    }

    // Mini-files-style miller columns. The number of visible columns is
    // capped to whatever the box width can fit (each column is roughly
    // 20–28 chars). Always reserve the rightmost column for details.
    let drilled_depth = app.model_picker_path.len();
    let max_tree_depth = app.model_picker_tree.max_depth();
    let total_columns = max_tree_depth.max(1) + 1; // +1 for details

    let col_width = (list_area.width / total_columns as u16).max(16);
    let cols: Vec<Rect> = (0..total_columns)
        .map(|i| Rect {
            x: list_area.x + i as u16 * col_width,
            y: list_area.y,
            width: col_width,
            height: list_area.height,
        })
        .collect();

    // Render each tree column.
    for (col_idx, col_rect) in cols.iter().enumerate().take(max_tree_depth) {
        let path_prefix: Vec<usize> = app
            .model_picker_path
            .iter()
            .copied()
            .take(col_idx)
            .collect();
        let nodes = app.model_picker_tree.column_at(col_idx, &path_prefix);
        let selection = app
            .model_picker_path
            .get(col_idx)
            .copied()
            .unwrap_or(0)
            .min(nodes.len().saturating_sub(1));
        let is_focused = col_idx == app.model_picker_focus;
        render_picker_column(f, *col_rect, nodes, selection, is_focused, theme);
    }

    // Details panel at the rightmost column.
    let details_col = cols.last().copied().unwrap_or(list_area);
    let detail_lines = build_picker_details(app);
    f.render_widget(
        Paragraph::new(detail_lines).wrap(Wrap { trim: false }),
        details_col,
    );

    let hint = "  hjkl move · Enter select · Esc cancel  (drilled: column ";
    let footer_line = format!(
        "{hint}{}/{})",
        app.model_picker_focus + 1,
        max_tree_depth.max(1)
    );
    let footer_rect = Rect {
        x: list_area.x,
        y: list_area.y + list_area.height.saturating_sub(1),
        width: list_area.width,
        height: 1,
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(footer_line, theme.muted))),
        footer_rect,
    );
    let _ = drilled_depth; // reserved for future filter UI

    draw_picker_border(f, box_area, theme);
}

pub(super) fn render_picker_column(
    f: &mut ratatui::Frame,
    area: Rect,
    nodes: &[crate::tui::model_picker::TreeNode],
    selection: usize,
    is_focused: bool,
    theme: &Theme,
) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    if nodes.is_empty() {
        lines.push(Line::from(Span::styled("  ·", theme.muted)));
    } else {
        let visible = area.height.saturating_sub(2) as usize;
        let start = selection.saturating_sub(visible.saturating_sub(1) / 2);
        let end = (start + visible).min(nodes.len());
        for (i, node) in nodes.iter().enumerate().take(end).skip(start) {
            let is_active = i == selection;
            let marker = if is_active && is_focused {
                "▸ "
            } else if is_active {
                "│ "
            } else {
                "  "
            };
            let mut style = Style::default();
            if is_active {
                style = style.add_modifier(Modifier::BOLD);
                if is_focused {
                    style = if let Some(fg) = theme.accent.fg {
                        style.fg(fg)
                    } else {
                        style.add_modifier(Modifier::REVERSED)
                    };
                }
            }
            let suffix = if node.children.is_empty() && node.model_index.is_some() {
                ""
            } else {
                "›"
            };
            let label = trim_to_width(&node.label, area.width.saturating_sub(4) as usize);
            lines.push(Line::from(vec![
                Span::styled(marker, style),
                Span::styled(label, style),
                Span::styled(format!(" {suffix}"), theme.muted),
            ]));
        }
        if start > 0 || end < nodes.len() {
            lines.push(Line::from(Span::styled(
                format!("  …{}/{}", end, nodes.len()),
                theme.muted.add_modifier(Modifier::DIM),
            )));
        }
    }
    f.render_widget(Paragraph::new(lines), area);
}

pub(super) fn build_picker_details(app: &AppState) -> Vec<Line<'static>> {
    let theme = &app.theme;
    let mut lines = Vec::new();
    let leaf_idx = picker_current_leaf(&app.model_picker_tree, &app.model_picker_path);
    let Some(idx) = leaf_idx else {
        lines.push(Line::from(Span::styled(
            "  drill in (l/→) for details",
            theme.muted,
        )));
        return lines;
    };
    let Some(model) = app.model_picker_items.get(idx) else {
        return lines;
    };
    lines.push(Line::from(Span::styled(
        format!(" {}", model.id),
        Style::default().add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));
    let ctx = if model.context_length > 0 {
        crate::tui::state::format_thousands(model.context_length as u32)
    } else {
        "?".into()
    };
    lines.push(Line::from(Span::styled(
        format!(" context  : {ctx}"),
        theme.muted,
    )));
    let mut flags = Vec::new();
    if model.features.tools {
        flags.push("tools");
    }
    if model.features.reasoning {
        flags.push("reasoning");
    }
    if model.features.vision {
        flags.push("vision");
    }
    if model.features.json_mode {
        flags.push("json");
    }
    let flag_str = if flags.is_empty() {
        "(none reported)".to_string()
    } else {
        flags.join(", ")
    };
    lines.push(Line::from(Span::styled(
        format!(" features : {flag_str}"),
        theme.muted,
    )));
    if let Some(p) = &model.pricing {
        lines.push(Line::from(Span::styled(
            format!(
                " pricing  : ${:.4}/1k in · ${:.4}/1k out",
                p.input_per_token * 1000.0,
                p.output_per_token * 1000.0,
            ),
            theme.muted,
        )));
    }
    if let Some(top) = &model.top_provider {
        lines.push(Line::from(Span::styled(
            format!(" upstream : {top}"),
            theme.muted,
        )));
    }
    lines
}

pub(super) fn trim_to_width(s: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= width {
        return s.to_string();
    }
    let head: String = chars.iter().take(width.saturating_sub(1)).collect();
    format!("{head}…")
}

pub(super) fn picker_current_leaf(
    tree: &crate::tui::model_picker::ModelTree,
    path: &[usize],
) -> Option<usize> {
    let mut current: &[crate::tui::model_picker::TreeNode] = &tree.labs;
    let mut leaf: Option<usize> = None;
    for &idx in path {
        let node = current.get(idx)?;
        if node.children.is_empty() {
            return node.model_index;
        }
        leaf = node.model_index;
        current = &node.children;
    }
    // Path didn't reach a leaf — return last seen leaf marker (None for
    // internal-only nodes).
    leaf
}

pub(super) fn draw_provider_picker(f: &mut ratatui::Frame, area: Rect, app: &AppState) {
    let theme = &app.theme;
    let width = area.width.min(72);
    let rows = (app.provider_picker_items.len() as u16).min(12);
    let height = (rows + 5).min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let box_area = Rect {
        x,
        y,
        width,
        height,
    };
    if box_area.height < 4 {
        return;
    }

    let bar = Rect {
        x: box_area.x,
        y: box_area.y,
        width: box_area.width,
        height: 1,
    };
    let mut title = "  Switch Provider  ".to_string();
    if (title.chars().count() as u16) < bar.width {
        let pad = bar.width as usize - title.chars().count();
        title = format!(
            "{}{}{}",
            " ".repeat(pad / 2),
            title.trim(),
            " ".repeat(pad - pad / 2)
        );
    }
    f.render_widget(
        Paragraph::new(title).style(theme.accent.add_modifier(Modifier::BOLD)),
        bar,
    );

    let list_area = Rect {
        x: box_area.x,
        y: box_area.y + 1,
        width: box_area.width,
        height: box_area.height.saturating_sub(2),
    };

    let mut lines: Vec<Line<'static>> = Vec::new();
    if app.provider_picker_items.is_empty() {
        lines.push(Line::from(Span::styled("  (no providers)", theme.muted)));
    } else {
        let active = app
            .provider_picker_selection
            .min(app.provider_picker_items.len().saturating_sub(1));
        for (i, e) in app.provider_picker_items.iter().enumerate() {
            let is_active = i == active;
            let marker = if is_active { "▸ " } else { "  " };
            let label = e.name.clone().unwrap_or_else(|| "default".into());
            let row_style = Style::default()
                .fg(theme.body_fg)
                .add_modifier(if is_active {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                });
            lines.push(Line::from(vec![
                Span::styled(marker, row_style.add_modifier(Modifier::BOLD)),
                Span::styled(format!("{label:<12}"), row_style),
                Span::styled(format!(" {}", e.model), theme.muted),
                Span::styled(format!("  ({})", e.base_url), theme.muted),
            ]));
        }
    }

    let hint = "  ↑↓ navigate · Enter select · Esc cancel  ";
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(hint, theme.muted)));

    f.render_widget(Paragraph::new(lines), list_area);
    draw_picker_border(f, box_area, theme);
}

pub(super) fn draw_diff_scrubber(f: &mut ratatui::Frame, area: Rect, app: &AppState) {
    let theme = &app.theme;
    let width = area.width.min(96);
    let total = app.scrubber_hunks.len() as u16;
    let height = (total * 4 + 8).min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let box_area = Rect {
        x,
        y,
        width,
        height,
    };
    if box_area.height < 6 {
        return;
    }

    let bar = Rect {
        x: box_area.x,
        y: box_area.y,
        width: box_area.width,
        height: 1,
    };
    let title = format!(
        "  Edit Scrubber — {} ({} hunks)  ",
        app.scrubber_path, total
    );
    f.render_widget(
        Paragraph::new(title).style(theme.accent.add_modifier(Modifier::BOLD)),
        bar,
    );

    let list_area = Rect {
        x: box_area.x,
        y: box_area.y + 1,
        width: box_area.width,
        height: box_area.height.saturating_sub(2),
    };

    let mut lines: Vec<Line<'static>> = Vec::new();
    let active = app
        .scrubber_selection
        .min(app.scrubber_hunks.len().saturating_sub(1));
    for (i, hunk) in app.scrubber_hunks.iter().enumerate() {
        let is_active = i == active;
        let accepted = app.scrubber_accepted.get(i).copied().unwrap_or(true);
        let marker = if is_active { "▸ " } else { "  " };
        let state = if accepted { "[✓]" } else { "[ ]" };
        let header = format!(
            "{marker}{state} hunk {} of {} · line {}",
            i + 1,
            total,
            hunk.line_no
        );
        let header_style = Style::default()
            .fg(theme.body_fg)
            .add_modifier(if is_active {
                Modifier::BOLD
            } else {
                Modifier::empty()
            });
        lines.push(Line::from(Span::styled(header, header_style)));
        for before in &hunk.before_lines {
            lines.push(Line::from(vec![
                Span::styled(
                    "    - ",
                    Style::default().fg(crate::tui::theme::Palette::RED),
                ),
                Span::styled(
                    before.clone(),
                    Style::default().fg(crate::tui::theme::Palette::RED),
                ),
            ]));
        }
        for after in &hunk.after_lines {
            lines.push(Line::from(vec![
                Span::styled(
                    "    + ",
                    Style::default().fg(crate::tui::theme::Palette::GREEN),
                ),
                Span::styled(
                    after.clone(),
                    Style::default().fg(crate::tui::theme::Palette::GREEN),
                ),
            ]));
        }
        lines.push(Line::from(""));
    }

    let hint = "  ↑↓ navigate · y/n toggle · Y all · N none · Enter apply · Esc cancel  ";
    lines.push(Line::from(Span::styled(hint, theme.muted)));

    f.render_widget(Paragraph::new(lines), list_area);
    draw_picker_border(f, box_area, theme);
}

/// Simple border drawn with box-drawing characters.
pub(super) fn draw_picker_border(f: &mut ratatui::Frame, r: Rect, theme: &Theme) {
    let style = theme.border;
    // Top
    if r.height > 0 {
        let top = "─".repeat(r.width as usize);
        f.render_widget(
            Paragraph::new(top).style(style),
            Rect {
                x: r.x,
                y: r.y,
                width: r.width,
                height: 1,
            },
        );
    }
    // Bottom
    if r.height > 1 {
        let bot = "─".repeat(r.width as usize);
        f.render_widget(
            Paragraph::new(bot).style(style),
            Rect {
                x: r.x,
                y: r.y + r.height - 1,
                width: r.width,
                height: 1,
            },
        );
    }
    // Left edge (corners overlap — good enough for a 1px line)
    if r.height > 2 {
        let left: Vec<Line<'static>> = (1..r.height - 1)
            .map(|_| Line::from(Span::styled("│", style)))
            .collect();
        f.render_widget(
            Paragraph::new(left),
            Rect {
                x: r.x,
                y: r.y + 1,
                width: 1,
                height: r.height - 2,
            },
        );
        let right: Vec<Line<'static>> = (1..r.height - 1)
            .map(|_| Line::from(Span::styled("│", style)))
            .collect();
        f.render_widget(
            Paragraph::new(right),
            Rect {
                x: r.x + r.width - 1,
                y: r.y + 1,
                width: 1,
                height: r.height - 2,
            },
        );
    }
}
