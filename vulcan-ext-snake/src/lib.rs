use std::sync::{Arc, Mutex};

use vulcan_frontend_api::{
    Canvas, CanvasControl, CanvasFactory, CanvasFrame, CanvasHandle, CanvasKey,
    FrontendCodeExtension, FrontendCommand, FrontendCommandAction, FrontendCtx,
    FrontendExtensionRegistration, TickRate,
};

pub struct SnakeFrontendExtension;

impl FrontendCodeExtension for SnakeFrontendExtension {
    fn id(&self) -> &'static str {
        "snake"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn frontend_capabilities(&self) -> Vec<&'static str> {
        vec!["text_io", "cell_canvas", "raw_keys", "tick_30hz"]
    }

    fn commands(&self) -> Vec<Arc<dyn FrontendCommand>> {
        vec![Arc::new(SnakeCommand)]
    }
}

struct SnakeCommand;

impl FrontendCommand for SnakeCommand {
    fn name(&self) -> &'static str {
        "snake"
    }

    fn description(&self) -> &'static str {
        "Open snake canvas"
    }

    fn run(&self, ctx: &mut FrontendCtx) -> FrontendCommandAction {
        let state = Arc::new(Mutex::new(SnakeState::new()));
        let tick_state = Arc::clone(&state);
        let _ = ctx.ui.set_tick(TickRate::Tick30Hz, move |handle| {
            if let Ok(mut state) = tick_state.lock() {
                state.step();
                if state.exited {
                    handle.stop();
                }
            }
        });
        let canvas_state = Arc::clone(&state);
        let _ = ctx
            .ui
            .custom(CanvasFactory::new(move |_handle: CanvasHandle| {
                Box::new(SnakeCanvas {
                    state: Arc::clone(&canvas_state),
                })
            }));
        FrontendCommandAction::Noop
    }
}

struct SnakeCanvas {
    state: Arc<Mutex<SnakeState>>,
}

impl Canvas for SnakeCanvas {
    fn render(&self) -> CanvasFrame {
        let state = self.state.lock().expect("snake state");
        CanvasFrame {
            title: "Snake".into(),
            lines: state.render(),
        }
    }

    fn on_key(&self, key: CanvasKey, handle: &CanvasHandle) -> CanvasControl {
        let mut state = self.state.lock().expect("snake state");
        match key {
            CanvasKey::Esc | CanvasKey::CtrlC => {
                state.exited = true;
                handle.exit();
                return CanvasControl::Exit;
            }
            CanvasKey::Up => state.turn(Direction::Up),
            CanvasKey::Down => state.turn(Direction::Down),
            CanvasKey::Left => state.turn(Direction::Left),
            CanvasKey::Right => state.turn(Direction::Right),
            _ => {}
        }
        CanvasControl::Continue
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
    Up,
    Down,
    Left,
    Right,
}

impl Direction {
    fn delta(self) -> (i16, i16) {
        match self {
            Direction::Up => (0, -1),
            Direction::Down => (0, 1),
            Direction::Left => (-1, 0),
            Direction::Right => (1, 0),
        }
    }

    fn opposite(self, other: Self) -> bool {
        matches!(
            (self, other),
            (Direction::Up, Direction::Down)
                | (Direction::Down, Direction::Up)
                | (Direction::Left, Direction::Right)
                | (Direction::Right, Direction::Left)
        )
    }
}

struct SnakeState {
    width: i16,
    height: i16,
    snake: Vec<(i16, i16)>,
    food: (i16, i16),
    dir: Direction,
    score: u16,
    exited: bool,
}

impl SnakeState {
    fn new() -> Self {
        Self {
            width: 24,
            height: 12,
            snake: vec![(8, 6), (7, 6), (6, 6)],
            food: (15, 6),
            dir: Direction::Right,
            score: 0,
            exited: false,
        }
    }

    fn turn(&mut self, next: Direction) {
        if !self.dir.opposite(next) {
            self.dir = next;
        }
    }

    fn step(&mut self) {
        if self.exited {
            return;
        }
        let (dx, dy) = self.dir.delta();
        let (hx, hy) = self.snake[0];
        let head = (
            (hx + dx).rem_euclid(self.width),
            (hy + dy).rem_euclid(self.height),
        );
        if self.snake.contains(&head) {
            *self = Self::new();
            return;
        }
        self.snake.insert(0, head);
        if head == self.food {
            self.score = self.score.saturating_add(1);
            self.food = (
                (head.0 + 7 + self.score as i16).rem_euclid(self.width),
                (head.1 + 5 + self.score as i16).rem_euclid(self.height),
            );
        } else {
            self.snake.pop();
        }
    }

    fn render(&self) -> Vec<String> {
        let mut lines = Vec::with_capacity(self.height as usize + 2);
        lines.push(format!(
            "Score: {}   arrows move   Esc/Ctrl+C exits",
            self.score
        ));
        for y in 0..self.height {
            let mut row = String::with_capacity(self.width as usize + 2);
            row.push('|');
            for x in 0..self.width {
                let cell = if (x, y) == self.snake[0] {
                    '@'
                } else if self.snake.iter().skip(1).any(|p| *p == (x, y)) {
                    'o'
                } else if (x, y) == self.food {
                    '*'
                } else {
                    ' '
                };
                row.push(cell);
            }
            row.push('|');
            lines.push(row);
        }
        lines
    }
}

inventory::submit! {
    FrontendExtensionRegistration {
        register: || Arc::new(SnakeFrontendExtension) as Arc<dyn FrontendCodeExtension>,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_declares_canvas_raw_key_and_tick_caps() {
        let caps = SnakeFrontendExtension.frontend_capabilities();
        assert!(caps.contains(&"cell_canvas"));
        assert!(caps.contains(&"raw_keys"));
        assert!(caps.contains(&"tick_30hz"));
    }

    #[test]
    fn snake_command_opens_canvas_and_tick() {
        let command = SnakeCommand;
        let mut ctx = FrontendCtx::default();

        let action = command.run(&mut ctx);

        assert!(matches!(action, FrontendCommandAction::Noop));
        assert_eq!(ctx.ui.drain_canvas_requests().len(), 1);
        assert_eq!(ctx.ui.drain_tick_requests().len(), 1);
    }

    #[test]
    fn canvas_exit_key_sets_handle() {
        let canvas = SnakeCanvas {
            state: Arc::new(Mutex::new(SnakeState::new())),
        };
        let handle = CanvasHandle::default();

        let control = canvas.on_key(CanvasKey::Esc, &handle);

        assert_eq!(control, CanvasControl::Exit);
        assert!(handle.has_exited());
    }
}
