//! One card: a bordered block titled with the task id, and the text helpers behind its lines.

use conductor_state::{Activity, CheckState, Phase, PrState, ReviewDecision, StatusVerb};
use jiff::{SignedDuration, Timestamp};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style, Stylize as _};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph, Widget as _};
use unicode_width::{UnicodeWidthChar as _, UnicodeWidthStr as _};

use crate::deck::CardModel;
use crate::deck::card::Attention;
use crate::herdr::types::AgentStatus;

/// A phase badge. The symbol carries the meaning; the color only backs it up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Badge {
    pub symbol: &'static str,
    pub style: Style,
}

pub fn badge(phase: Phase) -> Badge {
    let (symbol, style) = match phase {
        Phase::Working => ("●", Style::new().green()),
        Phase::NeedsDecision => ("?", Style::new().yellow().bold()),
        Phase::Review => ("◆", Style::new().cyan()),
        Phase::AwaitingMerge => ("⇡", Style::new().magenta()),
        Phase::Merged => ("✓", Style::new().green()),
        Phase::Failed => ("✗", Style::new().red().bold()),
        Phase::Intake | Phase::Briefed | Phase::Spawning | Phase::TornDown | Phase::Abandoned => {
            ("○", Style::new().dim())
        }
    };
    Badge { symbol, style }
}

/// Marks cards that need attention, in the header counts.
pub const ATTENTION: &str = "!";

/// Rows the card takes, borders included.
pub fn height(card: &CardModel) -> u16 {
    if has_pr(card) { 6 } else { 5 }
}

pub fn render(card: &CardModel, now: Timestamp, selected: bool, area: Rect, buf: &mut Buffer) {
    let width = usize::from(area.width.saturating_sub(2));
    let title = truncate(&card.task.id, width.saturating_sub(2));
    let mut block = Block::bordered().title(format!(" {title} "));
    if selected {
        block = block
            .border_type(BorderType::Thick)
            .title_style(Style::new().bold());
    }
    let badge = badge(card.task.phase);
    let mut lines = vec![
        fit(
            vec![
                Span::styled(badge.symbol, badge.style),
                Span::raw(" "),
                Span::raw(card.task.title.as_str()),
            ],
            width,
        ),
        activity_line(card, now, width),
        context_line(card, width),
    ];
    if has_pr(card) {
        lines.push(pr_line(card, width));
    }
    let mut paragraph = Paragraph::new(lines).block(block);
    if matches!(card.task.phase, Phase::TornDown | Phase::Abandoned) {
        paragraph = paragraph.dim();
    }
    paragraph.render(area, buf);
}

/// The activity on the left and the time in state on the right; the activity gives way first.
fn activity_line(card: &CardModel, now: Timestamp, width: usize) -> Line<'static> {
    let (text, style) = activity(card);
    let elapsed = elapsed(card.elapsed(now));
    let room = width.saturating_sub(elapsed.width() + 1);
    let mut line = fit(vec![Span::styled(text, style)], room);
    let gap = width.saturating_sub(line.width() + elapsed.width());
    line.push_span(" ".repeat(gap));
    line.push_span(Span::raw(elapsed).dim());
    line
}

/// What the worker is doing, or why it's stuck.
fn activity(card: &CardModel) -> (String, Style) {
    let attention = Style::new().bold();
    let live = &card.live;
    if card.attention.contains(&Attention::Permission) {
        return ("needs permission".into(), attention);
    }
    if let Some(first) = live.open_decisions.iter().next() {
        let text = match live.open_decisions.len() - 1 {
            0 => format!("waiting on decision: {first}"),
            more => format!("waiting on decision: {first} +{more}"),
        };
        return (text, attention);
    }
    for (reason, verb) in [
        (Attention::Blocked, StatusVerb::Blocked),
        (Attention::Failed, StatusVerb::Failed),
    ] {
        if card.attention.contains(&reason) {
            let word = if verb == StatusVerb::Blocked {
                "blocked"
            } else {
                "failed"
            };
            let text = live
                .status
                .as_ref()
                .filter(|status| status.verb == verb)
                .and_then(|status| status.text.as_deref())
                .map_or_else(|| word.to_owned(), |text| format!("{word}: {text}"));
            return (text, attention);
        }
    }
    let agent_working = card
        .agent
        .as_ref()
        .is_some_and(|agent| agent.status == AgentStatus::Working);
    let dim = Style::new().dim();
    match &live.activity {
        Activity::Working { tool: Some(tool) } => {
            let text = match &tool.summary {
                Some(summary) => format!("{} {summary}", tool.name),
                None => tool.name.clone(),
            };
            (text, Style::new())
        }
        Activity::Working { tool: None } => ("working".into(), Style::new()),
        _ if agent_working => ("working".into(), Style::new()),
        Activity::Idle { error: Some(error) } => (format!("idle: {error}"), dim),
        Activity::Idle { error: None } => ("idle".into(), dim),
        Activity::Exited { .. } => ("exited".into(), dim),
        Activity::NotStarted | Activity::NeedsPermission { .. } => ("not started".into(), dim),
    }
}

/// `ctx 26% · <branch>`.
fn context_line(card: &CardModel, width: usize) -> Line<'static> {
    let context = card
        .agent
        .as_ref()
        .and_then(|agent| agent.context.as_deref())
        .and_then(context_percent)
        .unwrap_or("–");
    let mut spans = vec![Span::raw(format!("ctx {context}"))];
    if let Some(branch) = &card.task.branch {
        spans.push(Span::raw(" · ").dim());
        spans.push(Span::raw(branch.clone()));
    }
    fit(spans, width)
}

/// `PR #123 ready · checks ✓ · approved`; the parts herdr or GitHub haven't reported are left out.
fn pr_line(card: &CardModel, width: usize) -> Line<'static> {
    let number = card.pr_url().and_then(pr_number);
    let mut spans = vec![Span::raw(match number {
        Some(number) => format!("PR #{number}"),
        None => "PR".to_owned(),
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
            CheckState::Pass => ("✓", Style::new().fg(Color::Green)),
            CheckState::Fail => ("✗", Style::new().fg(Color::Red).bold()),
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

/// `42s`, `4m12s`, `2h05m` or `3d04h`: two units at most, so it stays short.
pub fn elapsed(duration: SignedDuration) -> String {
    let secs = duration.as_secs().max(0);
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m{:02}s", secs / 60, secs % 60),
        3600..86400 => format!("{}h{:02}m", secs / 3600, secs % 3600 / 60),
        _ => format!("{}d{:02}h", secs / 86400, secs % 86400 / 3600),
    }
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

fn owned(span: Span<'_>) -> Span<'static> {
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
            (252, "4m12s"),
            (3600, "1h00m"),
            (7500, "2h05m"),
            (273_600, "3d04h"),
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
    fn every_phase_has_a_symbol() {
        for phase in Phase::ALL {
            assert!(!badge(phase).symbol.trim().is_empty(), "{phase:?}");
        }
    }
}
