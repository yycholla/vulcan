use ratatui::{
    Frame as TuiFrame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};
use throbber_widgets_tui::{BRAILLE_SIX, Throbber, WhichUse};

use crate::tui::{
    state::AppState,
    theme::{Palette, body},
    views::{build_chat_window, publish_chat_max_scroll},
    widgets::{PromptRowWidget, frame, prompt_row_height},
};

pub(in crate::tui) fn render(f: &mut TuiFrame, area: Rect, app: &AppState) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),
            Constraint::Length(prompt_row_height(&app.input, area.width, app.mode_label())),
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
    render_status(f, header, app);

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
    app.start_chat_clear_effect_if_pending(chat_area);
    app.effects.process_chat(f.buffer_mut(), chat_area);

    let model_status = app.model_status();
    let prompt = PromptRowWidget {
        mode: app.mode_label(),
        textarea: app.prompt_editor.textarea(),
        hints: app.prompt_hints(),
        model_status: &model_status,
        capacity_ratio: app.context_ratio(),
        thinking: app.thinking,
        activity_active: app.activity_motion_active(),
        activity_throbber: Some(&app.activity_throbber),
        effects: Some(&app.effects),
        theme: &app.theme,
    };
    let (cx, cy) = prompt.cursor(layout[1]);
    f.render_widget(prompt, layout[1]);
    app.cursor_set(cx, cy);
}

fn render_status(f: &mut TuiFrame, area: Rect, app: &AppState) {
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
    let tape = activity_tape(app);
    let text = format!("  {session}   {live}{queue}{reasoning}");
    let style = if app.thinking {
        Style::default()
            .fg(Palette::YELLOW)
            .add_modifier(Modifier::BOLD)
    } else {
        app.theme.muted.add_modifier(Modifier::BOLD)
    };
    let mut spans = vec![Span::styled(format!(" {tape}  "), style)];
    if app.activity_motion_active() {
        spans.push(
            Throbber::default()
                .throbber_set(BRAILLE_SIX)
                .use_type(WhichUse::Spin)
                .style(style)
                .throbber_style(
                    Style::default()
                        .fg(Palette::YELLOW)
                        .add_modifier(Modifier::BOLD),
                )
                .to_symbol_span(&app.activity_throbber),
        );
    }
    spans.push(Span::styled(text, style));
    f.render_widget(Paragraph::new(Line::from(spans)).style(style), area);
}

fn activity_tape(app: &AppState) -> &'static str {
    if !app.thinking {
        return "[..::]";
    }
    const FRAMES: [&str; 8] = [
        "[>   ]", "[=>  ]", "[==> ]", "[===>]", "[ <==]", "[  <=]", "[   <]", "[.  .]",
    ];
    let frame = (app.session_started.elapsed().as_millis() / 180) as usize % FRAMES.len();
    FRAMES[frame]
}
