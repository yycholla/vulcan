//! Overlay/picker rendering helpers extracted from `tui/mod.rs` (YYC-108).
//!
//! These functions own the slash-palette, session picker, model picker,
//! provider picker, and diff scrubber overlays. They take a `&mut Frame`
//! plus the relevant slices of `AppState` so they can draw without
//! holding any other state.

use std::sync::Arc;

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph, Wrap},
};
use tokio::sync::Mutex;

use crate::agent::Agent;
use crate::config::Config;

use super::keymap::SlashCommand;
use super::miller_columns;
use super::state::{AppState, SessionStatus};
use super::surface::{SurfaceFrame, SurfacePlacement, resolve_surface_area};
use super::theme::Theme;
use super::widgets::ProviderPickerWidget;
use super::{body, short_id};

pub(super) fn draw_palette(
    f: &mut Frame,
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

pub(super) fn draw_session_picker(f: &mut Frame, area: Rect, app: &AppState) {
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
    f.render_widget(Clear, box_area);
    f.render_widget(
        Paragraph::new("").style(Style::default().bg(theme.body_bg)),
        box_area,
    );

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
            .session_picker_selection()
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
                Span::styled(format!("{:<12}", short_id(&s.id)), name_style),
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

pub(super) fn draw_model_picker(f: &mut Frame, area: Rect, app: &AppState) {
    // YYC-102: render via the universal miller_columns widget. The
    // overlay anchors top-left and grows rightward as the user drills.
    // Column 0 = configured providers; columns 1+ = lab/series/version.
    let Some(model_picker) = app.model_picker_state() else {
        return;
    };
    let source = model_picker.source();
    let state = model_picker.miller_state();
    // Inset by 1 from the top-left so we don't paint over the very edge.
    let rect = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    if let Some(occupied) = miller_columns::render(f, rect, &state, &source, &app.theme) {
        app.effects.trigger_model_picker_open_if_armed(occupied);
        app.effects.process_model_picker(f.buffer_mut(), occupied);
    }
    // Footer hint anchored to the bottom of the area.
    let hint = "  hjkl navigate · Enter select · Esc cancel  ";
    let footer = Rect {
        x: area.x + 1,
        y: area.y + area.height.saturating_sub(1),
        width: area.width.saturating_sub(2),
        height: 1,
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(hint, app.theme.muted))),
        footer,
    );
}

pub(super) fn draw_surface_overlays(f: &mut Frame, area: Rect, app: &AppState) {
    for frame in app.surface_frames() {
        match frame {
            SurfaceFrame::FullscreenCanvas(_) => {}
            SurfaceFrame::SessionPicker { .. } => draw_session_picker(f, area, app),
            SurfaceFrame::ModelPicker => draw_model_picker(f, area, app),
            SurfaceFrame::ProviderPicker { items, selection } => {
                f.render_widget(
                    ProviderPickerWidget {
                        theme: &app.theme,
                        items: &items,
                        selection,
                    },
                    area,
                );
            }
            SurfaceFrame::DiffScrubber => draw_diff_scrubber(f, area, app),
            SurfaceFrame::PausePrompt => {}
            SurfaceFrame::TextSurface {
                title,
                body,
                placement,
            } => draw_text_surface(f, area, &title, &body, placement, app),
        }
    }
}

pub(super) fn draw_diagnostics(f: &mut Frame, area: Rect, app: &AppState) {
    if !app.show_diagnostics || area.width < 28 || area.height < 7 {
        return;
    }

    let width = area.width.min(54);
    let height = 7;
    let box_area = Rect {
        x: area.x + area.width.saturating_sub(width),
        y: area.y,
        width,
        height,
    };
    f.render_widget(Clear, box_area);
    f.render_widget(
        Paragraph::new("").style(Style::default().bg(app.theme.body_bg)),
        box_area,
    );

    let active = app.diagnostics.active_surface.as_deref().unwrap_or("none");
    let pending = app.queue.len() + app.deferred_queue.len();
    let token_ratio = app.context_ratio() * 100.0;
    let lines = vec![
        Line::from(Span::styled(
            " diagnostics",
            app.theme.accent.add_modifier(Modifier::BOLD),
        )),
        Line::from(format!(
            " frame {:>6} · draw {:>5.1}ms",
            app.diagnostics.frame_count, app.diagnostics.last_draw_ms
        )),
        Line::from(format!(
            " surfaces {:>2} · active {active}",
            app.diagnostics.surface_count
        )),
        Line::from(format!(
            " chat {:>4} · queue {:>2}",
            app.messages.len(),
            pending
        )),
        Line::from(format!(
            " tokens {} / {} · {:>3.0}%",
            crate::tui::state::format_thousands(app.prompt_tokens_last),
            crate::tui::state::format_thousands(app.token_max),
            token_ratio
        )),
    ];
    let inner = Rect {
        x: box_area.x + 2,
        y: box_area.y + 1,
        width: box_area.width.saturating_sub(4),
        height: box_area.height.saturating_sub(2),
    };
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(app.theme.body_bg)),
        inner,
    );
    draw_picker_border(f, box_area, &app.theme);
}

pub(super) fn draw_text_surface(
    f: &mut Frame,
    area: Rect,
    title: &str,
    body_lines: &[String],
    placement: SurfacePlacement,
    app: &AppState,
) {
    let placement = compact_text_surface_placement(area, body_lines.len(), placement);
    let box_area = resolve_surface_area(area, placement);
    if box_area.height < 4 {
        return;
    }

    f.render_widget(Clear, box_area);
    f.render_widget(
        Paragraph::new("").style(body().bg(app.theme.body_bg)),
        box_area,
    );

    let bar = Rect {
        x: box_area.x,
        y: box_area.y,
        width: box_area.width,
        height: 1,
    };
    let title = format!("  {title}  ");
    f.render_widget(
        Paragraph::new(title).style(app.theme.accent.add_modifier(Modifier::BOLD)),
        bar,
    );

    let inner = Rect {
        x: box_area.x + 2,
        y: box_area.y + 2,
        width: box_area.width.saturating_sub(4),
        height: box_area.height.saturating_sub(4),
    };
    let lines = body_lines
        .iter()
        .cloned()
        .map(Line::from)
        .collect::<Vec<_>>();
    f.render_widget(
        Paragraph::new(lines)
            .style(body().bg(app.theme.body_bg))
            .wrap(Wrap { trim: false }),
        inner,
    );

    let hint = Rect {
        x: box_area.x + 2,
        y: box_area.y + box_area.height.saturating_sub(2),
        width: box_area.width.saturating_sub(4),
        height: 1,
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Esc/Ctrl+C close",
            app.theme.muted,
        ))),
        hint,
    );
    draw_picker_border(f, box_area, &app.theme);
}

fn compact_text_surface_placement(
    area: Rect,
    body_line_count: usize,
    placement: SurfacePlacement,
) -> SurfacePlacement {
    match placement {
        SurfacePlacement::Modal { width, height } => {
            let max_height = area.height.saturating_sub(2);
            let natural_height = (body_line_count as u16 + 5).max(8);
            SurfacePlacement::Modal {
                width: width.min(area.width),
                height: natural_height.min(height).min(max_height),
            }
        }
        SurfacePlacement::Drawer { edge, size } => SurfacePlacement::Drawer {
            edge,
            size: size.min(match edge {
                super::surface::DrawerEdge::Left | super::surface::DrawerEdge::Right => area.width,
                super::surface::DrawerEdge::Top | super::surface::DrawerEdge::Bottom => area.height,
            }),
        },
        SurfacePlacement::Fullscreen => SurfacePlacement::Fullscreen,
    }
}

pub(super) async fn open_unified_picker(
    app: &mut AppState,
    config: &Config,
    agent: &Arc<Mutex<Agent>>,
    active_model_id: &str,
    active_provider_models: Vec<crate::provider::catalog::ModelInfo>,
) {
    use std::collections::HashMap;

    // Build the column-0 list: [default + named profiles, sorted].
    let mut labels: Vec<String> = Vec::new();
    let mut keys: Vec<Option<String>> = Vec::new();
    labels.push("default".to_string());
    keys.push(None);
    let mut named: Vec<&String> = config.providers.keys().collect();
    named.sort();
    for n in named {
        labels.push(n.clone());
        keys.push(Some(n.clone()));
    }

    // Determine active provider key.
    let active_profile = {
        let a = agent.lock().await;
        a.active_profile().map(str::to_string)
    };
    let active_key: String = active_profile.clone().unwrap_or_else(|| "default".into());

    // Seed the catalog cache with the already-loaded active provider.
    let mut items_by_key: HashMap<String, Vec<crate::provider::catalog::ModelInfo>> =
        HashMap::new();
    items_by_key.insert(active_key.clone(), active_provider_models);

    // Fetch catalogs for the other providers in parallel. Disable_catalog
    // entries (e.g. Ollama) are skipped — they get an empty tree and a
    // "type the model id with /model <id>" hint via the empty list.
    let mut handles: Vec<(String, tokio::task::JoinHandle<_>)> = Vec::new();
    for (key_opt, _label) in keys.iter().zip(labels.iter()) {
        let cache_key = key_opt.clone().unwrap_or_else(|| "default".into());
        if items_by_key.contains_key(&cache_key) {
            continue;
        }
        let provider_cfg = match key_opt {
            Some(name) => config.providers.get(name).cloned(),
            None => Some(config.provider.clone()),
        };
        let Some(provider_cfg) = provider_cfg else {
            continue;
        };
        if provider_cfg.disable_catalog {
            items_by_key.insert(cache_key, Vec::new());
            continue;
        }
        let api_key = config.api_key_for(&provider_cfg).unwrap_or_default();
        let handle = tokio::spawn(async move {
            use std::time::Duration;
            let client = match reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
            {
                Ok(c) => c,
                Err(_) => return Vec::new(),
            };
            let cat = crate::provider::catalog::for_base_url(
                client,
                &provider_cfg.base_url,
                &api_key,
                Duration::from_secs(0),
            );
            cat.list_models().await.unwrap_or_default()
        });
        handles.push((cache_key, handle));
    }
    for (cache_key, handle) in handles {
        match handle.await {
            Ok(models) => {
                items_by_key.insert(cache_key, models);
            }
            Err(_) => {
                items_by_key.insert(cache_key, Vec::new());
            }
        }
    }

    // Build trees for every provider.
    let mut trees_by_key: HashMap<String, crate::tui::model_picker::ModelTree> = HashMap::new();
    for (key_opt, label) in keys.iter().zip(labels.iter()) {
        let cache_key = key_opt.clone().unwrap_or_else(|| "default".into());
        let models = items_by_key.get(&cache_key).cloned().unwrap_or_default();
        let tree = crate::tui::model_picker::build_model_tree(label, &models);
        trees_by_key.insert(cache_key, tree);
    }

    // Initial path: active provider in column 0, then drill into the
    // active model if we can match it.
    let active_idx = keys
        .iter()
        .position(|k| match (k, &active_profile) {
            (None, None) => true,
            (Some(a), Some(b)) => a == b,
            _ => false,
        })
        .unwrap_or(0);

    let active_provider_tree = trees_by_key.get(&active_key).cloned().unwrap_or_default();
    let active_provider_items = items_by_key.get(&active_key).cloned().unwrap_or_default();
    let inner_path = initial_path_for_active_model(
        &active_provider_tree,
        active_model_id,
        &active_provider_items,
    );
    let mut path = vec![active_idx];
    path.extend(inner_path);

    app.open_model_picker(crate::tui::model_picker::ModelPickerState {
        provider_labels: labels,
        provider_keys: keys,
        items_by_key,
        trees_by_key,
        focus: path.len().saturating_sub(1),
        path,
    });
    app.effects.arm_model_picker_open();
}

fn initial_path_for_active_model(
    tree: &crate::tui::model_picker::ModelTree,
    active_id: &str,
    items: &[crate::provider::catalog::ModelInfo],
) -> Vec<usize> {
    let target = items.iter().position(|m| m.id == active_id);
    fn find_path(
        nodes: &[crate::tui::model_picker::TreeNode],
        target: Option<usize>,
        path: &mut Vec<usize>,
    ) -> bool {
        for (i, node) in nodes.iter().enumerate() {
            path.push(i);
            if node.model_index.is_some() && node.model_index == target {
                return true;
            }
            if find_path(&node.children, target, path) {
                return true;
            }
            path.pop();
        }
        false
    }
    let mut path = Vec::new();
    if !find_path(&tree.labs, target, &mut path) {
        // No exact match — start from column 0 with no drilled selection.
        path.clear();
        path.push(0);
    }
    path
}

pub(super) fn draw_diff_scrubber(f: &mut Frame, area: Rect, app: &AppState) {
    let Some(scrubber) = app.diff_scrubber_state() else {
        return;
    };
    let theme = &app.theme;
    let width = area.width.min(96);
    let total = scrubber.hunks.len() as u16;
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
    f.render_widget(Clear, box_area);
    f.render_widget(
        Paragraph::new("").style(Style::default().bg(theme.body_bg)),
        box_area,
    );

    let bar = Rect {
        x: box_area.x,
        y: box_area.y,
        width: box_area.width,
        height: 1,
    };
    let title = format!("  Edit Scrubber — {} ({} hunks)  ", scrubber.path, total);
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
    let active = scrubber
        .selection
        .min(scrubber.hunks.len().saturating_sub(1));
    for (i, hunk) in scrubber.hunks.iter().enumerate() {
        let is_active = i == active;
        let accepted = scrubber.accepted.get(i).copied().unwrap_or(true);
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

    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme.body_bg)),
        list_area,
    );
    draw_picker_border(f, box_area, theme);
}

/// Simple border drawn with box-drawing characters.
fn draw_picker_border(f: &mut Frame, r: Rect, theme: &Theme) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::surface::DrawerEdge;
    use ratatui::{
        Terminal,
        backend::{Backend, TestBackend},
    };

    #[test]
    fn snapshot_text_surface_modal_uses_compact_centered_chrome() {
        let body = render_text_surface_snapshot(
            Rect::new(0, 0, 48, 14),
            SurfacePlacement::Modal {
                width: 30,
                height: 12,
            },
        );

        insta::assert_snapshot!(body, @r"
        ////////////////////////////////////////////////
        ////////////////////////////////////////////////
        ////////////////////////////////////////////////
        /////////──────────────────────────────/////////
        /////////│                            │/////////
        /////////│ alpha                      │/////////
        /////////│ beta                       │/////////
        /////////│ gamma                      │/////////
        /////////│                            │/////////
        /////////│ Esc/Ctrl+C close           │/////////
        /////////──────────────────────────────/////////
        ////////////////////////////////////////////////
        ////////////////////////////////////////////////
        ////////////////////////////////////////////////
        ");
    }

    #[test]
    fn snapshot_text_surface_right_drawer_is_opaque_and_edge_aligned() {
        let body = render_text_surface_snapshot(
            Rect::new(0, 0, 48, 10),
            SurfacePlacement::Drawer {
                edge: DrawerEdge::Right,
                size: 22,
            },
        );

        insta::assert_snapshot!(body, @r"
        //////////////////////////──────────────────────
        //////////////////////////│                    │
        //////////////////////////│ alpha              │
        //////////////////////////│ beta               │
        //////////////////////////│ gamma              │
        //////////////////////////│                    │
        //////////////////////////│                    │
        //////////////////////////│                    │
        //////////////////////////│ Esc/Ctrl+C close   │
        //////////////////////////──────────────────────
        ");
    }

    #[test]
    fn snapshot_text_surface_bottom_drawer_is_opaque_and_bottom_aligned() {
        let body = render_text_surface_snapshot(
            Rect::new(0, 0, 48, 12),
            SurfacePlacement::Drawer {
                edge: DrawerEdge::Bottom,
                size: 8,
            },
        );

        insta::assert_snapshot!(body, @r"
        ////////////////////////////////////////////////
        ////////////////////////////////////////////////
        ////////////////////////////////////////////////
        ////////////////////////////////////////////////
        ────────────────────────────────────────────────
        │                                              │
        │ alpha                                        │
        │ beta                                         │
        │ gamma                                        │
        │                                              │
        │ Esc/Ctrl+C close                             │
        ────────────────────────────────────────────────
        ");
    }

    #[test]
    fn snapshot_diagnostics_overlay_is_top_right_and_opaque() {
        let backend = TestBackend::new(64, 10);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let mut app = AppState::new("test-model".into(), 128_000);
        app.show_diagnostics = true;
        app.prompt_tokens_last = 32_000;
        app.queue.push_back("queued".into());
        app.note_frame_draw(
            std::time::Duration::from_micros(2_500),
            std::time::Duration::from_millis(20),
        );

        terminal
            .draw(|f| {
                let area = f.area();
                let background = (0..area.height)
                    .map(|_| Line::from("/".repeat(area.width as usize)))
                    .collect::<Vec<_>>();
                f.render_widget(Paragraph::new(background), area);
                draw_diagnostics(f, area, &app);
            })
            .expect("draw diagnostics");

        insta::assert_snapshot!(terminal_buffer_text(terminal.backend()), @r"
        //////////──────────────────────────────────────────────────────
        //////////│  diagnostics                                       │
        //////////│  frame      1 · draw   2.5ms                       │
        //////////│  surfaces  0 · active none                         │
        //////////│  chat    0 · queue  1                              │
        //////////│  tokens 32,000 / 128,000 ·  25%                    │
        //////////──────────────────────────────────────────────────────
        ////////////////////////////////////////////////////////////////
        ////////////////////////////////////////////////////////////////
        ////////////////////////////////////////////////////////////////
        ");
    }

    fn render_text_surface_snapshot(area: Rect, placement: SurfacePlacement) -> String {
        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let app = AppState::new("test-model".into(), 128_000);
        let body_lines = vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()];

        terminal
            .draw(|f| {
                let background = (0..area.height)
                    .map(|_| Line::from("/".repeat(area.width as usize)))
                    .collect::<Vec<_>>();
                f.render_widget(
                    Paragraph::new(background).style(Style::default().bg(app.theme.body_bg)),
                    area,
                );
                draw_text_surface(f, area, "Surface Lab", &body_lines, placement, &app);
            })
            .expect("draw text surface");

        terminal_buffer_text(terminal.backend())
    }

    fn terminal_buffer_text(backend: &TestBackend) -> String {
        let area = backend.size().expect("backend size");
        let buffer = backend.buffer();
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            if y + 1 < area.height {
                out.push('\n');
            }
        }
        out
    }
}
