//! The deck's screen: a header with counts, the cards stacked in sections, and a footer with
//! key hints.

mod card;
mod decision;
mod glyphs;
mod input;
mod list;

use conductor_state::DeckIcons;
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use jiff::Timestamp;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Style, Stylize as _};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget as _, Wrap};
use unicode_width::{UnicodeWidthChar as _, UnicodeWidthStr as _};

use crate::deck::CardModel;
use crate::deck::card::{Section, State};
pub use decision::{Answer, DecisionModal, Outcome as DecisionOutcome};
use glyphs::Glyphs;
pub use input::{Input, Outcome as InputOutcome};
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
    /// Whether the Done section is folded to its rule.
    pub fold_done: bool,
    /// The glyphs to draw with.
    pub icons: DeckIcons,
    /// A transient message above the footer.
    pub toast: Option<&'a str>,
    /// The message being typed, which replaces the footer while `m` is open.
    pub input: Option<&'a str>,
    /// The decision being answered, drawn over the cards while `d` is open.
    pub decision: Option<&'a DecisionModal>,
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
    /// Fold or unfold the Done section.
    FoldDone,
    /// Jump to the selected card's worker.
    Focus,
    /// Type a message to the selected card's worker.
    Message,
    /// Answer the selected card's open decisions.
    Decide,
    OpenPr,
    FocusConductor,
    /// A left click on screen row `row`.
    Click {
        row: u16,
    },
}

pub fn draw(frame: &mut Frame, view: &View, selection: &mut Selection) {
    let problem = match view.body {
        Body::Cards { problem, .. } => problem,
        Body::NoConductor { .. } | Body::HerdrUnreachable { .. } => None,
    };
    let toast = view.toast.map(|text| {
        Paragraph::new(text.to_owned())
            .yellow()
            .wrap(Wrap { trim: true })
    });
    let toast_height = toast.as_ref().map_or(0, |toast| {
        u16::try_from(toast.line_count(frame.area().width)).unwrap_or(u16::MAX)
    });
    let [header, body, problem_area, toast_area, footer_rule, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Fill(1),
        Constraint::Length(u16::from(problem.is_some())),
        Constraint::Length(toast_height),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    selection.clear_drawn();
    let buf = frame.buffer_mut();
    draw_header(view, header, buf);
    Line::from("─".repeat(usize::from(footer_rule.width)))
        .dim()
        .render(footer_rule, buf);
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
        Body::Cards { cards, .. } => draw_cards(cards, view, selection, body, buf),
    };
    let modal_cursor = view
        .decision
        .and_then(|modal| decision::render(modal, body, buf));
    if let Some(problem) = problem {
        let text = format!("{} {problem}", card::ATTENTION);
        let line = card::fit(vec![Span::raw(text)], usize::from(problem_area.width));
        line.red().render(problem_area, buf);
    }
    if let Some(toast) = toast {
        toast.render(toast_area, buf);
    }
    match (view.decision, view.input) {
        (Some(modal), _) => {
            let hints = decision_hints(modal);
            card::fit(vec![Span::raw(hints)], usize::from(footer.width))
                .dim()
                .render(footer, buf);
            if let Some(cursor) = modal_cursor {
                frame.set_cursor_position(cursor);
            }
        }
        (None, Some(text)) => {
            let cursor = draw_input(Span::raw("› ").bold(), text, footer, buf);
            frame.set_cursor_position(cursor);
        }
        (None, None) => draw_footer(view, position, footer, buf),
    }
}

/// The repo name on the left and, with cards, how many are in each open section on the right,
/// over a heavy rule. With Nerd Font icons the name sits in a powerline segment over a light
/// rule.
fn draw_header(view: &View, area: Rect, buf: &mut Buffer) {
    let nerd = view.icons == DeckIcons::Nerd;
    let [top, rule] = Layout::vertical([Constraint::Length(1); 2]).areas(area);
    let counts = match view.body {
        Body::Cards { cards, .. } => {
            let mut spans = Vec::new();
            for (section, state) in [
                (Section::NeedsYou, State::NeedsYou),
                (Section::Review, State::Review),
                (Section::InProgress, State::InProgress),
                (Section::Todo, State::Pending),
            ] {
                let badge = card::badge(state, view.icons);
                let count = cards.iter().filter(|c| c.section() == section).count();
                let text = if nerd {
                    format!(" {} {count} ", badge.symbol)
                } else {
                    format!("{}{count} ", badge.symbol)
                };
                spans.push(Span::styled(text, badge.style));
            }
            Line::from(spans)
        }
        Body::NoConductor { .. } | Body::HerdrUnreachable { .. } => Line::default(),
    };
    let counts_width = u16::try_from(counts.width()).unwrap_or(u16::MAX);
    let [name, right] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(counts_width)]).areas(top);
    let repo = view.repo.unwrap_or("conductor");
    if nerd {
        let icon = format!(" {} ", glyphs::TERMINAL);
        // The segment's padding and its end.
        let room = usize::from(name.width).saturating_sub(icon.width() + 3);
        Line::from(vec![
            Span::raw(format!("{icon}{} ", card::truncate(repo, room)))
                .reversed()
                .bold(),
            Span::raw(glyphs::SEGMENT_END),
        ])
        .render(name, buf);
    } else {
        let label = " TASKS · ";
        let room = usize::from(name.width).saturating_sub(label.width() + 1);
        Line::from(format!("{label}{}", card::truncate(repo, room)))
            .bold()
            .render(name, buf);
    }
    counts.render(right, buf);
    if nerd {
        Line::from("─".repeat(usize::from(rule.width))).render(rule, buf);
    } else {
        Line::from("━".repeat(usize::from(rule.width)))
            .bold()
            .render(rule, buf);
    }
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

/// The cards that can be selected: all of them, or those above Done when it's folded. Done
/// sorts last, so that's a prefix.
pub fn shown(cards: &[CardModel], fold_done: bool) -> &[CardModel] {
    let end = if fold_done {
        (cards.iter())
            .position(|card| card.section() == Section::Done)
            .unwrap_or(cards.len())
    } else {
        cards.len()
    };
    &cards[..end]
}

/// Draws the cards that fit, each section under its rule, scrolled to the selection, and
/// returns the selected card's index.
fn draw_cards(
    all: &[CardModel],
    view: &View,
    selection: &mut Selection,
    area: Rect,
    buf: &mut Buffer,
) -> Option<usize> {
    let cards = shown(all, view.fold_done);
    let count = |section| all.iter().filter(|card| card.section() == section).count();
    let folded = cards.len() < all.len();
    let rule =
        |section: Section| section_rule(section, count(section), folded, view.icons, area.width);
    let starts_section =
        |index: usize| index == 0 || cards[index - 1].section() != cards[index].section();
    let last = cards.len().checked_sub(1);
    // The rows each card takes in the stack: its section's rule above the first card, and the
    // folded Done rule below the last.
    let heights: Vec<u16> = (0..cards.len())
        .map(|index| {
            u16::from(starts_section(index))
                + card::height(&cards[index], area.width)
                + u16::from(folded && Some(index) == last)
        })
        .collect();
    let ids: Vec<&str> = cards.iter().map(|card| card.task.id.as_str()).collect();
    let Some(selected) = selection.resolve(&ids) else {
        rule(Section::Done).render(Rect { height: 1, ..area }, buf);
        return None;
    };
    let mut y = area.y;
    let row = |y| Rect {
        y,
        height: 1,
        ..area
    };
    for index in selection.scroll(&heights, area.height) {
        let card = &cards[index];
        if starts_section(index) && y < area.bottom() {
            rule(card.section()).render(row(y), buf);
            y += 1;
        }
        let height = card::height(card, area.width).min(area.bottom() - y);
        if height == 0 {
            break;
        }
        let rect = Rect { y, height, ..area };
        card::render(card, view.now, index == selected, view.icons, rect, buf);
        selection.drew(y..y + height, index);
        y += height;
        if folded && Some(index) == last && y < area.bottom() {
            rule(Section::Done).render(row(y), buf);
            y += 1;
        }
    }
    Some(selected)
}

/// ` IN PROGRESS ───── 2`, with `▾` or `▸` after Done's count.
fn section_rule(
    section: Section,
    count: usize,
    folded: bool,
    icons: DeckIcons,
    width: u16,
) -> Line<'static> {
    let label = match section {
        Section::NeedsYou => "NEEDS YOU",
        Section::InProgress => "IN PROGRESS",
        Section::Review => "REVIEW",
        Section::Todo => "TODO",
        Section::Done => "DONE",
    };
    let glyphs = Glyphs::of(icons);
    if icons == DeckIcons::Nerd {
        return section_segment(section, label, count, folded, glyphs, width);
    }
    let label_style = if section == Section::NeedsYou {
        Style::new().magenta().bold()
    } else {
        Style::new().dim()
    };
    let tail = match section {
        Section::Done if folded => format!(" {count} {} ", glyphs.folded),
        Section::Done => format!(" {count} {} ", glyphs.unfolded),
        _ => format!(" {count} "),
    };
    let rule = usize::from(width).saturating_sub(label.width() + tail.width() + 2);
    let line = Line::from(vec![
        Span::raw(" "),
        Span::styled(label, label_style),
        Span::raw(" ").dim(),
        Span::raw("─".repeat(rule)).dim(),
        Span::raw(tail).dim(),
    ]);
    card::fit(line.spans, usize::from(width))
}

/// ` IN PROGRESS  2 ─────` as a powerline segment, with a chevron after Done's rule.
fn section_segment(
    section: Section,
    label: &str,
    count: usize,
    folded: bool,
    glyphs: &Glyphs,
    width: u16,
) -> Line<'static> {
    let color = if section == Section::NeedsYou {
        Color::Magenta
    } else {
        Color::DarkGray
    };
    let mut segment = Style::new().bg(color).bold();
    if section == Section::NeedsYou {
        segment = segment.fg(Color::Black);
    }
    let head = format!(" {count} ");
    let tail = match section {
        Section::Done if folded => format!(" {} ", glyphs.folded),
        Section::Done => format!(" {} ", glyphs.unfolded),
        _ => " ".to_owned(),
    };
    let rule = usize::from(width).saturating_sub(label.width() + head.width() + tail.width() + 3);
    let line = Line::from(vec![
        Span::styled(format!(" {label} "), segment),
        Span::styled(glyphs::SEGMENT_END, Style::new().fg(color)),
        Span::raw(head),
        Span::raw("─".repeat(rule)),
        Span::raw(tail),
    ]);
    card::fit(line.spans, usize::from(width))
}

/// An input line: `prompt` and as much of the end of `text` as fits. Returns where the
/// cursor goes.
fn draw_input(prompt: Span<'_>, text: &str, area: Rect, buf: &mut Buffer) -> Position {
    // Leave a column for the cursor after the text.
    let room = usize::from(area.width).saturating_sub(prompt.width() + 1);
    let mut width = 0;
    let start = text
        .char_indices()
        .rev()
        .take_while(|(_, c)| {
            width += c.width().unwrap_or(0);
            width <= room
        })
        .last()
        .map_or(text.len(), |(index, _)| index);
    let shown = &text[start..];
    let line = Line::from(vec![card::owned(prompt), Span::raw(shown.to_owned())]);
    let cursor_x = u16::try_from(line.width()).unwrap_or(u16::MAX);
    line.render(area, buf);
    Position::new(
        (area.x + cursor_x).min(area.right().saturating_sub(1)),
        area.y,
    )
}

/// Key hints on the left; the selection's position on the right.
fn draw_footer(view: &View, position: Option<usize>, area: Rect, buf: &mut Buffer) {
    let position = match (position, view.body) {
        (Some(index), Body::Cards { cards, .. }) => {
            format!("{}/{}", index + 1, shown(cards, view.fold_done).len())
        }
        _ => String::new(),
    };
    let position_width = u16::try_from(position.len()).unwrap_or(u16::MAX);
    let [_, left, _, right] = Layout::horizontal([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(u16::from(position_width > 0)),
        Constraint::Length(position_width),
    ])
    .areas(area);
    let hints = match view.body {
        Body::Cards { .. } => card_hints(view.show_hidden, view.fold_done, usize::from(left.width)),
        Body::NoConductor { .. } | Body::HerdrUnreachable { .. } => "q quit".to_owned(),
    };
    card::fit(vec![Span::raw(hints)], usize::from(left.width))
        .dim()
        .render(left, buf);
    Line::from(position).dim().render(right, buf);
}

/// As many key hints as fit in `width`, most useful first, always ending with `q quit`.
fn card_hints(show_hidden: bool, fold_done: bool, width: usize) -> String {
    const QUIT: &str = "q quit";
    let fold = if fold_done {
        "z show done"
    } else {
        "z fold done"
    };
    let toggle = if show_hidden {
        "L hide done"
    } else {
        "L show all"
    };
    let mut hints = String::new();
    for hint in [
        "⏎ focus",
        "m msg",
        "d decide",
        "o PR",
        "c conductor",
        fold,
        toggle,
        "j/k move",
    ] {
        let wider = format!("{hints}{hint} · ");
        if wider.width() + QUIT.len() > width {
            break;
        }
        hints = wider;
    }
    hints + QUIT
}

/// The decision modal's keys, for the footer while it's open.
fn decision_hints(modal: &DecisionModal) -> String {
    let mut hints = vec!["⏎ answer"];
    if modal.typing() {
        hints.push("tab options");
    } else {
        hints.push("tab note");
        if modal.several() {
            hints.push("n/p next");
        }
    }
    hints.push("esc cancel");
    hints.join(" · ")
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
            KeyCode::Char('z') => Some(Action::FoldDone),
            KeyCode::Enter => Some(Action::Focus),
            KeyCode::Char('m') => Some(Action::Message),
            KeyCode::Char('d') => Some(Action::Decide),
            KeyCode::Char('o') => Some(Action::OpenPr),
            KeyCode::Char('c') => Some(Action::FocusConductor),
            _ => None,
        },
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollDown => Some(Action::Down),
            MouseEventKind::ScrollUp => Some(Action::Up),
            MouseEventKind::Down(MouseButton::Left) => Some(Action::Click { row: mouse.row }),
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
    use conductor_state::{Decision, Phase};

    use crate::deck::testing::{REPO_ID, TempRepo, decision, event, fixture, task, ts};
    use crate::deck::{AgentInfo, Deck};
    use crate::herdr::types::AgentStatus;

    const NOW: &str = "12:34:56";

    /// Each set of glyphs, and the suffix its snapshots take.
    const ICONS: [(DeckIcons, &str); 2] = [(DeckIcons::Unicode, ""), (DeckIcons::Nerd, "_nerd")];

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
            fold_done: false,
            icons: DeckIcons::Unicode,
            toast: None,
            input: None,
            decision: None,
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
        for (icons, suffix) in ICONS {
            for width in [40, 60] {
                let view = View {
                    icons,
                    ..cards_view(&cards, None)
                };
                let backend = render(width, 64, &view, &mut Selection::default());
                assert_snapshot!(format!("one_card_per_phase{suffix}_{width}"), backend);
            }
        }
    }

    #[test]
    fn sections_and_folded_done() {
        let repo = TempRepo::new();
        let prompt = event("12:00:00", r#""kind":"prompt","text":"go""#);
        add(
            &repo,
            "work-01",
            Phase::Working,
            "Fix the login redirect",
            &[prompt],
        );
        add(
            &repo,
            "brief-02",
            Phase::Briefed,
            "Brief the export job",
            &[],
        );
        add(
            &repo,
            "merged-03",
            Phase::Merged,
            "Rename the config file",
            &[],
        );
        add(&repo, "merged-04", Phase::Merged, "Bump dependencies", &[]);
        let cards = cards(&repo);
        let ids: Vec<&str> = cards.iter().map(|card| card.task.id.as_str()).collect();
        for (icons, suffix) in ICONS {
            for width in [40, 60] {
                // Done open, with a done row selected.
                let mut selection = Selection::default();
                selection.move_by(&ids, 2);
                let view = View {
                    icons,
                    ..cards_view(&cards, None)
                };
                let open = render(width, 20, &view, &mut selection);
                assert_snapshot!(format!("sections_done_open{suffix}_{width}"), open);

                // Folded, the selection falls back to the last card still shown.
                let folded = View {
                    fold_done: true,
                    ..view
                };
                let shown: Vec<&str> = shown(&cards, true)
                    .iter()
                    .map(|card| card.task.id.as_str())
                    .collect();
                assert_eq!(shown, ["work-01", "brief-02"]);
                assert_eq!(selection.resolve(&shown), Some(1));
                let backend = render(width, 20, &folded, &mut selection);
                assert_snapshot!(format!("sections_done_folded{suffix}_{width}"), backend);
            }
        }
        // Only done cards, folded: just the rule.
        let done = &cards[2..];
        let view = View {
            fold_done: true,
            ..cards_view(done, None)
        };
        let mut selection = Selection::default();
        let backend = render(40, 8, &view, &mut selection);
        assert_snapshot!("sections_only_done_folded", backend);
        assert_eq!(selection.card_at(2), None);
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
        for (icons, suffix) in ICONS {
            for width in [40, 60] {
                let view = View {
                    icons,
                    ..cards_view(&cards, None)
                };
                let backend = render(width, 14, &view, &mut Selection::default());
                assert_snapshot!(
                    format!("needs_permission_and_pr_line{suffix}_{width}"),
                    backend
                );
            }
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
            fold_done: false,
            icons: DeckIcons::Unicode,
            toast: None,
            input: None,
            decision: None,
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
        // Under the header and the NEEDS YOU rule, the first card is selected; the second
        // isn't, but needs you, so its border is magenta.
        assert_eq!(buffer[(0, 3)].symbol(), "┏");
        assert_eq!(buffer[(0, 3)].fg, Color::White);
        let failed = cards
            .iter()
            .position(|card| card.task.phase == Phase::Failed);
        assert_eq!(failed, Some(0));
        assert_eq!(buffer[(2, 4)].symbol(), "✗");
        assert_eq!(buffer[(2, 4)].fg, Color::Red);
        assert_eq!(buffer[(0, 8)].symbol(), "╭");
        assert_eq!(buffer[(0, 8)].fg, Color::Magenta);
        assert_eq!(buffer[(2, 9)].symbol(), "⚑");
        assert_eq!(buffer[(2, 9)].fg, Color::Magenta);
    }

    #[test]
    fn toast_wraps_above_the_input_line() {
        let cards = every_phase();
        let input = "rebase onto main first, then rerun the integration tests";
        let view = View {
            toast: Some("worker is waiting on a prompt; press Enter to focus it"),
            input: Some(input),
            ..cards_view(&cards[..2], None)
        };
        for width in [40, 60] {
            let mut terminal = Terminal::new(TestBackend::new(width, 14)).unwrap();
            terminal
                .draw(|frame| draw(frame, &view, &mut Selection::default()))
                .unwrap();
            // The cursor sits just after the text, or at the edge once it scrolls.
            let after_text = u16::try_from(2 + input.len()).unwrap();
            let cursor = terminal.get_cursor_position().unwrap();
            assert_eq!(cursor, Position::new(after_text.min(width - 1), 13));
            assert_snapshot!(
                format!("toast_and_input_{width}"),
                terminal.backend().clone()
            );
        }
        let short = View {
            input: Some("hi"),
            ..view
        };
        let mut terminal = Terminal::new(TestBackend::new(40, 14)).unwrap();
        terminal
            .draw(|frame| draw(frame, &short, &mut Selection::default()))
            .unwrap();
        assert_eq!(
            terminal.get_cursor_position().unwrap(),
            Position::new(4, 13)
        );
    }

    fn auth_flow() -> Decision {
        let mut auth = decision(
            "auth-flow",
            &[
                ("refresh", "Refresh tokens", Some("rotate on every request")),
                ("sliding", "Sliding sessions", Some("extend on activity")),
                ("relogin", "Force a new login", None),
            ],
        );
        auth.title = "How should expired sessions recover?".into();
        auth.about = "Sessions expire mid-checkout and the redirect loops.".into();
        auth.recommend = Some("refresh".into());
        auth
    }

    fn press(modal: &mut DecisionModal, code: KeyCode) {
        modal.handle(&KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn decision_modal_with_a_note() {
        let cards = every_phase();
        let mut auth = auth_flow();
        auth.allow_freeform = true;
        let engine = decision("db-engine", &[("pg", "Postgres", None)]);
        let mut modal =
            DecisionModal::new("fix-login-07".into(), None, vec![auth, engine]).unwrap();
        press(&mut modal, KeyCode::Down);
        press(&mut modal, KeyCode::Tab);
        for c in "keep it behind a flag".chars() {
            press(&mut modal, KeyCode::Char(c));
        }
        let view = View {
            decision: Some(&modal),
            ..cards_view(&cards, None)
        };
        for width in [40, 60] {
            let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
            terminal
                .draw(|frame| draw(frame, &view, &mut Selection::default()))
                .unwrap();
            // The note is the modal's last row, above its bottom border.
            let note_x = u16::try_from(1 + "note › keep it behind a flag".width()).unwrap();
            let cursor = terminal.get_cursor_position().unwrap();
            assert_eq!(cursor.x, note_x.min(width - 2));
            assert_eq!(terminal.backend().buffer()[(0, cursor.y + 1)].symbol(), "┗");
            assert_snapshot!(
                format!("decision_modal_with_a_note_{width}"),
                terminal.backend().clone()
            );
        }
    }

    #[test]
    fn decision_modal_choosing() {
        let cards = every_phase();
        let modal = DecisionModal::new("fix-login-07".into(), None, vec![auth_flow()]).unwrap();
        let view = View {
            decision: Some(&modal),
            ..cards_view(&cards, None)
        };
        for width in [40, 60] {
            let backend = render(width, 20, &view, &mut Selection::default());
            assert_snapshot!(format!("decision_modal_choosing_{width}"), backend);
        }
    }

    #[test]
    fn records_where_each_card_was_drawn() {
        let cards = every_phase();
        let mut selection = Selection::default();
        render(40, 20, &cards_view(&cards, None), &mut selection);
        // The header takes rows 0 and 1 and the NEEDS YOU rule row 2; the first two cards
        // take five rows each, then comes the REVIEW rule.
        assert_eq!(selection.card_at(2), None);
        assert_eq!(selection.card_at(3), Some(0));
        assert_eq!(selection.card_at(7), Some(0));
        assert_eq!(selection.card_at(8), Some(1));
        assert_eq!(selection.card_at(12), Some(1));
        assert_eq!(selection.card_at(13), None);

        render(40, 20, &cards_view(&[], None), &mut selection);
        assert_eq!(selection.card_at(3), None);
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    fn mouse(kind: MouseEventKind) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column: 0,
            row: 7,
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
            (key(KeyCode::Char('c'), none), Some(Action::FocusConductor)),
            (key(KeyCode::Enter, none), Some(Action::Focus)),
            (key(KeyCode::Char('m'), none), Some(Action::Message)),
            (key(KeyCode::Char('d'), none), Some(Action::Decide)),
            (key(KeyCode::Char('o'), none), Some(Action::OpenPr)),
            (key(KeyCode::Char('z'), none), Some(Action::FoldDone)),
            (key(KeyCode::Char('x'), none), None),
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
            (
                mouse(MouseEventKind::Down(MouseButton::Left)),
                Some(Action::Click { row: 7 }),
            ),
            (mouse(MouseEventKind::Down(MouseButton::Right)), None),
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
