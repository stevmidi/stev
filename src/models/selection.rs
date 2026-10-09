//! A generic id-set selection with a moving "lead".
//!
//! Used for track, clip and per-clip event selection. It keeps a set of
//! selected [`Uuid`]s plus a *lead* — the most-recently focused id, which edits
//! that act on "the one selected thing" use. `select` collapses to a single id;
//! `set_ids` replaces the whole set (and, unlike `select`, may empty it).

use uuid::Uuid;

/// A set of selected ids with a lead. See the module docs.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Selection {
    /// The moving lead of a range selection (most-recently focused id).
    selected_id: Option<Uuid>,
    /// Every selected id, lead included.
    selected_ids: Vec<Uuid>,
}

impl Selection {
    /// An empty selection.
    pub(crate) fn new() -> Self {
        Self {
            selected_id: None,
            selected_ids: Vec::new(),
        }
    }

    // --- Lead / primary ---

    /// The lead id, if anything is selected.
    pub(crate) fn selected_id(&self) -> Option<Uuid> {
        self.selected_id
    }

    /// A copy of the full selected set.
    pub(crate) fn selected_ids(&self) -> Vec<Uuid> {
        self.selected_ids.clone()
    }

    /// The full selected set, borrowed.
    pub(crate) fn ids(&self) -> &[Uuid] {
        &self.selected_ids
    }

    /// Whether nothing is selected.
    pub(crate) fn is_empty(&self) -> bool {
        self.selected_ids.is_empty()
    }

    /// Replace the entire selection with a single event.
    pub(crate) fn select(&mut self, id: Option<Uuid>) -> Option<Uuid> {
        let old = self.selected_id;
        self.selected_id = id;
        self.selected_ids.clear();
        if let Some(id) = id {
            self.selected_ids.push(id);
        }
        old
    }

    /// Replace the selected set wholesale (e.g. from a marquee drag). Lead
    /// becomes the last id in `ids` (or `None` if `ids` is empty) — unlike
    /// `select()`, this can legitimately empty the selection.
    pub(crate) fn set_ids(&mut self, ids: Vec<Uuid>) {
        self.selected_id = ids.last().copied();
        self.selected_ids = ids;
    }

    /// Empties the selection, returning the ids that were selected.
    pub(crate) fn clear_selection(&mut self) -> Vec<Uuid> {
        self.selected_id = None;
        std::mem::take(&mut self.selected_ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_ids_replaces_the_set_and_moves_lead_to_the_last_id() {
        let mut selection = Selection::new();
        selection.select(Some(Uuid::new_v4())); // pre-existing lead

        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        selection.set_ids(vec![a, b]);

        assert_eq!(selection.selected_ids(), vec![a, b]);
        assert_eq!(selection.selected_id(), Some(b));
    }

    #[test]
    fn set_ids_with_empty_vec_clears_lead() {
        let mut selection = Selection::new();
        selection.select(Some(Uuid::new_v4()));

        selection.set_ids(vec![]);

        assert!(selection.selected_ids().is_empty());
        assert_eq!(selection.selected_id(), None);
    }

    #[test]
    fn clear_selection_returns_the_old_ids_and_empties_the_set() {
        let mut selection = Selection::new();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        selection.set_ids(vec![a, b]);

        assert_eq!(selection.clear_selection(), vec![a, b]);
        assert!(selection.is_empty());
        assert_eq!(selection.selected_id(), None);
    }
}
