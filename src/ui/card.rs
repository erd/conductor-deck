//! One card: a bordered block titled with the task id, and the text helpers behind its lines.

use conductor_state::{
    Activity, CheckState, DeckIcons, Phase, PrState, ReviewDecision, StatusVerb,
};
use jiff::{SignedDuration, Timestamp};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style, Stylize as _};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Padding, Paragraph, Widget as _};
use unicode_width::{UnicodeWidthChar as _, UnicodeWidthStr as _};

use crate::deck::CardModel;
use crate::deck::card::{Attention, State};
use crate::ui::glyphs::{self, Glyphs};

/// A state badge. The symbol carries the meaning; the color only backs it up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Badge {
    pub symbol: &'static str,
    pub style: Style,
}

pub fn badge(state: State, icons: DeckIcons) -> Badge {
    let glyphs = Glyphs::of(icons);
    let (symbol, style) = match state {
        State::NeedsYou => (glyphs.needs_you, Style::new().magenta().bold()),
        State::Failed => (glyphs.failed, Style::new().red().bold()),
        State::InProgress => (glyphs.in_progress, Style::new().yellow().bold()),
        State::Review => (glyphs.review, Style::new().cyan()),
        State::ReadyToMerge => (glyphs.ready_to_merge, Style::new().blue()),
        State::Pending => (glyphs.pending, Style::new().dim()),
        State::Done => (glyphs.done, Style::new().green()),
    };
    Badge { symbol, style }
}

/// The card's own badge: dim while its worker sits idle, and dim for tasks that ended
/// without merging.
fn card_badge(card: &CardModel, icons: DeckIcons) -> Badge {
    let badge = badge(card.state, icons);
    match (card.state, card.task.phase) {
        (State::Done, Phase::Abandoned) => Badge {
            symbol: Glyphs::of(icons).failed,
            style: Style::new().dim(),
        },
        (State::Done, Phase::TornDown) => Badge {
            style: Style::new().dim(),
            ..badge
        },
        (State::InProgress, _) if !card.working() => Badge {
            style: Style::new().yellow().dim(),
            ..badge
        },
        _ => badge,
    }
}

/// Marks problems, in the problem line and the herdr-unreachable message.
pub const ATTENTION: &str = "!";

/// The most rows a card's title wraps to.
const TITLE_LINES: usize = 3;

/// Rows the card takes at `width` columns, borders included. Done cards are a single row.
/// Nothing the worker reports changes it, so cards don't jump around while workers run.
pub fn height(card: &CardModel, width: u16) -> u16 {
    if card.state == State::Done {
        return 1;
    }
    let title = wrap(
        &card.task.title,
        usize::from(width.saturating_sub(4)),
        TITLE_LINES,
    );
    let title = u16::try_from(title.len().max(1)).unwrap_or(1);
    3 + title + u16::from(has_context(card)) + u16::from(has_pr(card))
}

pub fn render(
    card: &CardModel,
    now: Timestamp,
    selected: bool,
    icons: DeckIcons,
    area: Rect,
    buf: &mut Buffer,
) {
    if card.state == State::Done {
        render_row(card, now, selected, icons, area, buf);
        return;
    }
    let mut border = Style::new();
    if card.state == State::NeedsYou {
        border = border.magenta();
    } else if selected {
        border = border.fg(Color::White);
    }
    let (border_type, rule) = if selected {
        border = border.bold();
        (BorderType::Thick, "━")
    } else {
        (BorderType::Rounded, "─")
    };
    let id = truncate(&card.task.id, usize::from(area.width.saturating_sub(5)));
    let block = Block::bordered()
        .border_type(border_type)
        .border_style(border)
        .padding(Padding::horizontal(1))
        .title(Line::from(vec![
            Span::styled(rule, border),
            Span::raw(format!(" {id} ")),
        ]));
    let width = usize::from(area.width.saturating_sub(4));
    let title = if selected {
        Style::new().bold()
    } else {
        Style::new()
    };
    let mut lines = vec![state_line(card, now, icons, width)];
    lines.extend(
        wrap(&card.task.title, width, TITLE_LINES)
            .into_iter()
            .map(|line| Line::styled(line, title)),
    );
    if has_context(card) {
        lines.push(context_line(card, icons, width));
    }
    if has_pr(card) {
        lines.push(pr_line(card, icons, width));
    }
    Paragraph::new(lines).block(block).render(area, buf);
}

/// A done task on one row: badge, id and title on the left, the time on the right.
fn render_row(
    card: &CardModel,
    now: Timestamp,
    selected: bool,
    icons: DeckIcons,
    area: Rect,
    buf: &mut Buffer,
) {
    let badge = card_badge(card, icons);
    let elapsed = elapsed(card.elapsed(now));
    let width = usize::from(area.width);
    let text = if selected {
        Style::new().bold()
    } else {
        Style::new().dim()
    };
    // A gap before the time and a space after it.
    let room = width.saturating_sub(elapsed.width() + 2);
    let mut line = fit(
        vec![
            Span::styled(if selected { "›" } else { " " }, text),
            Span::styled(badge.symbol, badge.style),
            Span::raw(" "),
            Span::styled(card.task.id.as_str(), text),
            Span::raw(" "),
            Span::styled(card.task.title.as_str(), text),
        ],
        room,
    );
    let gap = width.saturating_sub(line.width() + elapsed.width() + 1);
    line.push_span(" ".repeat(gap));
    line.push_span(Span::styled(elapsed, text));
    line.render(area, buf);
}

/// The badge, the state word and what the worker is on, on the left, and the time in state on
/// the right; the detail gives way first. With Nerd Font icons, a card that's moving or needs
/// you wears its badge and state word as a pill.
fn state_line(card: &CardModel, now: Timestamp, icons: DeckIcons, width: usize) -> Line<'static> {
    let badge = card_badge(card, icons);
    let elapsed = format!(
        "{}{}",
        Glyphs::of(icons).elapsed,
        elapsed(card.elapsed(now))
    );
    let room = width.saturating_sub(elapsed.width() + 1);
    let mut spans = if icons == DeckIcons::Nerd && pill(card) {
        let cap = Style::new().fg(badge.style.fg.unwrap_or(Color::Reset));
        vec![
            Span::styled(glyphs::PILL_OPEN, cap),
            Span::styled(
                format!(" {} {}", badge.symbol, state_word(card)),
                badge.style.reversed(),
            ),
            Span::styled(glyphs::PILL_CLOSE, cap),
        ]
    } else {
        let text = format!("{} {}", badge.symbol, state_word(card));
        vec![Span::styled(text, badge.style)]
    };
    if let Some(detail) = detail(card) {
        spans.extend([Span::raw(" · ").dim(), Span::raw(detail)]);
    }
    let mut line = fit(spans, room);
    let gap = width.saturating_sub(line.width() + elapsed.width());
    line.push_span(" ".repeat(gap));
    line.push_span(Span::raw(elapsed).dim());
    line
}

/// Whether the card is moving or needs you: live states, but not an idle worker.
fn pill(card: &CardModel) -> bool {
    match card.state {
        State::NeedsYou | State::Failed | State::Review | State::ReadyToMerge => true,
        State::InProgress => card.working(),
        State::Pending | State::Done => false,
    }
}

/// The state, in words, most pressing reason first.
fn state_word(card: &CardModel) -> &'static str {
    let has = |reason| card.attention.contains(&reason);
    match card.state {
        State::NeedsYou if has(Attention::Permission) => "needs permission",
        State::NeedsYou if has(Attention::Decision) => "needs decision",
        State::NeedsYou if has(Attention::Blocked) => "blocked",
        State::NeedsYou => "changes requested",
        State::Failed if has(Attention::ChecksFailing) && !has(Attention::Failed) => {
            "checks failing"
        }
        State::Failed => "failed",
        State::InProgress if card.working() => "working",
        State::InProgress if matches!(card.live.activity, Activity::Exited { .. }) => "exited",
        State::InProgress => "idle",
        State::Review => "in review",
        State::ReadyToMerge => "ready to merge",
        State::Pending => match card.task.phase {
            Phase::Intake => "intake",
            Phase::Briefed => "briefed",
            _ => "spawning",
        },
        State::Done => match card.task.phase {
            Phase::TornDown => "torn down",
            Phase::Abandoned => "abandoned",
            _ => "merged",
        },
    }
}

/// What the worker is doing, or what it's stuck on.
fn detail(card: &CardModel) -> Option<String> {
    let live = &card.live;
    if card.attention.contains(&Attention::Permission) {
        return match &live.activity {
            Activity::NeedsPermission { message } if !message.is_empty() => Some(message.clone()),
            _ => None,
        };
    }
    if let Some(first) = live.open_decisions.iter().next() {
        return Some(match live.open_decisions.len() - 1 {
            0 => first.clone(),
            more => format!("{first} +{more}"),
        });
    }
    for (reason, verb) in [
        (Attention::Blocked, StatusVerb::Blocked),
        (Attention::Failed, StatusVerb::Failed),
    ] {
        if card.attention.contains(&reason) {
            let text = live
                .status
                .as_ref()
                .filter(|status| status.verb == verb)
                .and_then(|status| status.text.clone());
            if text.is_some() {
                return text;
            }
        }
    }
    match &live.activity {
        Activity::Working { tool: Some(tool) } => Some(match &tool.summary {
            Some(summary) => format!("{} {summary}", tool.name),
            None => tool.name.clone(),
        }),
        Activity::Idle { error: Some(error) } => Some(error.clone()),
        _ => None,
    }
}

/// Whether the task has a worker yet: a branch, or an agent herdr reports on.
fn has_context(card: &CardModel) -> bool {
    card.task.branch.is_some() || card.agent.is_some()
}

/// `ctx 26% · <branch>`.
fn context_line(card: &CardModel, icons: DeckIcons, width: usize) -> Line<'static> {
    let glyphs = Glyphs::of(icons);
    let context = card
        .agent
        .as_ref()
        .and_then(|agent| agent.context.as_deref())
        .and_then(context_percent)
        .unwrap_or("–");
    let mut spans = vec![Span::raw(format!("{}{context}", glyphs.context))];
    if let Some(branch) = &card.task.branch {
        spans.push(Span::raw(" · ").dim());
        spans.push(Span::raw(format!("{}{branch}", glyphs.branch)));
    }
    fit(spans, width)
}

/// `PR #123 ready · checks ✓ · approved`; the parts herdr or GitHub haven't reported are left out.
fn pr_line(card: &CardModel, icons: DeckIcons, width: usize) -> Line<'static> {
    let glyphs = Glyphs::of(icons);
    let number = card.pr_url().and_then(pr_number);
    let mut spans = vec![Span::raw(match number {
        Some(number) => format!("{} #{number}", glyphs.pr),
        None => glyphs.pr.to_owned(),
    })];
    let pr = card.live.pr;
    if let Some(pr) = pr {
        let state = match pr.state {
            PrState::Draft => "draft",
            PrState::Open => "ready",
            PrState::Merged => "merged",
            PrState::Closed => "closed",
        };
        spans.push(Span::raw(format!(" {state}")));
    }
    let separator = || Span::raw(" · ").dim();
    if let Some(checks) = pr.and_then(|pr| pr.checks) {
        let (symbol, style) = match checks {
            CheckState::Pass => (glyphs.done, Style::new().fg(Color::Green)),
            CheckState::Fail => (glyphs.failed, Style::new().fg(Color::Red).bold()),
            CheckState::Pending => ("…", Style::new().dim()),
        };
        spans.extend([
            separator(),
            Span::raw("checks "),
            Span::styled(symbol, style),
        ]);
    }
    if let Some(review) = pr.and_then(|pr| pr.review) {
        let (text, style) = match review {
            ReviewDecision::Approved => ("approved", Style::new().fg(Color::Green)),
            ReviewDecision::ChangesRequested => {
                ("changes requested", Style::new().fg(Color::Red).bold())
            }
            ReviewDecision::ReviewRequired => ("review required", Style::new()),
        };
        spans.extend([separator(), Span::styled(text, style)]);
    }
    fit(spans, width)
}

fn has_pr(card: &CardModel) -> bool {
    card.live.pr.is_some() || card.pr_url().is_some()
}

/// The number at the end of a GitHub PR URL.
pub fn pr_number(url: &str) -> Option<&str> {
    let (_, number) = url.trim_end_matches('/').rsplit_once("/pull/")?;
    (!number.is_empty() && number.bytes().all(|b| b.is_ascii_digit())).then_some(number)
}

/// The percentage in herdr's context readout, e.g. `26%` from `⛁ 26% (263k)`.
pub fn context_percent(readout: &str) -> Option<&str> {
    readout.split_whitespace().find(|word| {
        word.strip_suffix('%').is_some_and(|number| {
            !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        })
    })
}

/// `42s`, `4m 12s`, `2h 05m` or `3d 04h`: two units at most, so it stays short.
pub fn elapsed(duration: SignedDuration) -> String {
    let secs = duration.as_secs().max(0);
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m {:02}s", secs / 60, secs % 60),
        3600..86400 => format!("{}h {:02}m", secs / 3600, secs % 3600 / 60),
        _ => format!("{}d {:02}h", secs / 86400, secs % 86400 / 3600),
    }
}

/// `text` word-wrapped to `width` columns in at most `max` lines, the last ending in `…` if
/// anything was left out. A word wider than a line is broken.
pub fn wrap(text: &str, width: usize, max: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut rest = text.trim();
    while !rest.is_empty() && lines.len() < max && width > 0 {
        if rest.width() <= width || lines.len() + 1 == max {
            lines.push(truncate(rest, width));
            break;
        }
        // The last space that leaves the line within `width`, else as many characters as fit.
        let mut space = None;
        let mut fits = 0;
        let mut used = 0;
        for (index, ch) in rest.char_indices() {
            if ch == ' ' {
                space = Some(index);
            }
            used += ch.width().unwrap_or(0);
            if used > width {
                break;
            }
            fits = index + ch.len_utf8();
        }
        let end = space.filter(|&space| space > 0).unwrap_or(fits);
        if end == 0 {
            lines.push(truncate(rest, width));
            break;
        }
        lines.push(rest[..end].trim_end().to_owned());
        rest = rest[end..].trim_start();
    }
    lines
}

/// `text` cut to `width` columns, ending in `…` if anything was cut.
pub fn truncate(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    cut(text, width - 1)
}

/// As much of `text` as fits in `room` columns, then `…`.
fn cut(text: &str, room: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w > room {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

/// `spans` as one line cut to `width` columns, ending in `…` if anything was cut.
pub fn fit(spans: Vec<Span<'_>>, width: usize) -> Line<'static> {
    let total: usize = spans.iter().map(Span::width).sum();
    if total <= width {
        return Line::from(spans.into_iter().map(owned).collect::<Vec<_>>());
    }
    let mut room = width.saturating_sub(1);
    let mut out = Vec::new();
    for span in spans {
        let w = span.width();
        if w <= room {
            room -= w;
            out.push(owned(span));
            continue;
        }
        out.push(Span::styled(cut(&span.content, room), span.style));
        return Line::from(out);
    }
    Line::from(out)
}

pub fn owned(span: Span<'_>) -> Span<'static> {
    Span::styled(span.content.into_owned(), span.style)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_elapsed_in_two_units() {
        let cases = [
            (0, "0s"),
            (42, "42s"),
            (252, "4m 12s"),
            (3600, "1h 00m"),
            (7500, "2h 05m"),
            (273_600, "3d 04h"),
            (-5, "0s"),
        ];
        for (secs, text) in cases {
            assert_eq!(elapsed(SignedDuration::from_secs(secs)), text);
        }
    }

    #[test]
    fn finds_the_context_percentage() {
        assert_eq!(context_percent("⛁ 26% (263k)"), Some("26%"));
        assert_eq!(context_percent("12.5%"), Some("12.5%"));
        assert_eq!(context_percent("⛁ (263k)"), None);
        assert_eq!(context_percent("%"), None);
    }

    #[test]
    fn parses_pr_numbers() {
        assert_eq!(pr_number("https://github.com/erd/web/pull/42"), Some("42"));
        assert_eq!(pr_number("https://github.com/erd/web/pull/42/"), Some("42"));
        assert_eq!(pr_number("https://github.com/erd/web/pull/42/files"), None);
        assert_eq!(pr_number("https://github.com/erd/web"), None);
    }

    #[test]
    fn truncates_with_an_ellipsis() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("exactly", 7), "exactly");
        assert_eq!(truncate("truncated", 6), "trunc…");
        assert_eq!(truncate("⛁⛁⛁", 2), "⛁…");
        assert_eq!(truncate("abc", 1), "…");
        assert_eq!(truncate("abc", 0), "");
    }

    #[test]
    fn wraps_words_into_a_few_lines() {
        let title = "Fix the login redirect loop on expired sessions";
        assert_eq!(wrap(title, 50, 3), [title]);
        assert_eq!(
            wrap(title, 20, 3),
            ["Fix the login", "redirect loop on", "expired sessions"]
        );
        assert_eq!(wrap(title, 12, 3), ["Fix the", "login", "redirect lo…"]);
        // A space just past the edge still breaks there.
        assert_eq!(wrap("abcd efgh", 4, 3), ["abcd", "efgh"]);
        // Words wider than the line break mid-word.
        assert_eq!(wrap("abcdefgh", 3, 3), ["abc", "def", "gh"]);
        assert_eq!(wrap("⛁⛁⛁", 1, 2), ["⛁", "…"]);
        assert_eq!(wrap("", 10, 3), Vec::<String>::new());
        assert_eq!(wrap("abc", 0, 3), Vec::<String>::new());
    }

    #[test]
    fn fits_spans_keeping_styles() {
        let line = fit(vec![Span::raw("ab").red(), Span::raw("cdef")], 4);
        assert_eq!(line.to_string(), "abc…");
        assert_eq!(line.spans[0].style, Style::new().red());
        // The cut falls exactly at a span's end, with more spans after it.
        let line = fit(vec![Span::raw("abc"), Span::raw("d"), Span::raw("ef")], 5);
        assert_eq!(line.to_string(), "abcd…");
        assert_eq!(fit(vec![Span::raw("abc")], 3).to_string(), "abc");
    }

    #[test]
    fn every_state_has_a_single_width_symbol() {
        let states = [
            State::NeedsYou,
            State::Failed,
            State::InProgress,
            State::Review,
            State::ReadyToMerge,
            State::Pending,
            State::Done,
        ];
        for icons in [DeckIcons::Unicode, DeckIcons::Nerd] {
            for state in states {
                let symbol = badge(state, icons).symbol;
                assert_eq!(symbol.width(), 1, "{state:?} {icons:?}");
                assert!(!symbol.trim().is_empty(), "{state:?} {icons:?}");
            }
        }
    }
}
