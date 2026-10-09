//! A fixed-size worker pool for the audio callback.
//!
//! The audio callback is a deadline, not a throughput budget (see
//! [`AudioLoad`](super::AudioLoad)): everything must be rendered within
//! `frames / sample_rate` seconds, and doing it on one thread caps the app at
//! one core no matter how many the machine has. [`WorkerPool::for_each`] spreads
//! an independent per-item workload — in practice `InstrumentMixer`'s render pass, one
//! item per instrument track — across the pool *and the calling thread*, and
//! returns only when every item is done.
//!
//! It is not `rayon`: nothing here allocates, locks, or work-steals across
//! sections, because all of that is forbidden on the audio thread.
//!
//! ## The handshake
//!
//! One section (`for_each` call) runs as a strict ping-pong per worker:
//!
//! 1. the caller writes the type-erased [`Job`] and resets the claim cursor;
//! 2. it bumps `seq` on each participating worker and unparks it;
//! 3. every runner — workers *and* the caller — claims item indices off the
//!    shared `next` cursor with `fetch_add` and runs them until exhausted;
//! 4. each worker stores its `seq` into its `ack`;
//! 5. the caller spins until every participating worker has acked.
//!
//! Step 5 is what makes step 1 sound: a worker only touches `job` between
//! observing its `seq` change and storing its `ack`, and the caller never
//! rewrites `job` until every one of those windows has closed. There is
//! therefore no such thing as a straggler from a previous section, and the
//! `JobCtx` on the caller's stack is guaranteed to outlive every dereference of
//! it. The flip side is that the caller waits for the slowest worker to *wake*,
//! which is exactly what macOS audio workgroups exist to bound — see
//! `docs/170-multicore-scheduling.md`.

use std::cell::UnsafeCell;
use std::hint::spin_loop;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::null;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread::{Builder, JoinHandle, park_timeout};
use std::time::Duration;

use super::enter_render_thread;

/// How long a worker spins on its `seq` *after finishing a section* before it
/// parks — just long enough to catch a section already on its way, since the
/// caller unparks only after publishing and the store is often visible first.
///
/// Deliberately tiny. `spin_loop()` is `pause`, which costs ~140 cycles on
/// modern x86, so this is single-digit microseconds; anything longer burns a
/// core through the gap between blocks (~4 ms of every 5.33 ms at 256 frames /
/// 48 kHz) for a wakeup that will not arrive. A worker that is merely idle does
/// not spin at all.
const SPIN_ROUNDS: u32 = 128;

/// Park timeout for an idle worker. Nothing depends on the wakeup — `unpark`
/// carries a token, so a publish can never be missed, and shutdown unparks
/// explicitly. It exists only so a worker can't be wedged forever by a lost
/// wakeup, which is why it is long: a short one would spin these threads up
/// hundreds of times a second for nothing.
const PARK_TIMEOUT: Duration = Duration::from_millis(50);

/// One section's work, type-erased so the pool doesn't need to be generic.
#[derive(Clone, Copy)]
struct Job {
    /// Runs one item: `(run)(ctx, index)`.
    run: unsafe fn(*const (), usize),
    /// Points at a [`JobCtx`] on the *caller's stack*, valid for the whole
    /// section — see the handshake above.
    ctx: *const (),
    /// Number of items in the section.
    len: usize,
}

impl Job {
    /// The between-sections job: no items, a no-op runner.
    const EMPTY: Job = Job {
        run: run_nothing,
        ctx: null(),
        len: 0,
    };
}

/// The [`Job::EMPTY`] runner.
unsafe fn run_nothing(_ctx: *const (), _index: usize) {}

/// The caller-side payload a [`Job`] points at. Holds the items as a raw
/// pointer, never as `&mut [T]`: runners dereference disjoint indices
/// concurrently, so no reference spanning the whole slice may exist while they
/// run.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
struct JobCtx<T, F> {
    /// Raw pointer to the item slice (never a `&mut [T]` — runners hold
    /// disjoint `&mut T` into it concurrently).
    items: *mut T,
    /// The per-item closure, `Fn(usize, &mut T)`.
    f: F,
}

/// [`Job::run`] for a [`JobCtx<T, F>`]: derefs one claimed `index` and calls
/// the closure on it.
///
/// # Safety
///
/// `ctx` must point at a live `JobCtx<T, F>` whose `items` has at least
/// `index + 1` elements, and `index` must have been claimed by this runner
/// alone.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
unsafe fn run_job<T, F: Fn(usize, &mut T)>(ctx: *const (), index: usize) {
    // SAFETY: the caller's contract — `ctx` is the `JobCtx<T, F>` this `Job`
    // was built from, and it outlives the section.
    let ctx = unsafe { &*ctx.cast::<JobCtx<T, F>>() };
    // SAFETY: `index` was handed out by the claim cursor exactly once, so this
    // is the only live reference to that item.
    let item = unsafe { &mut *ctx.items.add(index) };
    (ctx.f)(index, item);
}

/// Per-worker handshake state. `seq` is written only by the caller, `ack` only
/// by that worker.
struct WorkerSlot {
    /// Section number the caller has published for this worker.
    seq: AtomicU64,
    /// Section number this worker has finished.
    ack: AtomicU64,
}

/// Shared state between the calling (audio) thread and the workers.
struct PoolShared {
    /// The current section's [`Job`] — only mutated during the handshake window.
    job: UnsafeCell<Job>,
    /// Claim cursor: `fetch_add` hands each item index to exactly one runner.
    next: AtomicUsize,
    /// Section counter, written only by the caller.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    seq: AtomicU64,
    /// Cleared on drop to retire the workers.
    running: AtomicBool,
    /// Per-worker handshake slots.
    workers: Vec<WorkerSlot>,
}

// SAFETY: `job` is the only non-atomic field. The caller writes it only while
// every worker's `ack` equals its `seq` — i.e. while no worker is inside the
// window where it may read `job` — and each worker's read is ordered after its
// own `seq` acquire-load, which pairs with the caller's release-store. The raw
// pointer inside `Job` is never dereferenced outside that window.
unsafe impl Send for PoolShared {}
unsafe impl Sync for PoolShared {}

impl PoolShared {
    /// Claims and runs item indices until the section is exhausted. Run by
    /// every runner, the calling thread included.
    fn run_claimed(&self, job: &Job) {
        loop {
            let index = self.next.fetch_add(1, Ordering::Relaxed);
            if index >= job.len {
                return;
            }
            // A panicking item would otherwise leave a worker's `ack` unset and
            // spin the audio thread forever, so it is contained here and the
            // item simply produces nothing. On the non-panicking path this
            // costs nothing.
            //
            // SAFETY: `index` was claimed by this runner alone, and `job.ctx`
            // stays live until the caller's barrier — which every runner
            // reaches before the caller returns.
            let _ = catch_unwind(AssertUnwindSafe(|| unsafe { (job.run)(job.ctx, index) }));
        }
    }
}

/// A pool of worker threads that help the calling thread through one
/// independent-per-item workload at a time. See the module docs.
pub(crate) struct WorkerPool {
    /// Handshake + job state shared with the workers.
    shared: Arc<PoolShared>,
    /// The `"audio-worker-N"` thread handles.
    threads: Vec<JoinHandle<()>>,
}

impl WorkerPool {
    /// Spawns `worker_count` `"audio-worker-N"` threads. `0` is valid and makes
    /// [`for_each`](Self::for_each) run everything on the calling thread.
    pub(crate) fn new(worker_count: usize) -> Self {
        let shared = Arc::new(PoolShared {
            job: UnsafeCell::new(Job::EMPTY),
            next: AtomicUsize::new(0),
            seq: AtomicU64::new(0),
            running: AtomicBool::new(true),
            workers: (0..worker_count)
                .map(|_| WorkerSlot {
                    seq: AtomicU64::new(0),
                    ack: AtomicU64::new(0),
                })
                .collect(),
        });

        let threads = (0..worker_count)
            .map(|index| {
                let shared = shared.clone();
                Builder::new()
                    .name(format!("audio-worker-{index}"))
                    .spawn(move || worker_loop(&shared, index))
                    .expect("failed to spawn audio worker thread")
            })
            .collect();

        Self { shared, threads }
    }

    /// Runs `f(index, item)` for every element of `items`, spread across the
    /// pool and the calling thread. Returns only once every item is done.
    ///
    /// `runners` is how many threads *should* take part, the caller included —
    /// pass the number of items that will actually do meaningful work, so a
    /// mostly-empty slice doesn't pay to wake workers that would find nothing.
    /// It is clamped to the pool size and to `items.len()`; `1` (or a pool with
    /// no workers) runs everything inline with no handshake at all.
    ///
    /// `f` must not block: every runner is a real-time thread, and the caller
    /// waits for all of them.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn for_each<T, F>(&self, items: &mut [T], runners: usize, f: F)
    where
        T: Send,
        F: Fn(usize, &mut T) + Sync,
    {
        let len = items.len();
        if len == 0 {
            return;
        }
        let helpers = self
            .shared
            .workers
            .len()
            .min(runners.saturating_sub(1))
            .min(len - 1);
        if helpers == 0 {
            for (index, item) in items.iter_mut().enumerate() {
                f(index, item);
            }
            return;
        }

        // Lives on this stack frame for the whole section — every runner has
        // acked by the time `for_each` returns, so no dereference can outlive
        // it.
        let ctx = JobCtx {
            items: items.as_mut_ptr(),
            f,
        };
        let job = Job {
            run: run_job::<T, F>,
            ctx: (&raw const ctx).cast(),
            len,
        };

        // SAFETY: no worker may read `job` until its `seq` is bumped below, and
        // the previous section's barrier already established that none is still
        // reading the last one.
        unsafe { *self.shared.job.get() = job };
        self.shared.next.store(0, Ordering::Relaxed);

        let seq = self.shared.seq.load(Ordering::Relaxed) + 1;
        self.shared.seq.store(seq, Ordering::Relaxed);
        for (slot, thread) in self.shared.workers[..helpers].iter().zip(&self.threads) {
            // Release: pairs with the worker's acquire-load, publishing `job`
            // and the reset cursor along with the new sequence number.
            slot.seq.store(seq, Ordering::Release);
            thread.thread().unpark();
        }

        // The calling thread is a runner too — it never parks, so it usually
        // finishes its share around the time the workers finish theirs.
        self.shared.run_claimed(&job);

        for slot in &self.shared.workers[..helpers] {
            while slot.ack.load(Ordering::Acquire) != seq {
                spin_loop();
            }
        }

        // Explicit, so it is obvious that `ctx` must outlive the barrier above.
        drop(ctx);
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        self.shared.running.store(false, Ordering::Release);
        for thread in &self.threads {
            thread.thread().unpark();
        }
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// One `"audio-worker-N"` thread's body: park until a section is published,
/// claim and run items, ack, repeat; exits when `running` clears.
fn worker_loop(shared: &PoolShared, index: usize) {
    // Per-thread, and this thread runs plugin DSP — see `super::thread_role`.
    enter_render_thread();

    let slot = &shared.workers[index];
    let mut last_seq = 0u64;

    loop {
        if !shared.running.load(Ordering::Acquire) {
            return;
        }

        // Acquire: pairs with the caller's release-store, so the `job` read
        // below sees the section this sequence number belongs to.
        let seq = slot.seq.load(Ordering::Acquire);
        if seq != last_seq {
            last_seq = seq;

            // SAFETY: the caller wrote `job` before the `seq` store this load
            // observed, and cannot write it again until this worker's `ack`
            // below.
            let job = unsafe { *shared.job.get() };
            shared.run_claimed(&job);
            slot.ack.store(seq, Ordering::Release);

            // Only here — right after a section — is another one plausibly
            // imminent enough to be worth spinning for. See `SPIN_ROUNDS`.
            let mut spins = 0;
            while spins < SPIN_ROUNDS && slot.seq.load(Ordering::Acquire) == last_seq {
                spin_loop();
                spins += 1;
            }
            continue;
        }

        // Genuinely idle: sleep, don't spin. `unpark` carries a token, so a
        // publish landing between the load above and this call still wakes us.
        park_timeout(PARK_TIMEOUT);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU32;
    use std::thread::current;

    use super::*;

    #[test]
    fn every_item_is_visited_exactly_once() {
        let pool = WorkerPool::new(4);
        let mut items = vec![0u32; 64];
        pool.for_each(&mut items, 5, |index, item| *item = index as u32 + 1);
        assert_eq!(items, (1..=64).collect::<Vec<u32>>());
    }

    #[test]
    fn many_back_to_back_sections_stay_correct() {
        let pool = WorkerPool::new(4);
        let mut items = vec![1u64; 32];
        for _ in 0..2000 {
            pool.for_each(&mut items, 5, |_, item| *item += 1);
        }
        assert!(items.iter().all(|v| *v == 2001), "items: {items:?}");
    }

    #[test]
    fn uneven_item_costs_are_balanced_not_dropped() {
        let pool = WorkerPool::new(3);
        let visits = (0..16).map(|_| AtomicU32::new(0)).collect::<Vec<_>>();
        let mut items = (0..16).collect::<Vec<usize>>();
        pool.for_each(&mut items, 4, |index, item| {
            // One deliberately expensive item, so the claim cursor has to hand
            // the rest to whoever is free rather than partitioning up front.
            let rounds = if index == 0 { 200_000 } else { 10 };
            let mut acc = 0usize;
            for i in 0..rounds {
                acc = acc.wrapping_add(i);
            }
            *item = acc.wrapping_add(index);
            visits[index].fetch_add(1, Ordering::Relaxed);
        });
        assert!(visits.iter().all(|v| v.load(Ordering::Relaxed) == 1));
    }

    #[test]
    fn an_empty_slice_is_a_no_op() {
        let pool = WorkerPool::new(2);
        let mut items: Vec<u32> = Vec::new();
        pool.for_each(&mut items, 4, |_, _| unreachable!());
    }

    #[test]
    fn a_pool_with_no_workers_still_runs_every_item() {
        let pool = WorkerPool::new(0);
        let mut items = vec![0u32; 8];
        pool.for_each(&mut items, 8, |index, item| *item = index as u32);
        assert_eq!(items, (0..8).collect::<Vec<u32>>());
    }

    #[test]
    fn one_runner_runs_everything_inline() {
        let pool = WorkerPool::new(4);
        let caller = current().id();
        let mut items = vec![0u32; 8];
        pool.for_each(&mut items, 1, |index, item| {
            assert_eq!(current().id(), caller);
            *item = index as u32;
        });
        assert_eq!(items, (0..8).collect::<Vec<u32>>());
    }

    #[test]
    fn a_panicking_item_does_not_wedge_the_pool() {
        let pool = WorkerPool::new(2);
        let mut items = vec![0u32; 8];
        pool.for_each(&mut items, 3, |index, item| {
            if index == 3 {
                panic!("item {index} is bad");
            }
            *item = 1;
        });
        // The section still completed, and the pool is reusable afterwards.
        assert_eq!(items[3], 0);
        pool.for_each(&mut items, 3, |_, item| *item = 9);
        assert!(items.iter().all(|v| *v == 9));
    }
}
