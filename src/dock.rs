//! Holds the deck at a fixed number of columns. herdr sizes splits by ratio, so the deck finds the
//! split it sits in and rewrites that ratio whenever the layout or terminal width changes. A
//! ratio the deck didn't write is the user dragging the divider, and their width becomes the
//! deck's target for the rest of the session.

use crate::herdr::types::{Layout, LayoutNode, SplitDirection};
use crate::herdr::{self, HerdrClient};

/// The fewest columns the deck leaves for the panes beside it.
const MIN_OTHER_COLS: u16 = 20;
/// herdr keeps every split ratio within this range.
const RATIO_RANGE: (f32, f32) = (0.1, 0.9);
/// Ratios closer than this count as the same; herdr stores them as f32.
const RATIO_EPSILON: f32 = 1e-4;

/// The split that sets the deck's width: the deck's nearest ancestor that splits left and right.
#[derive(Debug, Clone, PartialEq)]
pub struct DeckSplit {
    /// From the root, `true` for `second` at each split.
    pub path: Vec<bool>,
    /// Whether the deck is on the right (`second`) side.
    pub deck_second: bool,
    pub ratio: f32,
    /// Every pane on the deck's side, the deck included.
    pub deck_side: Vec<String>,
    /// Every pane on the other side.
    pub other_side: Vec<String>,
}

/// Finds the split that sets `deck`'s width. Splits that stack panes vertically are skipped, so
/// the deck can itself be split downwards. `None` if the deck isn't in the tree or nothing sits
/// beside it.
pub fn find_deck_split(root: &LayoutNode, deck: &str) -> Option<DeckSplit> {
    let steps = path_to(root, deck)?;
    (0..steps.len()).rev().find_map(|depth| {
        let path = &steps[..depth];
        let LayoutNode::Split {
            direction: SplitDirection::Right,
            ratio,
            first,
            second,
        } = node_at(root, path)?
        else {
            return None;
        };
        let deck_second = steps[depth];
        let (deck_node, other_node) = if deck_second {
            (second, first)
        } else {
            (first, second)
        };
        Some(DeckSplit {
            path: path.to_vec(),
            deck_second,
            ratio: *ratio,
            deck_side: pane_ids(deck_node),
            other_side: pane_ids(other_node),
        })
    })
}

/// The ratio that gives the deck `deck_cols` of a split `total` columns wide.
///
/// herdr gives the first side `round(ratio × total)` columns, less a divider column when its
/// theme draws one (`gap`), and the second side the rest.
pub fn ratio_for(total: u16, deck_cols: u16, deck_second: bool, gap: u16) -> f32 {
    let cut = if deck_second {
        total.saturating_sub(deck_cols)
    } else {
        deck_cols.saturating_add(gap)
    };
    (f32::from(cut) / f32::from(total.max(1))).clamp(RATIO_RANGE.0, RATIO_RANGE.1)
}

/// Remembers the deck's target width and the last ratio it wrote, to tell its own writes from
/// the user's.
#[derive(Debug)]
pub struct WidthKeeper {
    target: u16,
    last_write: Option<(Vec<bool>, f32)>,
}

impl WidthKeeper {
    pub fn new(target: u16) -> Self {
        Self {
            target,
            last_write: None,
        }
    }

    #[cfg(test)]
    pub fn target(&self) -> u16 {
        self.target
    }

    /// Returns the ratio to write to `split`, if any, given that it is `total` columns wide and
    /// the deck's side currently has `deck_cols`.
    pub fn decide(&mut self, split: &DeckSplit, total: u16, deck_cols: u16) -> Option<f32> {
        if total == 0 {
            return None;
        }
        match &self.last_write {
            // The split moved in the tree, so its ratio says nothing about who changed it.
            Some((path, _)) if *path != split.path => self.last_write = None,
            Some((_, written)) if (split.ratio - written).abs() > RATIO_EPSILON => {
                // Someone else changed the ratio since our last write: keep their width.
                self.target = deck_cols;
                self.last_write = Some((split.path.clone(), split.ratio));
                return None;
            }
            _ => {}
        }
        let want_cols = self.target.min(total.saturating_sub(MIN_OTHER_COLS));
        if deck_cols == want_cols {
            return None;
        }
        let gap = if split.deck_second {
            0
        } else {
            // The columns herdr gives the first side beyond what its panes show.
            cut(split.ratio, total).saturating_sub(deck_cols).min(1)
        };
        let ratio = ratio_for(total, want_cols, split.deck_second, gap);
        // herdr's clamp can make the target unreachable; don't rewrite a ratio already there.
        ((ratio - split.ratio).abs() > RATIO_EPSILON).then_some(ratio)
    }

    /// Records a ratio as herdr stored it after the deck wrote it.
    pub fn wrote(&mut self, path: Vec<bool>, ratio: f32) {
        self.last_write = Some((path, ratio));
    }
}

/// Reads the deck's tab and, if the deck's width has drifted from its target, sets it back.
pub fn keep_width(
    herdr: &impl HerdrClient,
    keeper: &mut WidthKeeper,
    deck: &str,
) -> herdr::Result<()> {
    let layout = herdr.pane_layout(deck)?;
    if layout.zoomed {
        return Ok(());
    }
    let tree = herdr.layout_export(&layout.tab_id)?;
    let Some(split) = find_deck_split(&tree.root, deck) else {
        return Ok(());
    };
    let all: Vec<String> = split
        .deck_side
        .iter()
        .chain(&split.other_side)
        .cloned()
        .collect();
    let (Some(total), Some(deck_cols)) = (width(&layout, &all), width(&layout, &split.deck_side))
    else {
        // The geometry was read before the tree changed; the next layout event catches up.
        return Ok(());
    };
    if let Some(ratio) = keeper.decide(&split, total, deck_cols) {
        let stored = herdr.set_split_ratio(&tree.tab_id, &split.path, ratio)?;
        let stored = match node_at(&stored.root, &split.path) {
            Some(LayoutNode::Split { ratio, .. }) => *ratio,
            _ => ratio,
        };
        keeper.wrote(split.path, stored);
    }
    Ok(())
}

/// The columns spanned by `panes`, or `None` if any of them is missing from `layout`.
fn width(layout: &Layout, panes: &[String]) -> Option<u16> {
    let mut span: Option<(u16, u16)> = None;
    for pane in panes {
        let rect = layout.panes.iter().find(|p| &p.pane_id == pane)?.rect;
        let (left, right) = (rect.x, rect.x.saturating_add(rect.width));
        span = Some(span.map_or((left, right), |(l, r)| (l.min(left), r.max(right))));
    }
    span.map(|(left, right)| right - left)
}

/// The columns herdr gives the first side of a split, divider included.
fn cut(ratio: f32, total: u16) -> u16 {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a ratio in 0..=1 of a u16 fits in a u16"
    )]
    let cut = (ratio * f32::from(total)).round() as u16;
    cut
}

fn path_to(node: &LayoutNode, pane: &str) -> Option<Vec<bool>> {
    match node {
        LayoutNode::Pane { pane_id } => (pane_id.as_deref() == Some(pane)).then(Vec::new),
        LayoutNode::Split { first, second, .. } => [(false, first), (true, second)]
            .into_iter()
            .find_map(|(side, child)| {
                let mut rest = path_to(child, pane)?;
                rest.insert(0, side);
                Some(rest)
            }),
    }
}

fn node_at<'a>(root: &'a LayoutNode, path: &[bool]) -> Option<&'a LayoutNode> {
    path.iter().try_fold(root, |node, &side| match node {
        LayoutNode::Split { first, second, .. } => Some(if side { &**second } else { &**first }),
        LayoutNode::Pane { .. } => None,
    })
}

fn pane_ids(node: &LayoutNode) -> Vec<String> {
    match node {
        LayoutNode::Pane { pane_id } => pane_id.iter().cloned().collect(),
        LayoutNode::Split { first, second, .. } => {
            let mut ids = pane_ids(first);
            ids.extend(pane_ids(second));
            ids
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr::fake::{Action, FakeHerdr};
    use crate::herdr::types::{PluginContext, Snapshot};

    fn pane(id: &str) -> LayoutNode {
        LayoutNode::Pane {
            pane_id: Some(id.into()),
        }
    }

    fn split(direction: SplitDirection, ratio: f32, a: LayoutNode, b: LayoutNode) -> LayoutNode {
        LayoutNode::Split {
            direction,
            ratio,
            first: Box::new(a),
            second: Box::new(b),
        }
    }

    fn right(ratio: f32, a: LayoutNode, b: LayoutNode) -> LayoutNode {
        split(SplitDirection::Right, ratio, a, b)
    }

    fn down(ratio: f32, a: LayoutNode, b: LayoutNode) -> LayoutNode {
        split(SplitDirection::Down, ratio, a, b)
    }

    fn ids(names: &[&str]) -> Vec<String> {
        names.iter().map(|&n| n.to_owned()).collect()
    }

    /// The columns herdr gives the deck for `ratio`, with `gap` divider columns on the first side.
    fn deck_cols(ratio: f32, total: u16, deck_second: bool, gap: u16) -> u16 {
        if deck_second {
            total - cut(ratio, total)
        } else {
            cut(ratio, total) - gap
        }
    }

    #[test]
    fn ratio_gives_exact_columns_across_widths() {
        for total in [80, 120, 132, 145, 200, 300, 480] {
            for gap in [0, 1] {
                let ratio = ratio_for(total, 48, true, gap);
                assert_eq!(
                    deck_cols(ratio, total, true, gap),
                    48,
                    "W={total} gap={gap}"
                );
                let ratio = ratio_for(total, 48, false, gap);
                assert_eq!(
                    deck_cols(ratio, total, false, gap),
                    48,
                    "W={total} gap={gap}"
                );
            }
        }
    }

    #[test]
    fn ratio_stays_in_herdrs_range() {
        assert!((ratio_for(60, 58, true, 0) - 0.1).abs() < f32::EPSILON);
        assert!((ratio_for(60, 2, true, 0) - 0.9).abs() < f32::EPSILON);
        assert!((ratio_for(0, 48, true, 0) - 0.1).abs() < f32::EPSILON);
    }

    #[test]
    fn finds_deck_beside_root() {
        let root = right(0.5, pane("c"), pane("deck"));
        assert_eq!(
            find_deck_split(&root, "deck"),
            Some(DeckSplit {
                path: vec![],
                deck_second: true,
                ratio: 0.5,
                deck_side: ids(&["deck"]),
                other_side: ids(&["c"]),
            })
        );
    }

    #[test]
    fn finds_nested_deck_split() {
        // [[c | deck] | other]: the deck was opened beside a conductor that already had a
        // neighbour.
        let root = right(0.6, right(0.5, pane("c"), pane("deck")), pane("other"));
        let found = find_deck_split(&root, "deck").unwrap();
        assert_eq!(found.path, [false]);
        assert!(found.deck_second);
        assert_eq!(found.other_side, ids(&["c"]));
    }

    #[test]
    fn skips_vertical_splits_around_the_deck() {
        // The deck split downwards, and the conductor's side split downwards too.
        let root = right(
            0.5,
            down(0.5, pane("c"), pane("c2")),
            down(0.3, pane("deck"), pane("below")),
        );
        let found = find_deck_split(&root, "deck").unwrap();
        assert_eq!(found.path, Vec::<bool>::new());
        assert_eq!(found.deck_side, ids(&["deck", "below"]));
        assert_eq!(found.other_side, ids(&["c", "c2"]));
    }

    #[test]
    fn finds_deck_on_the_left() {
        let root = down(0.5, pane("top"), right(0.4, pane("deck"), pane("c")));
        let found = find_deck_split(&root, "deck").unwrap();
        assert_eq!(found.path, [true]);
        assert!(!found.deck_second);
    }

    #[test]
    fn no_split_without_a_neighbour_beside() {
        assert_eq!(find_deck_split(&pane("deck"), "deck"), None);
        let root = down(0.5, pane("c"), pane("deck"));
        assert_eq!(find_deck_split(&root, "deck"), None);
        let root = right(0.5, pane("c"), pane("x"));
        assert_eq!(find_deck_split(&root, "deck"), None);
    }

    fn deck_split(ratio: f32) -> DeckSplit {
        find_deck_split(&right(ratio, pane("c"), pane("deck")), "deck").unwrap()
    }

    #[test]
    fn writes_until_the_width_is_right() {
        let mut keeper = WidthKeeper::new(48);
        let ratio = keeper.decide(&deck_split(0.5), 132, 66).unwrap();
        assert_eq!(deck_cols(ratio, 132, true, 0), 48);
        keeper.wrote(vec![], ratio);
        // herdr echoes our write back as a layout change: nothing more to do.
        assert_eq!(keeper.decide(&deck_split(ratio), 132, 48), None);
        assert_eq!(keeper.target(), 48);
    }

    #[test]
    fn terminal_resize_rewrites_the_same_target() {
        let mut keeper = WidthKeeper::new(48);
        let ratio = keeper.decide(&deck_split(0.5), 132, 66).unwrap();
        keeper.wrote(vec![], ratio);
        // The terminal grows: herdr keeps the ratio, so the deck widens.
        let wider = deck_cols(ratio, 200, true, 0);
        assert_ne!(wider, 48);
        let next = keeper.decide(&deck_split(ratio), 200, wider).unwrap();
        assert_eq!(deck_cols(next, 200, true, 0), 48);
        assert_eq!(keeper.target(), 48);
    }

    #[test]
    fn manual_resize_becomes_the_target() {
        let mut keeper = WidthKeeper::new(48);
        let ratio = keeper.decide(&deck_split(0.5), 132, 66).unwrap();
        keeper.wrote(vec![], ratio);
        // The user drags the divider: a ratio we didn't write.
        let dragged = deck_split(0.6);
        let cols = deck_cols(0.6, 132, true, 0);
        assert_eq!(keeper.decide(&dragged, 132, cols), None);
        assert_eq!(keeper.target(), cols);
        // Their width now holds through a terminal resize.
        let at_200 = deck_cols(0.6, 200, true, 0);
        let next = keeper.decide(&dragged, 200, at_200).unwrap();
        assert_eq!(deck_cols(next, 200, true, 0), cols);
    }

    #[test]
    fn restructured_tree_is_not_a_manual_resize() {
        let mut keeper = WidthKeeper::new(48);
        keeper.wrote(vec![false], 0.3);
        // A neighbour closed and the deck's split moved up to the root, at another ratio.
        let ratio = keeper.decide(&deck_split(0.5), 132, 66).unwrap();
        assert_eq!(deck_cols(ratio, 132, true, 0), 48);
        assert_eq!(keeper.target(), 48);
    }

    #[test]
    fn unreachable_target_settles_at_herdrs_limit() {
        // 48 of 600 columns needs a ratio past herdr's 0.9, so the deck stops at 60.
        let mut keeper = WidthKeeper::new(48);
        let ratio = keeper.decide(&deck_split(0.5), 600, 300).unwrap();
        assert!((ratio - 0.9).abs() < RATIO_EPSILON);
        keeper.wrote(vec![], ratio);
        let cols = deck_cols(ratio, 600, true, 0);
        assert_eq!(cols, 60);
        // Neither a rewrite of the same ratio nor a manual resize.
        assert_eq!(keeper.decide(&deck_split(ratio), 600, cols), None);
        assert_eq!(keeper.target(), 48);
    }

    #[test]
    fn records_the_ratio_herdr_stored() {
        // herdr may store a ratio other than the one sent; the stored one is the deck's own.
        let mut keeper = WidthKeeper::new(48);
        keeper.wrote(vec![], 0.6);
        assert_eq!(keeper.decide(&deck_split(0.6), 120, 48), None);
        assert_eq!(keeper.target(), 48);
    }

    #[test]
    fn leaves_room_beside_the_deck() {
        let mut keeper = WidthKeeper::new(48);
        let ratio = keeper.decide(&deck_split(0.5), 60, 30).unwrap();
        assert_eq!(deck_cols(ratio, 60, true, 0), 40);
    }

    #[test]
    fn deck_on_the_left_accounts_for_a_divider() {
        let mut keeper = WidthKeeper::new(48);
        let found = find_deck_split(&right(0.5, pane("deck"), pane("c")), "deck").unwrap();
        // herdr gave the first side 73 columns at 0.5 of 145, one of them a divider.
        let ratio = keeper.decide(&found, 145, 72).unwrap();
        assert_eq!(deck_cols(ratio, 145, false, 1), 48);
    }

    fn fake(root: LayoutNode, width: u16) -> FakeHerdr {
        let context = PluginContext::from_parts(None, Some("w1:deck".into()), None, None).unwrap();
        let herdr = FakeHerdr::new(context, Snapshot::default());
        herdr.add_tab(root, "w1:t1", width);
        herdr
    }

    /// The path of every ratio the deck wrote.
    fn ratio_writes(herdr: &FakeHerdr) -> Vec<Vec<bool>> {
        herdr
            .actions()
            .into_iter()
            .filter_map(|action| match action {
                Action::SetSplitRatio { path, .. } => Some(path),
                _ => None,
            })
            .collect()
    }

    fn deck_width(herdr: &FakeHerdr) -> u16 {
        let layout = herdr.pane_layout("w1:deck").unwrap();
        let rect = layout
            .panes
            .iter()
            .find(|p| p.pane_id == "w1:deck")
            .unwrap();
        rect.rect.width
    }

    #[test]
    fn keeps_the_deck_width_through_herdr() {
        let root = right(
            0.5,
            right(0.5, pane("w1:c"), pane("w1:deck")),
            down(0.5, pane("w1:a"), pane("w1:b")),
        );
        let herdr = fake(root, 200);
        let mut keeper = WidthKeeper::new(48);

        keep_width(&herdr, &mut keeper, "w1:deck").unwrap();
        assert_eq!(deck_width(&herdr), 48);
        assert_eq!(ratio_writes(&herdr), [[false]]);

        // Our own write coming back changes nothing.
        keep_width(&herdr, &mut keeper, "w1:deck").unwrap();
        assert_eq!(ratio_writes(&herdr).len(), 1);

        // The terminal narrows; the deck holds 48.
        herdr.edit_tab("w1:t1", |tab| tab.area.width = 160);
        keep_width(&herdr, &mut keeper, "w1:deck").unwrap();
        assert_eq!(deck_width(&herdr), 48);
        assert_eq!(ratio_writes(&herdr).len(), 2);
    }

    #[test]
    fn adopts_a_dragged_divider_through_herdr() {
        let herdr = fake(right(0.5, pane("w1:c"), pane("w1:deck")), 132);
        let mut keeper = WidthKeeper::new(48);
        keep_width(&herdr, &mut keeper, "w1:deck").unwrap();

        herdr.edit_tab("w1:t1", |tab| {
            if let LayoutNode::Split { ratio, .. } = &mut tab.tree.root {
                *ratio = 0.5;
            }
        });
        keep_width(&herdr, &mut keeper, "w1:deck").unwrap();
        assert_eq!(keeper.target(), 66);
        assert_eq!(ratio_writes(&herdr).len(), 1);

        herdr.edit_tab("w1:t1", |tab| tab.area.width = 180);
        keep_width(&herdr, &mut keeper, "w1:deck").unwrap();
        assert_eq!(deck_width(&herdr), 66);
    }

    #[test]
    fn leaves_a_zoomed_or_lone_deck_alone() {
        let herdr = fake(pane("w1:deck"), 132);
        let mut keeper = WidthKeeper::new(48);
        keep_width(&herdr, &mut keeper, "w1:deck").unwrap();

        let herdr = fake(right(0.5, pane("w1:c"), pane("w1:deck")), 132);
        herdr.edit_tab("w1:t1", |tab| tab.tree.zoomed = true);
        keep_width(&herdr, &mut keeper, "w1:deck").unwrap();
        assert_eq!(ratio_writes(&herdr), Vec::<Vec<bool>>::new());
    }
}
