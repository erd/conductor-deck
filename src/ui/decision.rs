//! The modal that `d` opens to answer a card's open decisions, oldest first.

use conductor_state::Decision;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Style, Stylize as _};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Widget as _, Wrap};
use unicode_width::UnicodeWidthStr as _;

use super::card;
use super::input::Input;

/// Marks the option the asker recommends.
const REC: &str = "REC";
/// The last row when a decision allows free text: the note becomes the answer.
const OTHER: &str = "other…";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    /// Keys move the choice and cycle decisions.
    List,
    /// Keys type into the note.
    Note,
}

#[derive(Debug)]
pub struct DecisionModal {
    task_id: String,
    /// The worker's pane, to tell it once the answer is recorded.
    pane: Option<String>,
    /// Never empty.
    decisions: Vec<Decision>,
    /// The decision shown.
    index: usize,
    /// The chosen row: an option, or one past the options for [`OTHER`].
    choice: usize,
    note: Input,
    focus: Focus,
}

/// What a key press did to the modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Open,
    Cancel,
    Submit(Answer),
}

/// An answer to record with `conductor decision answer`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub key: String,
    pub value: String,
    /// What the worker is told was chosen.
    pub label: String,
    pub note: Option<String>,
}

impl DecisionModal {
    /// `None` without decisions to answer.
    pub fn new(task_id: String, pane: Option<String>, decisions: Vec<Decision>) -> Option<Self> {
        if decisions.is_empty() {
            return None;
        }
        let mut modal = Self {
            task_id,
            pane,
            decisions,
            index: 0,
            choice: 0,
            note: Input::default(),
            focus: Focus::List,
        };
        modal.reset();
        Some(modal)
    }

    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    pub fn pane(&self) -> Option<&str> {
        self.pane.as_deref()
    }

    /// Whether `n`/`p` have another decision to show.
    pub fn several(&self) -> bool {
        self.decisions.len() > 1
    }

    /// Whether keys type into the note.
    pub fn typing(&self) -> bool {
        self.focus == Focus::Note
    }

    fn decision(&self) -> &Decision {
        &self.decisions[self.index]
    }

    fn rows(&self) -> usize {
        let decision = self.decision();
        decision.options.len() + usize::from(decision.allow_freeform)
    }

    fn other_chosen(&self) -> bool {
        self.choice >= self.decision().options.len()
    }

    /// Starts the shown decision afresh: on the recommended option, with an empty note. A
    /// decision with only free text starts in the note.
    fn reset(&mut self) {
        let decision = &self.decisions[self.index];
        self.choice = (decision.recommend.as_ref())
            .and_then(|value| decision.options.iter().position(|o| o.value == *value))
            .unwrap_or(0);
        self.focus = if decision.options.is_empty() {
            Focus::Note
        } else {
            Focus::List
        };
        self.note = Input::default();
    }

    /// Enter answers, Esc or Ctrl-C cancels, ↑/↓ choose and Tab switches between the options
    /// and the note. In the options, `j`/`k` also choose and `n`/`p` show the next or previous
    /// decision; in the note, other keys type.
    pub fn handle(&mut self, key: &KeyEvent) -> Outcome {
        if key.kind == KeyEventKind::Release {
            return Outcome::Open;
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return Outcome::Cancel,
            KeyCode::Char('c') if control => return Outcome::Cancel,
            KeyCode::Enter => return self.submit().map_or(Outcome::Open, Outcome::Submit),
            KeyCode::Up => self.move_by(-1),
            KeyCode::Down => self.move_by(1),
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::List => Focus::Note,
                    Focus::Note => Focus::List,
                };
            }
            _ if self.focus == Focus::Note => {
                self.note.handle(key);
            }
            KeyCode::Char('k') => self.move_by(-1),
            KeyCode::Char('j') => self.move_by(1),
            KeyCode::Char('n') => self.cycle(true),
            KeyCode::Char('p') => self.cycle(false),
            _ => {}
        }
        Outcome::Open
    }

    fn move_by(&mut self, delta: isize) {
        let last = self.rows().saturating_sub(1);
        self.choice = self.choice.saturating_add_signed(delta).min(last);
    }

    /// Shows the next decision, or the previous one, wrapping around.
    fn cycle(&mut self, forward: bool) {
        let count = self.decisions.len();
        let step = if forward { 1 } else { count - 1 };
        self.index = (self.index + step) % count;
        self.reset();
    }

    /// The answer as chosen, or `None` while [`OTHER`] is chosen with nothing typed, which
    /// moves the focus to the note instead.
    fn submit(&mut self) -> Option<Answer> {
        let decision = self.decision();
        let text = self.note.text().trim();
        let key = decision.key.clone();
        let answer = match decision.options.get(self.choice) {
            Some(option) => Answer {
                key,
                value: option.value.clone(),
                label: option.label.clone(),
                note: (!text.is_empty()).then(|| text.to_owned()),
            },
            None if decision.allow_freeform && !text.is_empty() => Answer {
                key,
                value: text.to_owned(),
                label: text.to_owned(),
                note: None,
            },
            None => {
                self.focus = Focus::Note;
                return None;
            }
        };
        Some(answer)
    }
}

/// Draws the modal at the top of `area`, over whatever was there, and returns where the
/// cursor goes while the note has the focus.
pub fn render(modal: &DecisionModal, area: Rect, buf: &mut Buffer) -> Option<Position> {
    Clear.render(area, buf);
    let decision = modal.decision();
    let width = usize::from(area.width.saturating_sub(2));
    let title = format!(" {}: {} ", modal.task_id, decision.key);
    let mut block = Block::bordered()
        .border_type(BorderType::Thick)
        .border_style(Style::new().yellow())
        .title(Line::from(card::truncate(&title, width)).bold());
    if modal.several() {
        let position = format!(" {}/{} ", modal.index + 1, modal.decisions.len());
        block = block.title(Line::from(position).right_aligned());
    }

    let mut lines = vec![Line::from(decision.title.clone()).bold()];
    lines.extend(
        decision
            .about
            .lines()
            .map(|line| Line::from(line.to_owned()).dim()),
    );
    lines.push(Line::default());
    for (index, option) in decision.options.iter().enumerate() {
        let recommended = decision.recommend.as_ref() == Some(&option.value);
        lines.push(row(
            &option.label,
            recommended,
            index == modal.choice,
            width,
        ));
        if let Some(hint) = &option.hint {
            lines.push(card::fit(
                vec![Span::raw(format!("    {hint}")).dim()],
                width,
            ));
        }
    }
    if decision.allow_freeform {
        lines.push(row(OTHER, false, modal.other_chosen(), width));
    }
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let content =
        u16::try_from(paragraph.line_count(area.width.saturating_sub(2))).unwrap_or(u16::MAX);
    // The content, a blank line and the note, inside the border.
    let height = content.saturating_add(4).min(area.height);
    let rect = Rect { height, ..area };
    let inner = block.inner(rect);
    block.render(rect, buf);
    if inner.height == 0 {
        return None;
    }
    let note = Rect {
        y: inner.bottom() - 1,
        height: 1,
        ..inner
    };
    paragraph.render(
        Rect {
            height: inner.height.saturating_sub(2),
            ..inner
        },
        buf,
    );
    let prompt = if modal.other_chosen() {
        "answer › "
    } else {
        "note › "
    };
    let prompt = if modal.typing() {
        Span::raw(prompt).bold()
    } else {
        Span::raw(prompt).dim()
    };
    let cursor = super::draw_input(prompt, modal.note.text(), note, buf);
    modal.typing().then_some(cursor)
}

/// `› label` with `REC` at the right edge when recommended.
fn row(label: &str, recommended: bool, chosen: bool, width: usize) -> Line<'static> {
    let marker = if chosen {
        Span::raw("› ").yellow().bold()
    } else {
        Span::raw("  ")
    };
    let rec = if recommended { REC.width() + 1 } else { 0 };
    let label = card::truncate(label, width.saturating_sub(2 + rec));
    let gap = width.saturating_sub(2 + label.width() + rec);
    let mut spans = vec![marker];
    spans.push(if chosen {
        Span::raw(label).bold()
    } else {
        Span::raw(label)
    });
    if recommended {
        spans.push(Span::raw(" ".repeat(gap + 1)));
        spans.push(Span::raw(REC).yellow());
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deck::testing::decision;

    fn press(modal: &mut DecisionModal, code: KeyCode) -> Outcome {
        modal.handle(&KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn type_text(modal: &mut DecisionModal, text: &str) {
        for c in text.chars() {
            assert_eq!(press(modal, KeyCode::Char(c)), Outcome::Open);
        }
    }

    fn engine() -> Decision {
        let mut engine = decision(
            "db-engine",
            &[
                ("pg", "Postgres", Some("what prod runs")),
                ("sqlite", "SQLite", None),
                ("mysql", "MySQL", None),
            ],
        );
        engine.recommend = Some("sqlite".into());
        engine
    }

    fn modal(decisions: Vec<Decision>) -> DecisionModal {
        DecisionModal::new("t".into(), Some("w2:p1".into()), decisions).unwrap()
    }

    fn answer(key: &str, value: &str, label: &str, note: Option<&str>) -> Outcome {
        Outcome::Submit(Answer {
            key: key.into(),
            value: value.into(),
            label: label.into(),
            note: note.map(Into::into),
        })
    }

    #[test]
    fn needs_a_decision() {
        assert!(DecisionModal::new("t".into(), None, Vec::new()).is_none());
    }

    #[test]
    fn starts_on_the_recommendation_and_moves_within_the_options() {
        let mut modal = modal(vec![engine()]);
        assert_eq!(
            press(&mut modal, KeyCode::Enter),
            answer("db-engine", "sqlite", "SQLite", None)
        );
        press(&mut modal, KeyCode::Char('k'));
        press(&mut modal, KeyCode::Up);
        assert_eq!(
            press(&mut modal, KeyCode::Enter),
            answer("db-engine", "pg", "Postgres", None)
        );
        for _ in 0..4 {
            press(&mut modal, KeyCode::Char('j'));
        }
        assert_eq!(
            press(&mut modal, KeyCode::Enter),
            answer("db-engine", "mysql", "MySQL", None)
        );
    }

    #[test]
    fn tab_types_a_note_that_goes_with_the_answer() {
        let mut modal = modal(vec![engine(), decision("auth", &[("a", "A", None)])]);
        press(&mut modal, KeyCode::Tab);
        assert!(modal.typing());
        // Typed, not taken as keys.
        type_text(&mut modal, " jnp q ");
        press(&mut modal, KeyCode::Down);
        assert_eq!(
            press(&mut modal, KeyCode::Enter),
            answer("db-engine", "mysql", "MySQL", Some("jnp q"))
        );
        press(&mut modal, KeyCode::BackTab);
        assert!(!modal.typing());
    }

    #[test]
    fn n_and_p_cycle_decisions_and_start_each_afresh() {
        let auth = decision("auth", &[("a", "A", None), ("b", "B", None)]);
        let mut modal = modal(vec![engine(), auth]);
        press(&mut modal, KeyCode::Tab);
        type_text(&mut modal, "why");
        press(&mut modal, KeyCode::Tab);
        press(&mut modal, KeyCode::Char('n'));
        assert_eq!(
            press(&mut modal, KeyCode::Enter),
            answer("auth", "a", "A", None)
        );
        press(&mut modal, KeyCode::Char('j'));
        // Wraps around both ways.
        press(&mut modal, KeyCode::Char('n'));
        press(&mut modal, KeyCode::Char('p'));
        press(&mut modal, KeyCode::Char('p'));
        assert_eq!(
            press(&mut modal, KeyCode::Enter),
            answer("db-engine", "sqlite", "SQLite", None)
        );
    }

    #[test]
    fn other_answers_with_the_typed_text() {
        let mut freeform = engine();
        freeform.allow_freeform = true;
        let mut modal = modal(vec![freeform]);
        for _ in 0..3 {
            press(&mut modal, KeyCode::Down);
        }
        // Nothing typed yet: Enter moves to the note instead.
        assert_eq!(press(&mut modal, KeyCode::Enter), Outcome::Open);
        assert!(modal.typing());
        type_text(&mut modal, " duckdb ");
        assert_eq!(
            press(&mut modal, KeyCode::Enter),
            answer("db-engine", "duckdb", "duckdb", None)
        );
    }

    #[test]
    fn free_text_only_starts_in_the_note() {
        let mut open = decision("name", &[]);
        open.allow_freeform = true;
        let mut modal = modal(vec![open]);
        assert!(modal.typing());
        type_text(&mut modal, "Larkspur");
        assert_eq!(
            press(&mut modal, KeyCode::Enter),
            answer("name", "Larkspur", "Larkspur", None)
        );
    }

    #[test]
    fn esc_and_ctrl_c_cancel_and_releases_are_ignored() {
        let mut modal = modal(vec![engine()]);
        let mut release = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert_eq!(modal.handle(&release), Outcome::Open);
        assert_eq!(press(&mut modal, KeyCode::Esc), Outcome::Cancel);
        press(&mut modal, KeyCode::Tab);
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(modal.handle(&ctrl_c), Outcome::Cancel);
    }
}
