//! The cycle window — `[start, end)` in ticks — shared by clips and the
//! arrangement.
//!
//! `Region` stores its bounds as `Arc<AtomicI32>` so the render thread can read
//! the loop window every frame without a lock (`020-views-and-state.md`).
//! [`from_shared`](Region::from_shared) is how `Transport` and `Clip` bind a
//! `Region` straight onto their `SharedAtomics` pair rather than owning a copy
//! that could drift. Bounds are kept normalized (`start <= end`, both `>= 0`)
//! and region changes are always intentional — nothing here auto-follows the
//! cursor.

use std::sync::{
    Arc,
    atomic::{AtomicI32, Ordering},
};

/// A `[start, end)` tick window, bounds held as shared atomics. See the module
/// docs.
#[derive(Debug, Clone)]
pub(crate) struct Region {
    /// Window start tick — a position.
    start: Arc<AtomicI32>,
    /// Window end tick — a position. Held `>= start`.
    end: Arc<AtomicI32>,
}

impl Region {
    /// A standalone region owning fresh atomics.
    pub(crate) fn new(start: i32, end: i32) -> Self {
        Region {
            start: Arc::new(AtomicI32::new(start)),
            end: Arc::new(AtomicI32::new(end)),
        }
    }

    /// A region that *is* the given atomic pair — reads and writes go straight
    /// through to shared state. The binding used by `Transport` and `Clip`.
    pub(crate) fn from_shared(start: Arc<AtomicI32>, end: Arc<AtomicI32>) -> Self {
        Region { start, end }
    }

    /// Current start tick.
    pub(crate) fn start(&self) -> i32 {
        self.start.load(Ordering::Relaxed)
    }

    /// A clone of the start atomic handle — for wiring another `Region` or a
    /// `SharedAtomics` field onto the same value.
    pub(crate) fn start_atomic(&self) -> Arc<AtomicI32> {
        Arc::clone(&self.start)
    }

    /// Current end tick.
    pub(crate) fn end(&self) -> i32 {
        self.end.load(Ordering::Relaxed)
    }

    /// A clone of the end atomic handle — see [`start_atomic`](Self::start_atomic).
    pub(crate) fn end_atomic(&self) -> Arc<AtomicI32> {
        Arc::clone(&self.end)
    }

    /// Sets either bound (`None` keeps the current value), clamping negatives to
    /// `0` and resolving an inverted result toward whichever bound the caller
    /// pinned. Returns `true` if anything actually changed.
    pub(crate) fn set_region(&mut self, new_start: Option<i32>, new_end: Option<i32>) -> bool {
        let current_start = self.start();
        let current_end = self.end();

        let mut start = new_start.unwrap_or(current_start).max(0);
        let mut end = new_end.unwrap_or(current_end).max(0);

        if start > end {
            // Resolve toward the pinned bound: only a lone new start moves
            // `start`; every other case moves `end`.
            if new_start.is_some() && new_end.is_none() {
                start = end;
            } else {
                end = start;
            }
        }

        if start == current_start && end == current_end {
            return false;
        }

        self.start.store(start, Ordering::Relaxed);
        self.end.store(end, Ordering::Relaxed);
        true
    }

    /// Rounds the window *length* down to a whole multiple of `target_length`
    /// (a bar, typically), but never below one multiple. Start is left where it
    /// is; end moves.
    pub(crate) fn snap_to_grid(&mut self, target_length: i32) {
        let region_start = self.start();
        let region_end = self.end();
        let snapped_length = ((region_end - region_start) / target_length) * target_length;
        self.set_region(None, Some(region_start + snapped_length.max(target_length)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_region_normal_change_returns_true() {
        let mut r = Region::new(0, 0);
        assert!(r.set_region(Some(10), Some(100)));
        assert_eq!(r.start(), 10);
        assert_eq!(r.end(), 100);
    }

    #[test]
    fn set_region_no_change_returns_false() {
        let mut r = Region::new(10, 100);
        assert!(!r.set_region(Some(10), Some(100)));
    }

    #[test]
    fn set_region_inverted_both_some_clamps_end_to_new_start() {
        // When both are provided and start > end, end is pulled up to start.
        let mut r = Region::new(0, 200);
        r.set_region(Some(150), Some(50));
        assert_eq!(r.start(), 150);
        assert_eq!(r.end(), 150);
    }

    #[test]
    fn set_region_inverted_only_start_some_clamps_start_to_current_end() {
        // Only new_start provided and it would exceed current end → start clamped to end.
        let mut r = Region::new(0, 50);
        r.set_region(Some(100), None);
        assert_eq!(r.start(), 50);
        assert_eq!(r.end(), 50);
    }

    #[test]
    fn set_region_negative_values_clamped_to_zero() {
        let mut r = Region::new(10, 100);
        r.set_region(Some(-50), Some(-10));
        assert_eq!(r.start(), 0);
        assert_eq!(r.end(), 0);
    }

    #[test]
    fn snap_to_grid_aligns_length_to_multiple_of_unit() {
        let bar = 3840_i32; // PPQN*4 with PPQN=960
        let mut r = Region::new(0, bar * 2 + 50); // slightly over 2 bars
        r.snap_to_grid(bar);
        assert_eq!(r.end() - r.start(), bar * 2);
    }

    #[test]
    fn snap_to_grid_raises_sub_unit_length_to_one_unit() {
        let bar = 3840_i32;
        let mut r = Region::new(0, bar / 2); // less than one bar
        r.snap_to_grid(bar);
        assert_eq!(r.end() - r.start(), bar);
    }

    #[test]
    fn snap_to_grid_zero_length_region_becomes_one_unit() {
        let bar = 3840_i32;
        let mut r = Region::new(0, 0);
        r.snap_to_grid(bar);
        assert_eq!(r.end() - r.start(), bar);
    }
}
