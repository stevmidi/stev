//! Denormal flushing for the audio-path threads.
//!
//! A reverb, delay or filter tail decaying toward silence eventually produces
//! *subnormal* floats (magnitude below ~1e-38). On x86 those leave the fast
//! path: the CPU takes a microcode assist costing on the order of a hundred
//! cycles per operation, so a plugin can get an order of magnitude slower
//! precisely while it is fading out and doing nothing audible. It is the classic
//! "why does the meter climb *after* I stop playing".
//!
//! Setting `FTZ` (flush results to zero) and `DAZ` (treat inputs as zero) in
//! `MXCSR` removes the assist entirely, at the cost of turning values that were
//! already inaudible into exact zero. Every DAW does this, and the VST3 spec
//! makes it the host's job.
//!
//! `MXCSR` is **per thread**, so every thread that runs DSP has to set it: the
//! `cpal` callback thread (which we don't create, so it is set on entry to each
//! callback — a couple of dozen cycles against a block of millions) and each of
//! the [`WorkerPool`](super::WorkerPool)'s workers.
//!
//! On aarch64 this is a no-op: Apple Silicon handles subnormals at close to full
//! speed, so there is nothing to buy and `FPCR.FZ` would only cost precision.

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
use std::arch::asm;

/// Enables flush-to-zero and denormals-are-zero for the **calling thread**.
/// Cheap enough to call per audio callback; a no-op off x86.
pub(crate) fn flush_denormals_to_zero() {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        // `MXCSR` bit 15 = FTZ, bit 6 = DAZ. Read/modify/write rather than a
        // blind store, so the thread's rounding mode and exception masks
        // survive.
        const FTZ_DAZ: u32 = 0x8040;
        let mut csr: u32 = 0;
        // SAFETY: `stmxcsr`/`ldmxcsr` only read and write the 32 bits at the
        // address given, which is a live local. `_mm_setcsr` would be the safe
        // spelling but is deprecated.
        unsafe {
            asm!(
                "stmxcsr [{ptr}]",
                ptr = in(reg) &raw mut csr,
                options(nostack, preserves_flags),
            );
            csr |= FTZ_DAZ;
            asm!(
                "ldmxcsr [{ptr}]",
                ptr = in(reg) &raw const csr,
                options(nostack, preserves_flags, readonly),
            );
        }
    }
}

#[cfg(all(test, any(target_arch = "x86", target_arch = "x86_64")))]
mod tests {
    use std::hint::black_box;

    use super::*;

    /// Halving the smallest *normal* float lands in subnormal range, so it is
    /// exactly zero with the flush on and non-zero with it off. `black_box`
    /// keeps the multiply out of the constant folder, which does not model
    /// `MXCSR`.
    fn halve_the_smallest_normal() -> f32 {
        black_box(f32::MIN_POSITIVE) * black_box(0.5f32)
    }

    #[test]
    fn the_flush_changes_behaviour_on_the_calling_thread() {
        // Each test runs on its own thread, which starts with the default
        // `MXCSR` — no FTZ, no DAZ.
        assert_ne!(
            halve_the_smallest_normal(),
            0.0,
            "expected a subnormal before the flush is enabled"
        );

        flush_denormals_to_zero();

        assert_eq!(
            halve_the_smallest_normal(),
            0.0,
            "expected the subnormal to be flushed to zero"
        );
    }

    #[test]
    fn enabling_it_twice_is_harmless() {
        flush_denormals_to_zero();
        flush_denormals_to_zero();
        assert_eq!(halve_the_smallest_normal(), 0.0);
    }

    #[test]
    fn normal_arithmetic_is_untouched() {
        flush_denormals_to_zero();
        assert_eq!(black_box(0.1f32) + black_box(0.2f32), 0.1f32 + 0.2f32);
        assert_eq!(black_box(1.0f32) / black_box(3.0f32), 1.0f32 / 3.0f32);
    }
}
