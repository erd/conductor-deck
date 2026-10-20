//! The symbols the deck draws with: plain unicode for any monospace font, or Nerd Font icons.
//! Nerd glyphs are all in the BMP, one cell wide, and each icon is followed by a space so a
//! non-Mono Nerd Font has room for its wider icons.

use conductor_state::DeckIcons;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyphs {
    pub needs_you: &'static str,
    pub failed: &'static str,
    pub in_progress: &'static str,
    pub review: &'static str,
    pub ready_to_merge: &'static str,
    pub pending: &'static str,
    pub done: &'static str,
    /// After Done's count, open or folded.
    pub unfolded: &'static str,
    pub folded: &'static str,
    /// Before the time in state on a card.
    pub elapsed: &'static str,
    /// Before the context percentage.
    pub context: &'static str,
    /// Before the branch.
    pub branch: &'static str,
    /// Before the PR number.
    pub pr: &'static str,
}

pub const UNICODE: Glyphs = Glyphs {
    needs_you: "⚑",
    failed: "✗",
    in_progress: "◐",
    review: "◆",
    ready_to_merge: "⇡",
    pending: "·",
    done: "✓",
    unfolded: "▾",
    folded: "▸",
    elapsed: "",
    context: "ctx ",
    branch: "",
    pr: "PR",
};

pub const NERD: Glyphs = Glyphs {
    needs_you: "\u{f024}",      // fa-flag
    failed: "\u{ea87}",         // cod-error
    in_progress: "\u{f110}",    // fa-spinner
    review: "\u{ea70}",         // cod-eye
    ready_to_merge: "\u{eafe}", // cod-git_merge
    pending: "\u{f10c}",        // fa-circle_o
    done: "\u{f00c}",           // fa-check
    unfolded: "\u{eab4}",       // cod-chevron_down
    folded: "\u{eab6}",         // cod-chevron_right
    elapsed: "\u{f017} ",       // fa-clock_o
    context: "\u{eb08} ",       // cod-hubot
    branch: "\u{f418} ",        // oct-git_branch
    pr: "\u{f407}",             // oct-git_pull_request
};

/// The header's icon, before the repo name.
pub const TERMINAL: &str = "\u{ea85}"; // cod-terminal
/// A pill's caps, around its inverse middle.
pub const PILL_OPEN: &str = "\u{e0b6}"; // ple-left_half_circle_thick
pub const PILL_CLOSE: &str = "\u{e0b4}"; // ple-right_half_circle_thick
/// The point that ends a powerline segment.
pub const SEGMENT_END: &str = "\u{e0b0}"; // pl-left_hard_divider

impl Glyphs {
    pub fn of(icons: DeckIcons) -> &'static Self {
        match icons {
            DeckIcons::Unicode => &UNICODE,
            DeckIcons::Nerd => &NERD,
        }
    }
}
