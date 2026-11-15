//! What the engine renders offline for (`archive/490-offline-export.md`): a run that
//! takes the device away and has the sources rendered block by block in
//! *virtual time*, as fast as they compute, instead of against the device's
//! deadline.

use std::time::Instant;

/// One offline run, as the `"audio-engine"` thread drives it. The engine
/// drops its stream, keeps the mixer and, block by block, has the run bring
/// whatever feeds the sources up to the block's first instant
/// ([`advance`](Self::advance)) and then renders the block, until the run
/// says it has all it asked for ([`is_done`](Self::is_done)). Then it opens
/// the stream again.
///
/// Time in the run is made up: frame `n` of the run is
/// [`start`](Self::start)` + n / rate`, whenever it is rendered. The engine's
/// clock is anchored there and never observed, so an event stamped with an
/// instant on that line lands on its sample exactly.
pub(crate) trait OfflineRun: Send {
    /// The instant of the run's first frame.
    fn start(&self) -> Instant;

    /// Brings what feeds the sources up to `until`, a block's first instant:
    /// every event due before it is in their feeds when this returns. May
    /// wait; there is no deadline. `false` gives the run up.
    fn advance(&mut self, until: Instant) -> bool;

    /// Whether the sources have rendered everything the run is for.
    fn is_done(&self) -> bool;
}
