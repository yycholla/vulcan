//! Universal mini.files-style miller-columns widget (YYC-102).
//!
//! Anchored top-left, drilled by hjkl. Each column is its own bordered
//! block with a header equal to the parent path segment. The rightmost
//! column is a preview pane: leaf → rendered detail; branch → child
//! listing.
//!
//! `MillerSource` is the adapter trait; the model picker, session
//! picker, command palette, and `/skills` browser can all back the
//! same widget by implementing it.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
};

use crate::tui::theme::{Palette, Theme};

/// One row in a miller column.
#[derive(Clone, Debug)]
pub struct MillerEntry {
    pub label: String,
    /// Single-char icon (e.g. ▸, ▪, 󰈤, 📁). Empty string = no icon.
    pub icon: String,
    /// Branch entry → arrow rendered, drill-down works.
    pub has_children: bool,
}

/// Rendered preview content for the rightmost column.
#[derive(Clone, Debug, Default)]
pub struct MillerPreview {
    pub title: String,
    pub lines: Vec<Line<'static>>,
}

/// Adapter: the consumer of the widget implements this to expose
/// hierarchical content.
pub trait MillerSource {
    /// Title for the column at `path.len()` depth — typically the
    /// parent's label, or a root anchor like `~/vulcan`.
    fn header(&self, path: &[usize]) -> String;
    /// Rows for the column at `path.len()` depth.
    fn entries(&self, path: &[usize]) -> Vec<MillerEntry>;
    /// Preview pane content for the currently-selected row at `path`.
    fn preview(&self, path: &[usize]) -> Option<MillerPreview>;
}

/// Cursor state. Owned by the consumer so the widget itself stays
/// stateless across redraws.
#[derive(Clone, Debug, Default)]
pub struct MillerState {
    /// Selection index per column. Length == focused column + 1.
    pub path: Vec<usize>,
    /// Which column has keyboard focus (0..= path.len()-1).
    pub focus: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MillerAction {
    /// No state change, redraw and wait for the next key.
    Continue,
    /// User pressed Esc / q — caller should close the overlay.
    Cancel,
    /// User pressed Enter on a row that should commit.
    Commit,
}

impl MillerState {
    pub fn new() -> Self {
        Self {
            path: vec![0],
            focus: 0,
        }
    }
    pub fn current_selection(&self) -> usize {
        self.path.get(self.focus).copied().unwrap_or(0)
    }
}

/// Move the focused column's cursor by `delta`. Clamps to entries.
pub fn move_cursor<S: MillerSource>(state: &mut MillerState, source: &S, delta: i32) {
    let prefix: Vec<usize> = state.path.iter().take(state.focus).copied().collect();
    let entries = source.entries(&prefix);
    if entries.is_empty() {
        return;
    }
    while state.path.len() <= state.focus {
        state.path.push(0);
    }
    let cur = state.path[state.focus] as i32 + delta;
    let max = (entries.len() - 1) as i32;
    state.path[state.focus] = cur.clamp(0, max) as usize;
    // Drilled columns past the focused one are stale — drop them.
    state.path.truncate(state.focus + 1);
}

/// Drill into the focused selection's children. Returns true when the
/// drill happened; false when the row is a leaf (caller should commit).
pub fn drill<S: MillerSource>(state: &mut MillerState, source: &S) -> bool {
    let prefix: Vec<usize> = state.path.iter().take(state.focus).copied().collect();
    let entries = source.entries(&prefix);
    let sel = state.path.get(state.focus).copied().unwrap_or(0);
    let Some(entry) = entries.get(sel) else {
        return false;
    };
    if !entry.has_children {
        return false;
    }
    while state.path.len() <= state.focus + 1 {
        state.path.push(0);
    }
    state.path[state.focus + 1] = 0;
    state.focus += 1;
    true
}

/// Move focus to the parent column (collapse rightmost). Returns true
/// when the collapse happened; false when already at column 0 (caller
/// should close the overlay).
pub fn ascend(state: &mut MillerState) -> bool {
    if state.focus == 0 {
        return false;
    }
    state.focus -= 1;
    state.path.truncate(state.focus + 1);
    true
}

/// Width budget per column kind (mini.files defaults adapted to TUI).
const WIDTH_FOCUS: u16 = 36;
const WIDTH_NOFOCUS: u16 = 18;
const WIDTH_PREVIEW: u16 = 40;

/// Render the picker top-left-anchored inside `area`. Each column is a
/// titled, bordered block; the focused column is wider; the rightmost
/// column is a preview pane only when there's something to preview.
pub fn render<S: MillerSource>(
    f: &mut Frame,
    area: Rect,
    state: &MillerState,
    source: &S,
    theme: &Theme,
) -> Option<Rect> {
    if area.width < 24 || area.height < 6 {
        return None;
    }

    let drill_columns = state.focus + 1;
    let preview = source.preview(&state.path);
    let show_preview = preview.is_some();

    // Compute how many columns fit. Mini.files prioritizes focus + preview,
    // then adds non-focused columns while space remains.
    let mut budget = area.width;
    let preview_w = if show_preview {
        WIDTH_PREVIEW.min(
            budget
                .saturating_sub(WIDTH_FOCUS)
                .max(WIDTH_PREVIEW.min(20)),
        )
    } else {
        0
    };
    if show_preview {
        budget = budget.saturating_sub(preview_w);
    }
    budget = budget.saturating_sub(WIDTH_FOCUS);
    let mut max_visible_cols = 1; // focused
    while budget >= WIDTH_NOFOCUS && max_visible_cols < drill_columns {
        budget = budget.saturating_sub(WIDTH_NOFOCUS);
        max_visible_cols += 1;
    }

    // Center the focused column in the visible window: `to` is the last
    // column drawn, `from` the first.
    let to = drill_columns;
    let from = to.saturating_sub(max_visible_cols);

    let mut x_cursor = area.x;
    let max_height = area.height.saturating_sub(1); // keep last row for footer
    let mut occupied: Option<Rect> = None;

    for col_idx in from..drill_columns {
        let prefix: Vec<usize> = state.path.iter().take(col_idx).copied().collect();
        let entries = source.entries(&prefix);
        let selection = state.path.get(col_idx).copied().unwrap_or(0);
        let header = source.header(&prefix);
        let is_focused = col_idx == state.focus;
        let width = if is_focused {
            WIDTH_FOCUS
        } else {
            WIDTH_NOFOCUS
        };

        let rect = Rect {
            x: x_cursor,
            y: area.y,
            width,
            height: fitted_column_height(entries.len(), max_height),
        };
        draw_column(f, rect, &header, &entries, selection, is_focused, theme);
        occupied = union_rect(occupied, rect);
        x_cursor = x_cursor.saturating_add(width);
    }

    if show_preview && preview_w > 0 {
        let preview_height = preview
            .as_ref()
            .map(|p| fitted_preview_height(p.lines.len(), max_height))
            .unwrap_or_else(|| fitted_preview_height(0, max_height));
        let preview_rect = Rect {
            x: x_cursor,
            y: area.y,
            width: preview_w,
            height: preview_height,
        };
        draw_preview(f, preview_rect, preview.as_ref(), theme);
        occupied = union_rect(occupied, preview_rect);
    }
    occupied
}

fn draw_column(
    f: &mut Frame,
    rect: Rect,
    header: &str,
    entries: &[MillerEntry],
    selection: usize,
    is_focused: bool,
    theme: &Theme,
) {
    if rect.width < 4 || rect.height < 3 {
        return;
    }
    fill_rect(f, rect, opaque_surface_bg(theme));
    let border_style = if is_focused {
        theme.accent.add_modifier(Modifier::BOLD)
    } else {
        theme.border
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(border_style)
        .title(format!(" {} ", header));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    if entries.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled("  (empty)", theme.muted))),
            inner,
        );
        return;
    }

    let visible = inner.height as usize;
    let active = selection.min(entries.len().saturating_sub(1));
    let start = active.saturating_sub(visible.saturating_sub(1) / 2);
    let end = (start + visible).min(entries.len());

    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, entry) in entries.iter().enumerate().take(end).skip(start) {
        let is_active = i == active;
        let icon = if entry.icon.is_empty() {
            String::new()
        } else {
            format!("{} ", entry.icon)
        };
        let arrow = if entry.has_children { " ›" } else { "" };
        let reserved = icon.chars().count() + arrow.chars().count();
        let label = trim_to_width(
            &entry.label,
            (inner.width as usize).saturating_sub(reserved),
        );
        let base_style = if is_active {
            // mini.files-style cursor row: full-width REVERSED block.
            if is_focused {
                Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
            } else {
                Style::default().add_modifier(Modifier::BOLD)
            }
        } else {
            Style::default()
        };
        // Pad to column width so the REVERSED row fills the visible
        // line — matches the mini.files cursor block.
        let row_width = icon.chars().count() + label.chars().count() + arrow.chars().count();
        let pad = (inner.width as usize).saturating_sub(row_width);
        let icon_style = merge_active(icon_style(&entry.icon, theme), base_style);
        let label_style = merge_active(Style::default(), base_style);
        let mut spans = Vec::new();
        if !icon.is_empty() {
            spans.push(Span::styled(icon, icon_style));
        }
        spans.push(Span::styled(label, label_style));
        if !arrow.is_empty() {
            spans.push(Span::styled(
                arrow.to_string(),
                theme.muted.patch(base_style),
            ));
        }
        if pad > 0 {
            spans.push(Span::styled(" ".repeat(pad), label_style));
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_preview(f: &mut Frame, rect: Rect, preview: Option<&MillerPreview>, theme: &Theme) {
    if rect.width < 6 || rect.height < 3 {
        return;
    }
    fill_rect(f, rect, opaque_surface_bg(theme));
    let title = preview
        .map(|p| p.title.clone())
        .unwrap_or_else(|| "preview".to_string());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(theme.border)
        .title(format!(" {title} "));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let body = match preview {
        Some(p) if !p.lines.is_empty() => {
            Paragraph::new(p.lines.clone()).wrap(Wrap { trim: false })
        }
        Some(_) => Paragraph::new(Line::from(Span::styled("  (empty)", theme.muted))),
        None => Paragraph::new(Line::from(Span::styled(
            "  drill in to preview",
            theme.muted,
        ))),
    };
    f.render_widget(body, inner);
}

fn fitted_column_height(entry_count: usize, max_height: u16) -> u16 {
    let desired = (entry_count as u16).saturating_add(2).max(3);
    desired.min(max_height.max(3))
}

fn fitted_preview_height(line_count: usize, max_height: u16) -> u16 {
    let desired = (line_count as u16).saturating_add(2).max(5);
    desired.min(max_height.max(3))
}

fn fill_rect(f: &mut Frame, rect: Rect, bg: Color) {
    let style = Style::default().bg(bg);
    let area = f.area();
    for y in rect.y..rect.bottom().min(area.bottom()) {
        for x in rect.x..rect.right().min(area.right()) {
            let cell = &mut f.buffer_mut()[(x, y)];
            cell.set_symbol(" ");
            cell.set_style(style);
        }
    }
}

fn opaque_surface_bg(theme: &Theme) -> Color {
    match theme.body_fg {
        Color::Rgb(r, g, b) if luma(r, g, b) < 128 => Palette::PAPER,
        Color::Rgb(_, _, _) => Color::Rgb(0x0d, 0x18, 0x25),
        _ => Color::Rgb(0x0d, 0x18, 0x25),
    }
}

fn luma(r: u8, g: u8, b: u8) -> u16 {
    ((r as u16 * 299) + (g as u16 * 587) + (b as u16 * 114)) / 1000
}

fn icon_style(icon: &str, theme: &Theme) -> Style {
    match icon {
        "✦" => theme.success.add_modifier(Modifier::BOLD),
        "◈" => theme.system.add_modifier(Modifier::BOLD),
        "◆" => theme.tool_call.add_modifier(Modifier::BOLD),
        _ => theme.muted,
    }
}

fn merge_active(base: Style, active: Style) -> Style {
    base.patch(active)
}

fn union_rect(current: Option<Rect>, next: Rect) -> Option<Rect> {
    Some(match current {
        None => next,
        Some(rect) => {
            let x = rect.x.min(next.x);
            let y = rect.y.min(next.y);
            let right = rect.right().max(next.right());
            let bottom = rect.bottom().max(next.bottom());
            Rect::new(x, y, right.saturating_sub(x), bottom.saturating_sub(y))
        }
    })
}

fn trim_to_width(s: &str, width: usize) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeSource;
    impl MillerSource for FakeSource {
        fn header(&self, path: &[usize]) -> String {
            format!("col@{}", path.len())
        }
        fn entries(&self, path: &[usize]) -> Vec<MillerEntry> {
            // Three branches at root, two leaves at depth 1, none deeper.
            match path.len() {
                0 => vec![
                    MillerEntry {
                        label: "alpha".into(),
                        icon: String::new(),
                        has_children: true,
                    },
                    MillerEntry {
                        label: "beta".into(),
                        icon: String::new(),
                        has_children: true,
                    },
                ],
                1 => vec![
                    MillerEntry {
                        label: "leaf1".into(),
                        icon: String::new(),
                        has_children: false,
                    },
                    MillerEntry {
                        label: "leaf2".into(),
                        icon: String::new(),
                        has_children: false,
                    },
                ],
                _ => vec![],
            }
        }
        fn preview(&self, _path: &[usize]) -> Option<MillerPreview> {
            None
        }
    }

    #[test]
    fn cursor_navigation_clamps() {
        let mut state = MillerState::new();
        let src = FakeSource;
        move_cursor(&mut state, &src, 3); // overshoots → clamps to 1
        assert_eq!(state.path[0], 1);
        move_cursor(&mut state, &src, -10);
        assert_eq!(state.path[0], 0);
    }

    #[test]
    fn drill_into_branch_then_ascend() {
        let mut state = MillerState::new();
        let src = FakeSource;
        assert!(drill(&mut state, &src));
        assert_eq!(state.focus, 1);
        assert_eq!(state.path.len(), 2);
        assert!(ascend(&mut state));
        assert_eq!(state.focus, 0);
        assert_eq!(state.path.len(), 1);
        // Already at column 0 → ascend returns false.
        assert!(!ascend(&mut state));
    }

    #[test]
    fn drill_returns_false_on_leaf() {
        let mut state = MillerState::new();
        let src = FakeSource;
        drill(&mut state, &src); // into alpha
        // Now at depth 1 looking at leaves; drilling further should bail.
        assert!(!drill(&mut state, &src));
        assert_eq!(state.focus, 1);
    }
}
