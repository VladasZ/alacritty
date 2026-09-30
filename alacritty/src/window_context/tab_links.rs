//! Visual links between neighbor tabs.
//!
//! A link joins a tab to the open tab right after it. Linked tabs form a
//! chain that the strip marks with a line under it. A chain moves as one block, so a move or a drop
//! never splits it, and only the link keys add or remove a link. Links carry no other
//! meaning.
//!
//! Each tab keeps one flag for the gap on its right. Closed tabs sit after
//! every open tab, so links only ever join open tabs.

use crate::event::TabId;

use super::WindowContext;

/// First and last index of the chain holding `index`. `links[i]` tells if
/// tab `i` is linked to tab `i + 1`.
pub(crate) fn chain_bounds(links: &[bool], index: usize) -> (usize, usize) {
    let mut first = index;
    while first > 0 && links.get(first - 1) == Some(&true) {
        first -= 1;
    }
    let mut last = index;
    while links.get(last).copied().unwrap_or(false) {
        last += 1;
    }
    (first, last)
}

/// The slice to rotate to move the chain holding `index` past the whole
/// neighbor block on one side, as `(start, end, shift)`. Rotating
/// `start..=end` left by `shift` moves the chain back, right by `shift`
/// moves it forward. None at the edge of the open tabs.
pub(super) fn chain_move(
    links: &[bool],
    open: usize,
    index: usize,
    forward: bool,
) -> Option<(usize, usize, usize)> {
    let (first, last) = chain_bounds(links, index);
    if forward {
        if last + 1 >= open {
            return None;
        }
        let (_, next_last) = chain_bounds(links, last + 1);
        Some((first, next_last, next_last - last))
    } else {
        let previous_last = first.checked_sub(1)?;
        let (previous_first, _) = chain_bounds(links, previous_last);
        Some((previous_first, last, first - previous_first))
    }
}

/// Links a closed tab had, so a restore can put them back.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct SavedLinks {
    left: Option<TabId>,
    right: Option<TabId>,
}

impl WindowContext {
    fn tab_links(&self) -> Vec<bool> {
        self.tabs.iter().map(|tab| tab.linked_right).collect()
    }

    /// Toggle the link between the active tab and its open neighbor.
    pub(super) fn toggle_tab_link(&mut self, forward: bool) {
        let index = self.active_tab;
        if self.is_closed(index) {
            return;
        }
        let gap = if forward { index } else { index.wrapping_sub(1) };
        if gap + 1 >= self.open_tab_count() {
            return;
        }
        self.tabs[gap].linked_right ^= true;
        self.dirty = true;
    }

    /// Move the active tab's chain one block toward `forward`.
    pub(super) fn move_active_tab(&mut self, forward: bool) {
        let index = self.active_tab;
        if self.is_closed(index) {
            return;
        }
        let links = self.tab_links();
        let Some((start, end, shift)) = chain_move(&links, self.open_tab_count(), index, forward)
        else {
            return;
        };

        let slice = &mut self.tabs[start..=end];
        if forward {
            slice.rotate_right(shift);
            self.active_tab += shift;
        } else {
            slice.rotate_left(shift);
            self.active_tab -= shift;
        }
        self.display.damage_tracker.frame().mark_fully_damaged();
        self.display.damage_tracker.next_frame().mark_fully_damaged();
        self.dirty = true;
    }

    /// Take the open tab at `index` out of its chain before it leaves the
    /// open tabs. A tab linked on both sides leaves its neighbors linked, so
    /// the chain stays whole.
    pub(super) fn unlink_tab(&mut self, index: usize) -> SavedLinks {
        let linked_left = index > 0 && self.tabs[index - 1].linked_right;
        let linked_right = self.tabs[index].linked_right;
        let saved = SavedLinks {
            left: linked_left.then(|| self.tabs[index - 1].id),
            right: linked_right.then(|| self.tabs[index + 1].id),
        };
        if linked_left {
            self.tabs[index - 1].linked_right = linked_right;
        }
        self.tabs[index].linked_right = false;
        saved
    }

    /// Link a tab just put back at `index` to the neighbors it had. A tab put
    /// back inside a chain joins that chain, so the chain stays whole.
    pub(super) fn relink_tab(&mut self, index: usize, saved: SavedLinks) {
        let open = self.open_tab_count();
        let left = index.checked_sub(1).map(|left| &self.tabs[left]);
        let right = (index + 1 < open).then(|| &self.tabs[index + 1]);
        let inside_chain = left.is_some_and(|left| left.linked_right);
        let link_left = inside_chain || left.is_some_and(|tab| saved.left == Some(tab.id));
        let link_right = right.is_some()
            && (inside_chain || right.is_some_and(|tab| saved.right == Some(tab.id)));

        if index > 0 {
            self.tabs[index - 1].linked_right = link_left;
        }
        self.tabs[index].linked_right = link_right;
    }
}

#[cfg(test)]
mod tests {
    use super::{chain_bounds, chain_move};

    #[test]
    fn chain_bounds_walks_both_ways() {
        let links = [false, true, true, false, false];
        assert_eq!(chain_bounds(&links, 0), (0, 0));
        assert_eq!(chain_bounds(&links, 2), (1, 3));
        assert_eq!(chain_bounds(&links, 3), (1, 3));
        assert_eq!(chain_bounds(&links, 4), (4, 4));
    }

    #[test]
    fn chains_move_past_whole_blocks() {
        // Tabs 0, 1-2, 3-4-5, 6.
        let links = [false, true, false, true, true, false, false];
        assert_eq!(chain_move(&links, 7, 1, false), Some((0, 2, 1)));
        assert_eq!(chain_move(&links, 7, 2, true), Some((1, 5, 3)));
        assert_eq!(chain_move(&links, 7, 6, false), Some((3, 6, 3)));
        assert_eq!(chain_move(&links, 7, 0, false), None);
        assert_eq!(chain_move(&links, 7, 6, true), None);
        // Closed tabs after the open ones are never a target.
        assert_eq!(chain_move(&links, 6, 4, true), None);
    }
}
