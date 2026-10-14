use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::widgets::Block;

pub const TITLE: &str = "conductor";

pub fn draw(frame: &mut Frame) {
    frame.render_widget(Block::bordered().title(TITLE), frame.area());
}

/// Raw mode delivers Ctrl-C as a key event rather than SIGINT, so it is handled here with `q`.
pub fn is_quit(key: &KeyEvent) -> bool {
    if key.kind != KeyEventKind::Press {
        return false;
    }
    match key.code {
        KeyCode::Char('q') => true,
        KeyCode::Char('c') => key.modifiers.contains(KeyModifiers::CONTROL),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn draws_titled_empty_frame() {
        let mut terminal = Terminal::new(TestBackend::new(20, 4)).unwrap();
        terminal.draw(draw).unwrap();
        terminal.backend().assert_buffer_lines([
            "┌conductor─────────┐",
            "│                  │",
            "│                  │",
            "└──────────────────┘",
        ]);
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn quits_on_q_and_ctrl_c() {
        assert!(is_quit(&key(KeyCode::Char('q'), KeyModifiers::NONE)));
        assert!(is_quit(&key(KeyCode::Char('c'), KeyModifiers::CONTROL)));
    }

    #[test]
    fn ignores_other_keys() {
        assert!(!is_quit(&key(KeyCode::Char('c'), KeyModifiers::NONE)));
        assert!(!is_quit(&key(KeyCode::Enter, KeyModifiers::NONE)));
        let mut release = key(KeyCode::Char('q'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert!(!is_quit(&release));
    }
}
