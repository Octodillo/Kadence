//! A global allocator that counts allocations made inside a backend cycle.
//!
//! `assert_no_alloc` (in its `warn_debug` mode) prints a line to stderr from
//! the real-time thread on every cycle that allocates. A plugin that
//! allocates per block then writes to stderr every period, which costs more
//! than the allocation being measured and can cause the xruns it hides. This
//! allocator only counts.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};

/// Set by the first allocator call that goes through [`RtAllocCounter`]: any
/// process that installed it has allocated long before it opens a backend.
static INSTALLED: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// Nesting depth of [`forbid`] on this thread.
    static FORBID: Cell<u32> = const { Cell::new(0) };
    /// Allocator calls made on this thread while `FORBID` was non-zero.
    static VIOLATIONS: Cell<u64> = const { Cell::new(0) };
}

/// Wraps the system allocator and counts every allocation, reallocation and
/// deallocation made inside a backend cycle, without printing anything. The
/// count shows up in
/// [`BackendStats::rt_alloc_violations`](crate::BackendStats::rt_alloc_violations)
/// when it is installed in a binary or test:
///
/// ```
/// #[global_allocator]
/// static ALLOC: daw_backend::RtAllocCounter = daw_backend::RtAllocCounter;
/// ```
pub struct RtAllocCounter;

impl RtAllocCounter {
    /// Whether allocations in cycles are being counted: this allocator is
    /// the global one.
    pub(crate) fn counting() -> bool {
        INSTALLED.load(Ordering::Relaxed)
    }

    #[inline]
    fn check() {
        // Read before writing: after the first call this never dirties the
        // cache line every thread's allocations read.
        if !INSTALLED.load(Ordering::Relaxed) {
            INSTALLED.store(true, Ordering::Relaxed);
        }
        // `try_with`: the allocator also runs while thread locals are torn down.
        let forbidden = FORBID.try_with(|f| f.get() > 0).unwrap_or(false);
        if forbidden {
            let _ = VIOLATIONS.try_with(|v| v.set(v.get() + 1));
        }
    }
}

// SAFETY: every call forwards to `System` unchanged; the check only touches
// an atomic and const-initialised thread locals, which never allocate.
unsafe impl GlobalAlloc for RtAllocCounter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::check();
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::check();
        // SAFETY: as for `alloc`.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        Self::check();
        // SAFETY: the caller upholds `GlobalAlloc::dealloc`'s contract.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::check();
        // SAFETY: the caller upholds `GlobalAlloc::realloc`'s contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// Allocator calls caught on this thread so far.
pub(crate) fn violations() -> u64 {
    VIOLATIONS.with(Cell::get)
}

/// Runs `f` with allocations counted as violations on this thread. The
/// depth is restored even if `f` unwinds.
pub(crate) fn forbid<R>(f: impl FnOnce() -> R) -> R {
    struct Depth;
    impl Drop for Depth {
        fn drop(&mut self) {
            FORBID.with(|d| d.set(d.get() - 1));
        }
    }
    FORBID.with(|d| d.set(d.get() + 1));
    let _depth = Depth;
    f()
}
