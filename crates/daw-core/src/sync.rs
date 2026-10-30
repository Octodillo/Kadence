//! Atomics and cells that switch to `loom`'s model-checked versions under `--cfg loom`.

#[cfg(loom)]
pub(crate) use loom::cell::UnsafeCell;
#[cfg(loom)]
pub(crate) use loom::sync::Arc;
#[cfg(loom)]
pub(crate) use loom::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize};

#[cfg(not(loom))]
pub(crate) use std::sync::Arc;
#[cfg(not(loom))]
pub(crate) use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize};

/// `std::cell::UnsafeCell` with loom's closure-based API.
#[cfg(not(loom))]
#[derive(Debug)]
pub(crate) struct UnsafeCell<T>(std::cell::UnsafeCell<T>);

#[cfg(not(loom))]
impl<T> UnsafeCell<T> {
    pub(crate) fn new(v: T) -> Self {
        Self(std::cell::UnsafeCell::new(v))
    }

    pub(crate) fn with_mut<R>(&self, f: impl FnOnce(*mut T) -> R) -> R {
        f(self.0.get())
    }
}

/// Keeps the producer and consumer indices on separate cache lines.
#[derive(Debug, Default)]
#[repr(align(128))]
pub(crate) struct CachePadded<T>(pub(crate) T);

impl<T> std::ops::Deref for CachePadded<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

/// Waits a little before retrying a spin loop: a few CPU pause hints, then yielding the
/// thread so a holder that was preempted can finish.
#[cfg(not(loom))]
pub(crate) fn backoff(spins: u32) {
    if spins < 64 {
        std::hint::spin_loop();
    } else {
        std::thread::yield_now();
    }
}

#[cfg(loom)]
pub(crate) fn backoff(_spins: u32) {
    loom::thread::yield_now();
}
