use std::{
    cell::{Cell, RefCell},
    time::{Duration, Instant},
};

use ratatui::{buffer::Buffer, layout::Rect, style::Style};
use tachyonfx::{
    CellFilter, EffectManager, Interpolation,
    fx::{self, EvolveSymbolSet},
    pattern::SweepPattern,
};

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
enum TuiEffectKey {
    #[default]
    ChatClear,
    ModelPickerOpen,
}

pub struct TuiEffects {
    prompt_border_phase: Cell<u16>,
    chat: RefCell<EffectManager<TuiEffectKey>>,
    picker: RefCell<EffectManager<TuiEffectKey>>,
    model_picker_open_armed: Cell<bool>,
    frame_delta: Cell<Duration>,
    last_tick: Cell<Instant>,
}

impl Default for TuiEffects {
    fn default() -> Self {
        Self {
            prompt_border_phase: Cell::new(0),
            chat: RefCell::new(EffectManager::default()),
            picker: RefCell::new(EffectManager::default()),
            model_picker_open_armed: Cell::new(false),
            frame_delta: Cell::new(Duration::from_millis(16)),
            last_tick: Cell::new(Instant::now()),
        }
    }
}

impl TuiEffects {
    pub fn prepare_frame(&self) {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(self.last_tick.replace(now));
        self.frame_delta.set(elapsed.min(Duration::from_millis(80)));
    }

    pub fn advance_prompt_border_sweep(&self) {
        self.prompt_border_phase
            .set(self.prompt_border_phase.get().wrapping_add(1));
    }

    pub fn prompt_border_phase(&self) -> u16 {
        self.prompt_border_phase.get()
    }

    pub fn trigger_chat_clear(&self, area: Rect) {
        let effect = fx::explode(10.0, 3.0, 800)
            .with_area(area)
            .with_filter(CellFilter::Text);
        self.trigger_chat_effect(effect);
    }

    pub fn trigger_chat_reveal(&self, area: Rect) {
        let effect = fx::explode(10.0, 3.0, 700)
            .with_area(area)
            .with_filter(CellFilter::Text)
            .reversed();
        self.trigger_chat_effect(effect);
    }

    fn trigger_chat_effect(&self, effect: tachyonfx::Effect) {
        let mut chat = self.chat.borrow_mut();
        chat.add_unique_effect(TuiEffectKey::ChatClear, effect);
        self.last_tick.set(Instant::now());
        self.frame_delta.set(Duration::from_millis(16));
    }

    pub fn process_chat(&self, buf: &mut Buffer, area: Rect) {
        let before = buf.clone();
        self.chat
            .borrow_mut()
            .process_effects(self.frame_delta.get().into(), buf, area);
        restore_area_background(buf, &before, area);
        restore_outside_area(buf, &before, area);
    }

    pub fn chat_running(&self) -> bool {
        self.chat.borrow().is_running()
    }

    pub fn arm_model_picker_open(&self) {
        self.model_picker_open_armed.set(true);
    }

    pub fn trigger_model_picker_open_if_armed(&self, area: Rect) {
        if !self.model_picker_open_armed.replace(false) || area.is_empty() {
            return;
        }
        let style = Style::default();
        let effect = fx::evolve_into(
            (EvolveSymbolSet::Quadrants, style),
            (180, Interpolation::ExpoOut),
        )
        .with_area(area)
        .with_filter(CellFilter::Text)
        .with_pattern(SweepPattern::down_to_up(5));
        self.picker
            .borrow_mut()
            .add_unique_effect(TuiEffectKey::ModelPickerOpen, effect);
        self.last_tick.set(Instant::now());
        self.frame_delta.set(Duration::from_millis(16));
    }

    pub fn process_model_picker(&self, buf: &mut Buffer, area: Rect) {
        self.picker
            .borrow_mut()
            .process_effects(self.frame_delta.get().into(), buf, area);
    }

    pub fn model_picker_running(&self) -> bool {
        self.model_picker_open_armed.get() || self.picker.borrow().is_running()
    }
}

fn restore_area_background(buf: &mut Buffer, before: &Buffer, area: Rect) {
    for y in area.y..area.bottom().min(buf.area.bottom()) {
        for x in area.x..area.right().min(buf.area.right()) {
            buf[(x, y)].bg = before[(x, y)].bg;
        }
    }
}

fn restore_outside_area(buf: &mut Buffer, before: &Buffer, area: Rect) {
    for y in buf.area.y..buf.area.bottom() {
        for x in buf.area.x..buf.area.right() {
            if !area.contains(ratatui::layout::Position { x, y }) {
                buf[(x, y)] = before[(x, y)].clone();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_border_sweep_advances() {
        let effects = TuiEffects::default();

        effects.advance_prompt_border_sweep();
        effects.advance_prompt_border_sweep();

        assert_eq!(effects.prompt_border_phase(), 2);
    }

    #[test]
    fn chat_clear_effect_runs_until_processed() {
        let effects = TuiEffects::default();
        let area = Rect::new(0, 0, 24, 5);
        let mut buffer = Buffer::empty(area);

        effects.trigger_chat_clear(area);
        assert!(effects.chat_running());

        effects.process_chat(&mut buffer, area);
        assert!(effects.chat_running());
    }

    #[test]
    fn chat_reveal_effect_runs_until_processed() {
        let effects = TuiEffects::default();
        let area = Rect::new(0, 0, 24, 5);
        let mut buffer = Buffer::empty(area);

        effects.trigger_chat_reveal(area);
        assert!(effects.chat_running());

        effects.process_chat(&mut buffer, area);
        assert!(effects.chat_running());
    }

    #[test]
    fn chat_clear_effect_clips_particles_to_chat_area() {
        let effects = TuiEffects::default();
        let effect_area = Rect::new(2, 1, 10, 3);
        let buffer_area = Rect::new(0, 0, 20, 8);
        let mut buffer = Buffer::empty(buffer_area);
        buffer[(0, 0)].set_symbol("P");
        buffer[(2, 1)].set_symbol("x");

        effects.trigger_chat_clear(effect_area);
        effects.process_chat(&mut buffer, effect_area);

        assert_eq!(buffer[(0, 0)].symbol(), "P");
    }

    #[test]
    fn chat_clear_effect_preserves_chat_background() {
        let effects = TuiEffects::default();
        let area = Rect::new(0, 0, 20, 5);
        let mut buffer = Buffer::empty(area);
        buffer[(0, 0)].set_symbol("x");
        buffer[(0, 0)].bg = ratatui::style::Color::Reset;

        effects.trigger_chat_clear(area);
        effects.process_chat(&mut buffer, area);

        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                assert_eq!(buffer[(x, y)].bg, ratatui::style::Color::Reset);
            }
        }
    }

    #[test]
    fn model_picker_open_effect_runs_when_armed() {
        let effects = TuiEffects::default();
        let area = Rect::new(0, 0, 24, 5);
        let mut buffer = Buffer::empty(area);

        effects.arm_model_picker_open();
        effects.trigger_model_picker_open_if_armed(area);
        assert!(effects.model_picker_running());

        effects.process_model_picker(&mut buffer, area);
        assert!(effects.model_picker_running());
    }
}
