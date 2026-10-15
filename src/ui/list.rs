//! Which card is selected, and which cards are scrolled into view.

use std::ops::Range;

/// The selected card, kept by task id so the selection follows a task when the sort moves it.
#[derive(Debug, Default)]
pub struct Selection {
    id: Option<String>,
    /// Where the selection was, for when its task disappears.
    index: usize,
    /// The first card in view.
    offset: usize,
}

impl Selection {
    /// The selected card's index in `ids`: the selected task if it's still there, else whatever
    /// took its place.
    pub fn resolve(&mut self, ids: &[&str]) -> Option<usize> {
        let last = ids.len().checked_sub(1)?;
        let index = self
            .id
            .as_deref()
            .and_then(|id| ids.iter().position(|other| *other == id))
            .unwrap_or(self.index.min(last));
        self.select(ids, index);
        Some(index)
    }

    /// Moves the selection `delta` cards down (or up, when negative), stopping at either end.
    pub fn move_by(&mut self, ids: &[&str], delta: isize) {
        if let Some(index) = self.resolve(ids) {
            let next = index.saturating_add_signed(delta).min(ids.len() - 1);
            self.select(ids, next);
        }
    }

    /// Scrolls just enough to show the whole selected card in `viewport` rows, given each
    /// card's height, and returns the cards that fit from the top of the view. Call
    /// [`resolve`](Self::resolve) first.
    pub fn scroll(&mut self, heights: &[u16], viewport: u16) -> Range<usize> {
        let Some(last) = heights.len().checked_sub(1) else {
            self.offset = 0;
            return 0..0;
        };
        let rows =
            |range: Range<usize>| -> usize { heights[range].iter().map(|&h| usize::from(h)).sum() };
        let viewport = usize::from(viewport);
        let selected = self.index.min(last);
        self.offset = self.offset.min(selected);
        while self.offset < selected && rows(self.offset..selected + 1) > viewport {
            self.offset += 1;
        }
        // Scroll back up when the cards below no longer fill the view.
        while self.offset > 0 && rows(self.offset - 1..heights.len()) <= viewport {
            self.offset -= 1;
        }
        let mut end = self.offset;
        let mut used = 0;
        while end < heights.len() && used + usize::from(heights[end]) <= viewport {
            used += usize::from(heights[end]);
            end += 1;
        }
        // A card taller than the whole view still shows, cut off.
        self.offset..end.max(self.offset + 1)
    }

    fn select(&mut self, ids: &[&str], index: usize) {
        self.index = index;
        self.id = Some(ids[index].to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDS: [&str; 5] = ["a", "b", "c", "d", "e"];

    #[test]
    fn starts_at_the_top_and_stops_at_the_ends() {
        let mut selection = Selection::default();
        assert_eq!(selection.resolve(&[]), None);
        assert_eq!(selection.resolve(&IDS), Some(0));
        selection.move_by(&IDS, -1);
        assert_eq!(selection.resolve(&IDS), Some(0));
        for _ in 0..10 {
            selection.move_by(&IDS, 1);
        }
        assert_eq!(selection.resolve(&IDS), Some(4));
    }

    #[test]
    fn follows_its_task_when_the_order_changes() {
        let mut selection = Selection::default();
        selection.move_by(&IDS, 2);
        assert_eq!(selection.resolve(&["c", "a", "b"]), Some(0));
    }

    #[test]
    fn keeps_its_place_when_its_task_goes() {
        let mut selection = Selection::default();
        selection.move_by(&IDS, 2);
        assert_eq!(selection.resolve(&["a", "b", "d", "e"]), Some(2));
        // ... and moves on from the task that took its place.
        selection.move_by(&["a", "b", "d", "e"], 1);
        assert_eq!(selection.resolve(&IDS), Some(4));
        assert_eq!(selection.resolve(&["a"]), Some(0));
    }

    #[test]
    fn scrolls_to_keep_the_selection_in_view() {
        let heights = [5, 6, 5, 5, 6];
        let mut selection = Selection::default();
        selection.resolve(&IDS);
        assert_eq!(selection.scroll(&heights, 12), 0..2);
        selection.move_by(&IDS, 2);
        assert_eq!(selection.scroll(&heights, 12), 1..3);
        selection.move_by(&IDS, 2);
        assert_eq!(selection.scroll(&heights, 12), 3..5);
        // Moving up inside the view doesn't scroll.
        selection.move_by(&IDS, -1);
        assert_eq!(selection.scroll(&heights, 12), 3..5);
        selection.move_by(&IDS, -3);
        assert_eq!(selection.scroll(&heights, 12), 0..2);
    }

    #[test]
    fn scrolls_back_when_the_view_grows() {
        let heights = [5, 5, 5, 5];
        let mut selection = Selection::default();
        selection.move_by(&IDS[..4], 3);
        assert_eq!(selection.scroll(&heights, 10), 2..4);
        assert_eq!(selection.scroll(&heights, 20), 0..4);
    }

    #[test]
    fn shows_a_card_taller_than_the_view() {
        let mut selection = Selection::default();
        selection.resolve(&IDS[..2]);
        assert_eq!(selection.scroll(&[6, 6], 4), 0..1);
        assert_eq!(selection.scroll(&[], 4), 0..0);
    }
}
