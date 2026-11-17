//! Keeps denormal numbers out of audio threads.
//!
//! A decaying filter, reverb or delay ends up computing with denormal (subnormal) floats,
//! which many CPUs handle one or two orders of magnitude slower than normal ones: enough to
//! miss the deadline on a tail that should cost nothing. Every thread that runs audio code,
//! ours or a plugin's, calls [`flush_to_zero`] once before it starts.

/// Makes the calling thread round denormal results to zero and read denormal inputs as zero
/// (FTZ and DAZ on x86-64, FZ on AArch64), for as long as it lives. Does nothing on other
/// architectures. Real-time safe.
pub fn flush_to_zero() {
    #[cfg(target_arch = "x86_64")]
    {
        /// Flush-to-zero and denormals-are-zero in MXCSR. Every x86-64 CPU has both.
        const FTZ_DAZ: u32 = 0x8040;
        let mut csr = 0u32;
        // SAFETY: reads and writes the calling thread's SSE control register through a valid
        // pointer. Only the two denormal bits change; rounding and exception masks stay.
        unsafe {
            std::arch::asm!("stmxcsr [{}]", in(reg) &raw mut csr, options(nostack, preserves_flags));
            csr |= FTZ_DAZ;
            std::arch::asm!("ldmxcsr [{}]", in(reg) &raw const csr, options(nostack, readonly, preserves_flags));
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        /// Flush-to-zero in FPCR.
        const FZ: u64 = 1 << 24;
        // SAFETY: reads and writes the calling thread's floating-point control register; only
        // the flush-to-zero bit changes.
        unsafe {
            let mut fpcr: u64;
            std::arch::asm!("mrs {}, fpcr", out(reg) fpcr, options(nomem, nostack, preserves_flags));
            fpcr |= FZ;
            std::arch::asm!("msr fpcr, {}", in(reg) fpcr, options(nomem, nostack, preserves_flags));
        }
    }
}

#[cfg(all(test, not(loom), any(target_arch = "x86_64", target_arch = "aarch64")))]
mod tests {
    use std::hint::black_box;

    fn halved_smallest() -> f32 {
        black_box(f32::MIN_POSITIVE) * black_box(0.5)
    }

    #[test]
    fn denormal_results_become_zero_on_this_thread_only() {
        std::thread::spawn(|| {
            assert!(halved_smallest() > 0.0, "denormals work by default");
            super::flush_to_zero();
            assert_eq!(halved_smallest(), 0.0);
        })
        .join()
        .unwrap();
        std::thread::spawn(|| assert!(halved_smallest() > 0.0))
            .join()
            .unwrap();
    }
}
