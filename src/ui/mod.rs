//! The deck's screen: a header with counts, the stacked cards, and a footer with key hints.

mod card;
mod list;

use conductor_state::Phase;
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use jiff::Timestamp;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::Stylize as _;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget as _, Wrap};

use crate::deck::CardModel;
use crate::deck::card::Group;
pub use list::Selection;

#[derive(Clone, Copy)]
pub struct View<'a> {
    /// The repo's name, once the deck knows it.
    pub repo: Option<&'a str>,
    pub body: Body<'a>,
    /// What elapsed times are measured against.
    pub now: Timestamp,
    /// Whether torn-down and abandoned tasks are shown.
    pub show_hidden: bool,
}

#[derive(Clone, Copy)]
pub enum Body<'a> {
    NoConductor {
        reason: &'a str,
    },
    HerdrUnreachable {
        error: &'a str,
    },
    Cards {
        cards: &'a [CardModel],
        /// The latest error, if any.
        problem: Option<&'a str>,
    },
}

/// What a key press or mouse event asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    Up,
    Down,
    ToggleHidden,
}

pub fn draw(frame: &mut Frame, view: &View, selection: &mut Selection) {
    let problem = match view.body {
        Body::Cards { problem, .. } => problem,
        Body::NoConductor { .. } | Body::HerdrUnreachable { .. } => None,
    };
    let [header, body, problem_area, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(u16::from(problem.is_some())),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    let buf = frame.buffer_mut();
    draw_header(view, header, buf);
    let position = match view.body {
        Body::NoConductor { reason } => {
            draw_message(Line::from("no conductor here").bold(), reason, body, buf);
            None
        }
        Body::HerdrUnreachable { error } => {
            let title = Line::from(format!("{} herdr unreachable", card::ATTENTION))
                .red()
                .bold();
            draw_message(title, error, body, buf);
            None
        }
        Body::Cards { cards: [], .. } => {
            draw_message(Line::from("no tasks").dim(), "", body, buf);
            None
        }
        Body::Cards { cards, .. } => draw_cards(cards, view.now, selection, body, buf),
    };
    if let Some(problem) = problem {
        let text = format!("{} {problem}", card::ATTENTION);
        let line = card::fit(vec![Span::raw(text)], usize::from(problem_area.width));
        line.red().render(problem_area, buf);
    }
    draw_footer(view, position, footer, buf);
}

/// The repo name on the left and, with cards, how many need attention, are working, and are
/// awaiting merge on the right.
fn draw_header(view: &View, area: Rect, buf: &mut Buffer) {
    let counts = match view.body {
        Body::Cards { cards, .. } => {
            let count = |group| cards.iter().filter(|card| card.group == group).count();
            let working = card::badge(Phase::Working);
            let merge = card::badge(Phase::AwaitingMerge);
            Line::from(vec![
                Span::raw(format!(
                    "{}{}",
                    card::ATTENTION,
                    count(Group::NeedsAttention)
                ))
                .yellow()
                .bold(),
                Span::raw(" "),
                Span::styled(
                    format!("{}{}", working.symbol, count(Group::Working)),
                    working.style,
                ),
                Span::raw(" "),
                Span::styled(
                    format!("{}{}", merge.symbol, count(Group::AwaitingMerge)),
                    merge.style,
                ),
            ])
        }
        Body::NoConductor { .. } | Body::HerdrUnreachable { .. } => Line::default(),
    };
    let counts_width = u16::try_from(counts.width()).unwrap_or(u16::MAX);
    let [name, _, right] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(u16::from(counts_width > 0)),
        Constraint::Length(counts_width),
    ])
    .areas(area);
    let repo = view.repo.unwrap_or("conductor");
    Line::from(card::truncate(repo, usize::from(name.width)))
        .bold()
        .render(name, buf);
    counts.render(right, buf);
}

/// A state with no cards: a title, and a dim explanation wrapped below it.
fn draw_message(title: Line<'static>, detail: &str, area: Rect, buf: &mut Buffer) {
    let mut lines = vec![Line::default(), title];
    if !detail.is_empty() {
        lines.push(Line::from(detail.to_owned()).dim());
    }
    Paragraph::new(lines)
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true })
        .render(area, buf);
}

/// Draws the cards that fit, scrolled to the selection, and returns the selected card's index.
fn draw_cards(
    cards: &[CardModel],
    now: Timestamp,
    selection: &mut Selection,
    area: Rect,
    buf: &mut Buffer,
) -> Option<usize> {
    let ids: Vec<&str> = cards.iter().map(|card| card.task.id.as_str()).collect();
    let selected = selection.resolve(&ids)?;
    let heights: Vec<u16> = cards.iter().map(card::height).collect();
    let mut y = area.y;
    for index in selection.scroll(&heights, area.height) {
        let height = heights[index].min(area.bottom() - y);
        let rect = Rect { y, height, ..area };
        card::render(&cards[index], now, index == selected, rect, buf);
        y += height;
    }
    Some(selected)
}

/// Key hints on the left; the selection's position on the right.
fn draw_footer(view: &View, position: Option<usize>, area: Rect, buf: &mut Buffer) {
    let hints = match view.body {
        Body::Cards { .. } if view.show_hidden => "j/k move · L hide done · q quit",
        Body::Cards { .. } => "j/k move · L show all · q quit",
        Body::NoConductor { .. } | Body::HerdrUnreachable { .. } => "q quit",
    };
    let position = match (position, view.body) {
        (Some(index), Body::Cards { cards, .. }) => format!("{}/{}", index + 1, cards.len()),
        _ => String::new(),
    };
    let position_width = u16::try_from(position.len()).unwrap_or(u16::MAX);
    let [left, _, right] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(u16::from(position_width > 0)),
        Constraint::Length(position_width),
    ])
    .areas(area);
    card::fit(vec![Span::raw(hints)], usize::from(left.width))
        .dim()
        .render(left, buf);
    Line::from(position).dim().render(right, buf);
}

/// Maps input to an action. Raw mode delivers Ctrl-C as a key event rather than SIGINT, so it
/// quits here along with `q`.
pub fn action(event: &Event) -> Option<Action> {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
            KeyCode::Char('q') => Some(Action::Quit),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(Action::Quit)
            }
            KeyCode::Char('j') | KeyCode::Down => Some(Action::Down),
            KeyCode::Char('k') | KeyCode::Up => Some(Action::Up),
            KeyCode::Char('L') => Some(Action::ToggleHidden),
            _ => None,
        },
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollDown => Some(Action::Down),
            MouseEventKind::ScrollUp => Some(Action::Up),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyEvent, MouseEvent};
    use insta::assert_snapshot;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    use super::*;
    use crate::deck::testing::{REPO_ID, TempRepo, event, fixture, task, ts};
    use crate::deck::{AgentInfo, Deck};
    use crate::herdr::types::AgentStatus;

    const NOW: &str = "12:34:56";

    fn render(width: u16, height: u16, view: &View, selection: &mut Selection) -> TestBackend {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, view, selection)).unwrap();
        terminal.backend().clone()
    }

    fn cards_view<'a>(cards: &'a [CardModel], problem: Option<&'a str>) -> View<'a> {
        View {
            repo: Some("web"),
            body: Body::Cards { cards, problem },
            now: ts(NOW),
            show_hidden: false,
        }
    }

    /// A task with a realistic title and branch.
    fn add(repo: &TempRepo, id: &str, phase: Phase, title: &str, lines: &[String]) {
        let mut task = task(id, phase);
        task.title = title.into();
        task.branch = Some(format!("conductor/{id}"));
        repo.write_task(&task);
        repo.append(id, lines);
    }

    fn cards(repo: &TempRepo) -> Vec<CardModel> {
        let mut deck = Deck::open(repo.home.clone(), REPO_ID.into()).unwrap();
        deck.show_hidden = true;
        deck.cards()
    }

    fn agent(context: &str) -> AgentInfo {
        AgentInfo {
            status: AgentStatus::Working,
            context: Some(context.into()),
            limit: None,
        }
    }

    /// One card in each phase, so each badge shows.
    fn every_phase() -> Vec<CardModel> {
        let repo = TempRepo::new();
        let start = event("12:00:00", r#""kind":"session_start","source":"startup""#);
        let cases: [(&str, Phase, &str, Vec<String>); 11] = [
            ("intake-01", Phase::Intake, "Capture the goal", vec![]),
            ("brief-02", Phase::Briefed, "Brief the export job", vec![]),
            (
                "spawn-03",
                Phase::Spawning,
                "Spin up the worker",
                vec![start.clone()],
            ),
            (
                "fix-login-07",
                Phase::Working,
                "Fix the login redirect loop on expired sessions",
                fixture("working_tool.jsonl"),
            ),
            (
                "db-choice-04",
                Phase::NeedsDecision,
                "Pick a database",
                fixture("decisions.jsonl"),
            ),
            (
                "review-05",
                Phase::Review,
                "Add audit log",
                fixture("idle.jsonl"),
            ),
            (
                "merge-06",
                Phase::AwaitingMerge,
                "Bump dependencies",
                fixture("pr_lifecycle.jsonl")[..8].to_vec(),
            ),
            (
                "merged-08",
                Phase::Merged,
                "Rename the config file",
                fixture("exited.jsonl"),
            ),
            (
                "failed-09",
                Phase::Failed,
                "Migrate the queue",
                fixture("idle_error.jsonl"),
            ),
            (
                "down-10",
                Phase::TornDown,
                "Old spike",
                fixture("exited.jsonl"),
            ),
            ("dropped-11", Phase::Abandoned, "Dropped idea", vec![]),
        ];
        for (id, phase, title, lines) in cases {
            add(&repo, id, phase, title, &lines);
        }
        let mut cards = cards(&repo);
        for card in &mut cards {
            if card.task.id == "fix-login-07" {
                card.agent = Some(agent("⛁ 26% (263k)"));
            }
        }
        cards
    }

    #[test]
    fn one_card_per_phase() {
        let cards = every_phase();
        for width in [40, 60] {
            let backend = render(
                width,
                64,
                &cards_view(&cards, None),
                &mut Selection::default(),
            );
            assert_snapshot!(format!("one_card_per_phase_{width}"), backend);
        }
    }

    #[test]
    fn needs_permission_and_pr_line() {
        let repo = TempRepo::new();
        add(
            &repo,
            "perm-01",
            Phase::Working,
            "Clean the build directory",
            &fixture("needs_permission.jsonl"),
        );
        let mut pr = task("pr-02", Phase::AwaitingMerge);
        pr.title = "Fix the login redirect loop".into();
        pr.branch = Some("conductor/pr-02".into());
        pr.pr_url = Some("https://github.com/erd/web/pull/123".into());
        repo.write_task(&pr);
        repo.append("pr-02", &fixture("pr_lifecycle.jsonl")[..7]);
        let cards = cards(&repo);
        for width in [40, 60] {
            let backend = render(
                width,
                14,
                &cards_view(&cards, None),
                &mut Selection::default(),
            );
            assert_snapshot!(format!("needs_permission_and_pr_line_{width}"), backend);
        }
    }

    #[test]
    fn empty_and_error_states() {
        let no_conductor = View {
            repo: None,
            body: Body::NoConductor {
                reason: "/src/web has no conductor record",
            },
            now: ts(NOW),
            show_hidden: false,
        };
        let unreachable = View {
            body: Body::HerdrUnreachable {
                error: "herdr socket: Connection refused (os error 61)",
            },
            ..no_conductor
        };
        let views = [
            ("no_tasks", cards_view(&[], None)),
            (
                "no_tasks_with_problem",
                cards_view(&[], Some("herdr socket: Broken pipe")),
            ),
            ("no_conductor", no_conductor),
            ("herdr_unreachable", unreachable),
        ];
        for (name, view) in views {
            for width in [40, 60] {
                let backend = render(width, 8, &view, &mut Selection::default());
                assert_snapshot!(format!("{name}_{width}"), backend);
            }
        }
    }

    #[test]
    fn overflow_scrolls_to_the_selection() {
        let repo = TempRepo::new();
        for n in 1..=8 {
            let prompt = event(&format!("12:0{n}:00"), r#""kind":"prompt","text":"go""#);
            add(
                &repo,
                &format!("task-{n:02}"),
                Phase::Working,
                &format!("Task number {n}"),
                &[prompt],
            );
        }
        let cards = cards(&repo);
        let ids: Vec<&str> = cards.iter().map(|card| card.task.id.as_str()).collect();
        for width in [40, 60] {
            let mut selection = Selection::default();
            let view = cards_view(&cards, None);
            let top = render(width, 18, &view, &mut selection);
            assert_snapshot!(format!("overflow_top_{width}"), top);
            for _ in 0..5 {
                selection.move_by(&ids, 1);
            }
            let scrolled = render(width, 18, &view, &mut selection);
            assert_snapshot!(format!("overflow_scrolled_{width}"), scrolled);
        }
    }

    #[test]
    fn selection_is_thick_and_badges_are_colored_symbols() {
        let cards = every_phase();
        let backend = render(40, 64, &cards_view(&cards, None), &mut Selection::default());
        let buffer = backend.buffer();
        // The first card is selected; the second isn't.
        assert_eq!(buffer[(0, 1)].symbol(), "┏");
        assert_eq!(buffer[(0, 6)].symbol(), "┌");
        let failed = cards
            .iter()
            .position(|card| card.task.phase == Phase::Failed);
        assert_eq!(failed, Some(0));
        assert_eq!(buffer[(1, 2)].symbol(), "✗");
        assert_eq!(buffer[(1, 2)].fg, Color::Red);
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    fn mouse(kind: MouseEventKind) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        })
    }

    #[test]
    fn maps_keys_and_the_wheel() {
        let none = KeyModifiers::NONE;
        let cases = [
            (key(KeyCode::Char('q'), none), Some(Action::Quit)),
            (
                key(KeyCode::Char('c'), KeyModifiers::CONTROL),
                Some(Action::Quit),
            ),
            (key(KeyCode::Char('c'), none), None),
            (key(KeyCode::Enter, none), None),
            (key(KeyCode::Char('j'), none), Some(Action::Down)),
            (key(KeyCode::Down, none), Some(Action::Down)),
            (key(KeyCode::Char('k'), none), Some(Action::Up)),
            (key(KeyCode::Up, none), Some(Action::Up)),
            (
                key(KeyCode::Char('L'), KeyModifiers::SHIFT),
                Some(Action::ToggleHidden),
            ),
            (mouse(MouseEventKind::ScrollDown), Some(Action::Down)),
            (mouse(MouseEventKind::ScrollUp), Some(Action::Up)),
            (mouse(MouseEventKind::Moved), None),
        ];
        for (event, expected) in cases {
            assert_eq!(action(&event), expected, "{event:?}");
        }
    }

    #[test]
    fn ignores_key_releases() {
        let mut release = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert_eq!(action(&Event::Key(release)), None);
    }
}
