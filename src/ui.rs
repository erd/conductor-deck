use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::style::Stylize as _;
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph};

use crate::deck::CardModel;

pub const TITLE: &str = "conductor";

pub enum View<'a> {
    NoConductor {
        reason: &'a str,
    },
    Cards {
        cards: &'a [CardModel],
        /// The latest error, if any.
        problem: Option<&'a str>,
    },
}

/// A plain list of task titles until the cards are rendered (#4).
pub fn draw(frame: &mut Frame, view: &View) {
    let lines: Vec<Line> = match view {
        View::NoConductor { reason } => {
            vec!["no conductor here".into(), Line::from(*reason).dim()]
        }
        View::Cards { cards, problem } => {
            let mut lines: Vec<Line> = cards
                .iter()
                .map(|card| Line::from(card.task.title.as_str()))
                .collect();
            if lines.is_empty() {
                lines.push(Line::from("no tasks").dim());
            }
            if let Some(problem) = problem {
                lines.push(Line::from(*problem).red());
            }
            lines
        }
    };
    let block = Block::bordered().title(TITLE);
    frame.render_widget(Paragraph::new(lines).block(block), frame.area());
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

    fn render(view: &View) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(20, 4)).unwrap();
        terminal.draw(|frame| draw(frame, view)).unwrap();
        terminal
    }

    fn text(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn draws_no_conductor() {
        let terminal = render(&View::NoConductor {
            reason: "not a repo",
        });
        assert_eq!(
            text(&terminal),
            [
                "┌conductor─────────┐",
                "│no conductor here │",
                "│not a repo        │",
                "└──────────────────┘",
            ]
        );
    }

    #[test]
    fn draws_empty_deck_with_problem() {
        let view = View::Cards {
            cards: &[],
            problem: Some("herdr down"),
        };
        assert_eq!(
            text(&render(&view)),
            [
                "┌conductor─────────┐",
                "│no tasks          │",
                "│herdr down        │",
                "└──────────────────┘",
            ]
        );
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
