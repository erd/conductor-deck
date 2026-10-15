//! The one-line input that `m` opens at the bottom of the deck.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

#[derive(Debug, Default)]
pub struct Input {
    text: String,
}

/// What a key press did to the input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Editing,
    Cancel,
    Send(String),
}

impl Input {
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Enter sends, Esc or Ctrl-C cancels, Backspace deletes the last character, and other
    /// characters are typed, including the deck's own keys.
    pub fn handle(&mut self, key: &KeyEvent) -> Outcome {
        if key.kind == KeyEventKind::Release {
            return Outcome::Editing;
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter => return Outcome::Send(std::mem::take(&mut self.text)),
            KeyCode::Esc => return Outcome::Cancel,
            KeyCode::Char('c') if control => return Outcome::Cancel,
            KeyCode::Char(c) if !control => self.text.push(c),
            KeyCode::Backspace => {
                self.text.pop();
            }
            _ => {}
        }
        Outcome::Editing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(input: &mut Input, code: KeyCode) -> Outcome {
        input.handle(&KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn type_text(input: &mut Input, text: &str) {
        for c in text.chars() {
            assert_eq!(press(input, KeyCode::Char(c)), Outcome::Editing);
        }
    }

    #[test]
    fn types_deletes_and_sends() {
        let mut input = Input::default();
        type_text(&mut input, "q jk cé");
        assert_eq!(input.text(), "q jk cé");
        press(&mut input, KeyCode::Backspace);
        press(&mut input, KeyCode::Left);
        assert_eq!(input.text(), "q jk c");
        assert_eq!(
            press(&mut input, KeyCode::Enter),
            Outcome::Send("q jk c".into())
        );
        assert_eq!(input.text(), "");
    }

    #[test]
    fn esc_and_ctrl_c_cancel() {
        let mut input = Input::default();
        type_text(&mut input, "hi");
        assert_eq!(press(&mut input, KeyCode::Esc), Outcome::Cancel);
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(input.handle(&ctrl_c), Outcome::Cancel);
    }

    #[test]
    fn ignores_releases_and_other_control_keys() {
        let mut input = Input::default();
        let mut release = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert_eq!(input.handle(&release), Outcome::Editing);
        let ctrl_a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL);
        assert_eq!(input.handle(&ctrl_a), Outcome::Editing);
        assert_eq!(input.text(), "");
    }
}
