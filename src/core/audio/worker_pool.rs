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
//! Each worker has one state word, `IDLE → PUBLISHED → TAKEN → IDLE`. One
//! section (`for_each` call) runs:
//!
//! 1. the caller writes the type-erased [`Job`] and resets the claim cursor;
//! 2. it sets each participating worker's state to `PUBLISHED` and unparks it;
//! 3. a worker that wakes swaps `PUBLISHED → TAKEN`; from then on it may read
//!    the job;
//! 4. every runner — the workers that took the job *and* the caller — claims
//!    item indices off the shared `next` cursor with `fetch_add` and runs them
//!    until exhausted, and each worker then stores `IDLE`;
//! 5. the caller, done with its share, **revokes** every worker still
//!    `PUBLISHED` (`PUBLISHED → IDLE`) and spins only on those that took the
//!    job, until they are `IDLE` again.
//!
//! The two swaps in steps 3 and 5 race on one word, so exactly one wins: a
//! worker either took the job and is waited for, or was revoked and never
//! reads it. A worker only touches `job` while `TAKEN`, and the caller never
//! rewrites `job` until every worker is `IDLE`, so there is no such thing as a
//! straggler from a previous section, and the `JobCtx` on the caller's stack
//! outlives every dereference of it.
//!
//! The revoke is what keeps a worker that is slow to wake from holding up the
//! block: the caller runs whatever it didn't take, and the late worker finds
//! its job gone (`docs/170-multicore-scheduling.md`, issue #32). What is left
//! is a worker that took the job and then lost its CPU — mid-item, its voice's
//! own timer shows it. [`WorkerPool::for_each`] returns the time the caller
//! spent at the barrier, for the overrun journal.

use std::cell::UnsafeCell;
use std::hint::spin_loop;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::null;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::thread::{Builder, JoinHandle, park_timeout};
use std::time::{Duration, Instant};

use super::enter_render_thread;

/// A worker's state between sections, or after it was revoked: it must not
/// read `job`.
const IDLE: u8 = 0;
/// The caller has published a section for this worker, which hasn't woken yet.
const PUBLISHED: u8 = 1;
/// The worker took the section and may read `job` until it stores [`IDLE`].
const TAKEN: u8 = 2;

/// How long a worker spins on its state *after finishing a section* before it
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

/// Per-worker handshake state: [`IDLE`], [`PUBLISHED`] or [`TAKEN`]. The
/// caller moves it `IDLE → PUBLISHED` and revokes `PUBLISHED → IDLE`; the
/// worker moves it `PUBLISHED → TAKEN → IDLE`.
struct WorkerSlot {
    /// The state word both sides swap on.
    state: AtomicU8,
}

/// Shared state between the calling (audio) thread and the workers.
struct PoolShared {
    /// The current section's [`Job`] — only mutated during the handshake window.
    job: UnsafeCell<Job>,
    /// Claim cursor: `fetch_add` hands each item index to exactly one runner.
    next: AtomicUsize,
    /// Cleared on drop to retire the workers.
    running: AtomicBool,
    /// Per-worker handshake slots.
    workers: Vec<WorkerSlot>,
}

// SAFETY: `job` is the only non-atomic field. The caller writes it only while
// every worker's state is `IDLE` — i.e. while no worker is inside the window
// where it may read `job` — and each worker's read is ordered after its own
// `PUBLISHED → TAKEN` swap (acquire), which reads the caller's release-store of
// `PUBLISHED`. The raw pointer inside `Job` is never dereferenced outside that
// window.
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
    /// Spawns `worker_count` `"audio-worker-N"` threads. Each calls
    /// `on_start` on itself before its first section and holds what it returns
    /// until the thread exits, so a guard that must be dropped on its own
    /// thread (a workgroup membership) can be. `0` is valid and makes
    /// [`for_each`](Self::for_each) run everything on the calling thread.
    pub(crate) fn new<F, G>(worker_count: usize, on_start: F) -> Self
    where
        F: Fn() -> G + Send + Sync + 'static,
    {
        let on_start = Arc::new(on_start);
        let shared = Arc::new(PoolShared {
            job: UnsafeCell::new(Job::EMPTY),
            next: AtomicUsize::new(0),
            running: AtomicBool::new(true),
            workers: (0..worker_count)
                .map(|_| WorkerSlot {
                    state: AtomicU8::new(IDLE),
                })
                .collect(),
        });

        let threads = (0..worker_count)
            .map(|index| {
                let shared = shared.clone();
                let on_start = on_start.clone();
                Builder::new()
                    .name(format!("audio-worker-{index}"))
                    .spawn(move || {
                        let _guard = on_start();
                        worker_loop(&shared, index);
                    })
                    .expect("failed to spawn audio worker thread")
            })
            .collect();

        Self { shared, threads }
    }

    /// Worker threads in the pool, not counting the calling thread.
    pub(crate) fn worker_count(&self) -> usize {
        self.threads.len()
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
    ///
    /// Returns how long the caller waited at the barrier after its own share,
    /// for workers still rendering — zero when it ran everything inline.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn for_each<T, F>(&self, items: &mut [T], runners: usize, f: F) -> Duration
    where
        T: Send,
        F: Fn(usize, &mut T) + Sync,
    {
        let len = items.len();
        if len == 0 {
            return Duration::ZERO;
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
            return Duration::ZERO;
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

        // SAFETY: no worker may read `job` until it takes the section published
        // below, and the previous section's barrier left every worker `IDLE`,
        // so none is still reading the last one.
        unsafe { *self.shared.job.get() = job };
        self.shared.next.store(0, Ordering::Relaxed);

        for (slot, thread) in self.shared.workers[..helpers].iter().zip(&self.threads) {
            // Release: pairs with the worker's acquire swap, publishing `job`
            // and the reset cursor along with the state.
            slot.state.store(PUBLISHED, Ordering::Release);
            thread.thread().unpark();
        }

        // The calling thread is a runner too — it never parks, so it usually
        // finishes its share around the time the workers finish theirs.
        self.shared.run_claimed(&job);

        let barrier = Instant::now();
        for slot in &self.shared.workers[..helpers] {
            // Not woken yet: revoke, so it never reads `job` and nobody waits
            // for its wake-up — the items it would have run are done.
            if slot
                .state
                .compare_exchange(PUBLISHED, IDLE, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                continue;
            }
            // It took the section: wait until it is out of it.
            while slot.state.load(Ordering::Acquire) != IDLE {
                spin_loop();
            }
        }
        let waited = barrier.elapsed();

        // Explicit, so it is obvious that `ctx` must outlive the barrier above.
        drop(ctx);
        waited
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
/// take it, claim and run items, go idle, repeat; exits when `running` clears.
/// A section revoked before this worker woke is simply never seen.
fn worker_loop(shared: &PoolShared, index: usize) {
    // Per-thread, and this thread runs plugin DSP — see `super::thread_role`.
    enter_render_thread();

    let slot = &shared.workers[index];

    loop {
        if !shared.running.load(Ordering::Acquire) {
            return;
        }

        // Acquire: pairs with the caller's release-store of `PUBLISHED`, so
        // the `job` read below sees the section just published. Losing the
        // race to the caller's revoke means it ran this worker's share itself.
        if slot
            .state
            .compare_exchange(PUBLISHED, TAKEN, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            // SAFETY: the caller wrote `job` before publishing, and cannot
            // write it again until this worker stores `IDLE` below.
            let job = unsafe { *shared.job.get() };
            shared.run_claimed(&job);
            slot.state.store(IDLE, Ordering::Release);

            // Only here — right after a section — is another one plausibly
            // imminent enough to be worth spinning for. See `SPIN_ROUNDS`.
            let mut spins = 0;
            while spins < SPIN_ROUNDS && slot.state.load(Ordering::Relaxed) != PUBLISHED {
                spin_loop();
                spins += 1;
            }
            continue;
        }

        // Genuinely idle: sleep, don't spin. `unpark` carries a token, so a
        // publish landing between the swap above and this call still wakes
        // us. A revoked section leaves its token behind: one wake-up for
        // nothing, then back here.
        park_timeout(PARK_TIMEOUT);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU32;
    use std::thread::{current, sleep};

    use super::*;

    #[test]
    fn every_item_is_visited_exactly_once() {
        let pool = WorkerPool::new(4, || ());
        let mut items = vec![0u32; 64];
        pool.for_each(&mut items, 5, |index, item| *item = index as u32 + 1);
        assert_eq!(items, (1..=64).collect::<Vec<u32>>());
    }

    #[test]
    fn many_back_to_back_sections_stay_correct() {
        let pool = WorkerPool::new(4, || ());
        let mut items = vec![1u64; 32];
        for _ in 0..2000 {
            pool.for_each(&mut items, 5, |_, item| *item += 1);
        }
        assert!(items.iter().all(|v| *v == 2001), "items: {items:?}");
    }

    #[test]
    fn uneven_item_costs_are_balanced_not_dropped() {
        let pool = WorkerPool::new(3, || ());
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
        let pool = WorkerPool::new(2, || ());
        let mut items: Vec<u32> = Vec::new();
        pool.for_each(&mut items, 4, |_, _| unreachable!());
    }

    #[test]
    fn a_pool_with_no_workers_still_runs_every_item() {
        let pool = WorkerPool::new(0, || ());
        let mut items = vec![0u32; 8];
        pool.for_each(&mut items, 8, |index, item| *item = index as u32);
        assert_eq!(items, (0..8).collect::<Vec<u32>>());
    }

    #[test]
    fn one_runner_runs_everything_inline() {
        let pool = WorkerPool::new(4, || ());
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
        let pool = WorkerPool::new(2, || ());
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

    /// Issue #32: a worker that hasn't woken when the caller finishes is
    /// revoked, not waited for. A worker stuck in its start hook stands in for
    /// one the scheduler is slow to run.
    #[test]
    fn a_worker_slow_to_wake_does_not_hold_up_the_section() {
        let pool = WorkerPool::new(1, || sleep(Duration::from_millis(500)));
        let caller = current().id();
        let mut items = vec![0u32; 4];
        let started = Instant::now();
        let waited = pool.for_each(&mut items, 2, |index, item| {
            assert_eq!(current().id(), caller, "the sleeping worker ran an item");
            *item = index as u32 + 1;
        });
        assert!(
            started.elapsed() < Duration::from_millis(250),
            "waited {:?} for a worker that never woke",
            started.elapsed()
        );
        assert_eq!(items, vec![1, 2, 3, 4]);
        assert!(waited < Duration::from_millis(250));

        // Once awake, the worker finds its section revoked and the pool still
        // works.
        sleep(Duration::from_millis(600));
        pool.for_each(&mut items, 2, |_, item| *item = 7);
        assert!(items.iter().all(|v| *v == 7));
    }

    #[test]
    fn an_inline_run_waits_at_no_barrier() {
        let pool = WorkerPool::new(2, || ());
        let mut items = vec![0u32; 4];
        assert_eq!(
            pool.for_each(&mut items, 1, |_, item| *item = 1),
            Duration::ZERO
        );
    }

    #[test]
    fn each_worker_holds_its_start_guard_for_its_life() {
        /// Counts itself in on creation and out on drop.
        struct Guard(Arc<AtomicUsize>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::Relaxed);
            }
        }

        let live = Arc::new(AtomicUsize::new(0));
        let hook_live = live.clone();
        let pool = WorkerPool::new(3, move || {
            hook_live.fetch_add(1, Ordering::Relaxed);
            Guard(hook_live.clone())
        });
        // Workers start on their own time — a section no longer waits for one
        // that hasn't woken — so give them a moment.
        let deadline = Instant::now() + Duration::from_secs(5);
        while live.load(Ordering::Relaxed) < 3 && Instant::now() < deadline {
            sleep(Duration::from_millis(1));
        }
        assert_eq!(pool.worker_count(), 3);
        assert_eq!(live.load(Ordering::Relaxed), 3);
        drop(pool);
        assert_eq!(
            live.load(Ordering::Relaxed),
            0,
            "guards dropped as workers exit"
        );
    }
}
