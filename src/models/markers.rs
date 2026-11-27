//! The arrangement's markers (`600-markers.md`): up to nine places in the
//! song, each a numbered slot recalled by its digit. A [`Marker`] is its
//! number, its tick and an optional name; [`Markers`] holds them, at most
//! one per number and one per tick, in tick order. Arrangement data: saved
//! with the project, changed only through undoable edits (`SetMarkerEdit`,
//! and the time edits, which carry them along with [`Markers::insert_time`]
//! and [`Markers::delete_time`]).

/// The highest marker number; numbers run `1..=MAX_MARKER`, one key each.
pub(crate) const MAX_MARKER: u8 = 9;

/// Whether `number` names a marker slot (`1..=MAX_MARKER`).
fn is_marker_number(number: u8) -> bool {
    (1..=MAX_MARKER).contains(&number)
}

/// One marker: a numbered place in the arrangement.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Marker {
    /// Its slot, `1..=MAX_MARKER`: the digit that recalls it.
    pub(crate) number: u8,
    /// Where it sits, in arrangement ticks, `>= 0`.
    pub(crate) tick: i32,
    /// Its name, `None` for a marker shown by its number alone. Never empty.
    pub(crate) name: Option<String>,
}

/// The arrangement's markers, in tick order, at most one per number and one
/// per tick (`600-markers.md` decisions 1 and 3).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct Markers {
    /// The markers, sorted by tick.
    list: Vec<Marker>,
}

impl Markers {
    /// The markers in `list`, as a project saved them, held to the rules: a
    /// number outside `1..=MAX_MARKER` is dropped, a tick below 0 is held at
    /// 0, a blank name is none, and of two with the same number or tick the
    /// later one wins, as a later `set` would.
    pub(crate) fn from_saved(list: impl IntoIterator<Item = Marker>) -> Self {
        let mut markers = Markers::default();
        for marker in list {
            markers.set(marker.number, marker.tick);
            markers.rename(marker.number, marker.name.as_deref());
        }
        markers
    }

    /// The markers in tick order.
    pub(crate) fn as_slice(&self) -> &[Marker] {
        &self.list
    }

    /// The `(number, tick)` of each marker, in tick order: what a test
    /// compares.
    #[cfg(test)]
    pub(crate) fn placed(&self) -> Vec<(u8, i32)> {
        self.list
            .iter()
            .map(|marker| (marker.number, marker.tick))
            .collect()
    }

    /// Whether there are none.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Marker `number`, if it is set.
    pub(crate) fn get(&self, number: u8) -> Option<&Marker> {
        self.list.iter().find(|marker| marker.number == number)
    }

    /// Sets marker `number` at `tick` (held to `>= 0`): moves it there if it
    /// is set elsewhere, keeping its name, and replaces any other marker
    /// already on that tick (`600-markers.md` decision 3). A number outside
    /// `1..=MAX_MARKER` changes nothing.
    pub(crate) fn set(&mut self, number: u8, tick: i32) {
        if !is_marker_number(number) {
            return;
        }
        let tick = tick.max(0);
        let name = self.get(number).and_then(|marker| marker.name.clone());
        self.list
            .retain(|marker| marker.number != number && marker.tick != tick);
        let at = self.list.partition_point(|marker| marker.tick < tick);
        self.list.insert(at, Marker { number, tick, name });
    }

    /// Names marker `number` `name`, trimmed; a blank or `None` name clears
    /// it, leaving the number. Its tick stays. Nothing when it isn't set.
    pub(crate) fn rename(&mut self, number: u8, name: Option<&str>) {
        let name = name.map(str::trim).filter(|name| !name.is_empty());
        if let Some(marker) = self.list.iter_mut().find(|marker| marker.number == number) {
            marker.name = name.map(str::to_owned);
        }
    }

    /// Insert time: every marker at or after `at` moves `amount` later,
    /// as the clips there do (`600-markers.md` decision 9). Returns whether
    /// any moved. `delete_time(at, at + amount)` undoes it exactly: nothing
    /// is left inside the gap.
    pub(crate) fn insert_time(&mut self, at: i32, amount: i32) -> bool {
        let mut moved = false;
        for marker in &mut self.list {
            if marker.tick >= at {
                marker.tick += amount;
                moved = true;
            }
        }
        moved
    }

    /// Delete time over `[start, end)`: the markers inside go, and every
    /// one at or after `end` moves `end - start` earlier (decision 9).
    pub(crate) fn delete_time(&mut self, start: i32, end: i32) {
        self.list
            .retain(|marker| marker.tick < start || marker.tick >= end);
        for marker in &mut self.list {
            if marker.tick >= end {
                marker.tick -= end - start;
            }
        }
    }

    /// Whether any marker sits at or after `tick`: one an insert of time
    /// there would move.
    pub(crate) fn any_at_or_after(&self, tick: i32) -> bool {
        self.list.iter().any(|marker| marker.tick >= tick)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Markers `(number, tick)` set in that order.
    fn markers(list: &[(u8, i32)]) -> Markers {
        let mut markers = Markers::default();
        for &(number, tick) in list {
            markers.set(number, tick);
        }
        markers
    }

    #[test]
    fn setting_an_empty_slot_adds_it_in_tick_order() {
        let markers = markers(&[(2, 1920), (1, 0), (5, 960)]);
        assert_eq!(markers.placed(), vec![(1, 0), (5, 960), (2, 1920)]);
    }

    #[test]
    fn setting_a_number_already_set_moves_it_and_keeps_its_name() {
        let mut markers = markers(&[(1, 0), (2, 960)]);
        markers.rename(1, Some("Verse"));
        markers.set(1, 3840);
        assert_eq!(markers.placed(), vec![(2, 960), (1, 3840)]);
        assert_eq!(markers.get(1).unwrap().name.as_deref(), Some("Verse"));
    }

    #[test]
    fn setting_on_another_markers_tick_replaces_it() {
        let mut markers = markers(&[(1, 0), (2, 960)]);
        markers.set(3, 960);
        assert_eq!(markers.placed(), vec![(1, 0), (3, 960)]);
        assert!(markers.get(2).is_none());
    }

    #[test]
    fn setting_a_number_where_it_already_is_changes_nothing() {
        let mut markers = markers(&[(1, 0), (2, 960)]);
        markers.rename(2, Some("Chorus"));
        let before = markers.clone();
        markers.set(2, 960);
        assert_eq!(markers, before);
    }

    #[test]
    fn a_number_outside_the_slots_is_refused() {
        let mut markers = Markers::default();
        markers.set(0, 0);
        markers.set(10, 0);
        assert!(markers.is_empty());
    }

    #[test]
    fn a_marker_before_the_start_is_held_at_zero() {
        assert_eq!(markers(&[(1, -50)]).placed(), vec![(1, 0)]);
    }

    #[test]
    fn rename_keeps_the_tick_and_a_blank_name_clears_it() {
        let mut markers = markers(&[(4, 1920)]);
        markers.rename(4, Some("  Bridge "));
        assert_eq!(markers.get(4).unwrap().name.as_deref(), Some("Bridge"));
        assert_eq!(markers.get(4).unwrap().tick, 1920);
        markers.rename(4, Some("   "));
        assert_eq!(markers.get(4).unwrap().name, None);
    }

    #[test]
    fn insert_time_moves_the_markers_at_or_after_the_point() {
        let mut markers = markers(&[(1, 0), (2, 960), (3, 1920)]);
        markers.insert_time(960, 480);
        assert_eq!(markers.placed(), vec![(1, 0), (2, 1440), (3, 2400)]);
    }

    #[test]
    fn delete_time_removes_the_inside_and_pulls_the_later_back() {
        let mut markers = markers(&[(1, 0), (2, 960), (3, 1439), (4, 1440), (5, 3840)]);
        markers.delete_time(960, 1440);
        assert_eq!(markers.placed(), vec![(1, 0), (4, 960), (5, 3360)]);
    }

    #[test]
    fn a_saved_list_is_held_to_the_rules() {
        let saved = [
            Marker {
                number: 1,
                tick: 960,
                name: Some(String::new()),
            },
            Marker {
                number: 12,
                tick: 0,
                name: None,
            },
            Marker {
                number: 2,
                tick: 960,
                name: Some("Chorus".to_owned()),
            },
            Marker {
                number: 3,
                tick: -10,
                name: None,
            },
        ];
        let markers = Markers::from_saved(saved);
        assert_eq!(markers.placed(), vec![(3, 0), (2, 960)]);
        assert_eq!(markers.get(2).unwrap().name.as_deref(), Some("Chorus"));
    }

    #[test]
    fn any_at_or_after_counts_a_marker_on_the_tick() {
        let markers = markers(&[(1, 960)]);
        assert!(markers.any_at_or_after(960));
        assert!(!markers.any_at_or_after(961));
    }
}
