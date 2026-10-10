//! A project load's plugin restore, spread over frames: the plugins it
//! still has to load, in order, and how far it has come. Loading a plugin
//! blocks the eframe main thread (its instance is `!Send`, so it can't be
//! loaded anywhere else), so `Display` loads one per frame, before the
//! project is applied, and the
//! [`Overlay::RestoringInstruments`](super::modal_focus::Overlay) panel
//! repaints between them. See `130-plugin-host.md` § Project persistence.

use std::collections::VecDeque;

use crate::models::track::InstrumentRef;

/// The plugins a project load still has to restore, each with the engine
/// slot it goes into, and how many there were.
#[derive(Debug)]
pub(super) struct InstrumentRestore {
    /// What is left to load, front first.
    queue: VecDeque<(usize, InstrumentRef)>,
    /// How many there were.
    total: usize,
}

impl InstrumentRestore {
    /// A restore of `specs`, in order.
    pub(super) fn new(specs: Vec<(usize, InstrumentRef)>) -> Self {
        InstrumentRestore {
            total: specs.len(),
            queue: VecDeque::from(specs),
        }
    }

    /// The plugin to load next and the slot it goes into.
    pub(super) fn next(&self) -> Option<&(usize, InstrumentRef)> {
        self.queue.front()
    }

    /// Takes the plugin to load next off the queue.
    pub(super) fn pop(&mut self) -> Option<(usize, InstrumentRef)> {
        self.queue.pop_front()
    }

    /// Whether everything has been loaded.
    pub(super) fn is_done(&self) -> bool {
        self.queue.is_empty()
    }

    /// The step the next load is, 1-based, and how many steps there are —
    /// "3 of 8".
    pub(super) fn step(&self) -> (usize, usize) {
        let done = self.total - self.queue.len();
        ((done + 1).min(self.total), self.total)
    }

    /// How much of the restore is done, `0.0..=1.0` — the progress bar.
    pub(super) fn fraction(&self) -> f32 {
        (self.total - self.queue.len()) as f32 / self.total.max(1) as f32
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::InstrumentRestore;
    use crate::models::track::InstrumentRef;

    fn plugin(name: &str) -> InstrumentRef {
        InstrumentRef {
            bundle_path: PathBuf::from(format!("/{name}.clap")),
            plugin_id: name.to_owned(),
            display_name: name.to_owned(),
            state: Vec::new(),
        }
    }

    #[test]
    fn loads_in_order_and_counts_the_steps() {
        let mut restore =
            InstrumentRestore::new(vec![(0, plugin("a")), (2, plugin("b")), (3, plugin("c"))]);
        assert_eq!(restore.step(), (1, 3));
        assert_eq!(restore.fraction(), 0.0);
        assert_eq!(restore.next().map(|(slot, _)| *slot), Some(0));

        let (slot, want) = restore.pop().expect("first");
        assert_eq!((slot, want.display_name.as_str()), (0, "a"));
        assert_eq!(restore.step(), (2, 3));
        assert!((restore.fraction() - 1.0 / 3.0).abs() < f32::EPSILON);

        restore.pop();
        restore.pop();
        assert!(restore.is_done());
        assert!(restore.next().is_none());
        assert_eq!(restore.step(), (3, 3));
        assert_eq!(restore.fraction(), 1.0);
    }
}
