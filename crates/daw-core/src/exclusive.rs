//! A value two threads take turns on without the audio thread ever blocking.
//!
//! Plugin instances put main-thread calls (`on_main_thread`, state, editors, `prepare`) and
//! audio-thread calls (`process`) on one `&mut self` object, so the two threads must never
//! be inside it at once. [`Exclusive`] gives the audio thread a try-lock that never waits
//! ([`try_audio`](Exclusive::try_audio)) and the main thread a lock that waits for the audio
//! thread to finish its current call ([`main`](Exclusive::main)).

use crate::sync::{AtomicU32, UnsafeCell};
use std::sync::atomic::Ordering;

const FREE: u32 = 0;
const AUDIO: u32 = 1;
const MAIN: u32 = 2;

/// Exclusive access to a `T` shared between a real-time thread and a normal one.
pub struct Exclusive<T> {
    state: AtomicU32,
    value: UnsafeCell<T>,
}

// SAFETY: the state word hands the value to one thread at a time, with acquire/release
// ordering on every hand-off, so `&Exclusive<T>` only ever yields one `&mut T` at once.
unsafe impl<T: Send> Sync for Exclusive<T> {}

impl<T> Exclusive<T> {
    pub fn new(value: T) -> Self {
        Self {
            state: AtomicU32::new(FREE),
            value: UnsafeCell::new(value),
        }
    }

    /// Runs `f` on the value if no other thread holds it, and `None` otherwise. Never waits,
    /// allocates or makes a system call, so the audio thread can use it.
    pub fn try_audio<R>(&self, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        self.state
            .compare_exchange(FREE, AUDIO, Ordering::Acquire, Ordering::Relaxed)
            .ok()?;
        let _release = Release(&self.state);
        // SAFETY: the state word is ours until `_release` drops.
        Some(self.value.with_mut(|v| f(unsafe { &mut *v })))
    }

    /// Runs `f` on the value, first waiting for the audio thread to leave it. The audio
    /// thread holds it for one call at a time, so the wait is short; while `f` runs, the
    /// audio thread's [`try_audio`](Self::try_audio) returns `None`, so keep `f` short too.
    pub fn main<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut spins = 0u32;
        while self
            .state
            .compare_exchange_weak(FREE, MAIN, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spins = spins.saturating_add(1);
            crate::sync::backoff(spins);
        }
        let _release = Release(&self.state);
        // SAFETY: as in `try_audio`.
        self.value.with_mut(|v| f(unsafe { &mut *v }))
    }

    /// Whether some thread is inside the value right now. Only a hint: it can change at once.
    pub fn is_busy(&self) -> bool {
        self.state.load(Ordering::Relaxed) != FREE
    }
}

/// Frees the state word, also when the closure panics.
struct Release<'a>(&'a AtomicU32);

impl Drop for Release<'_> {
    fn drop(&mut self) {
        self.0.store(FREE, Ordering::Release);
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    #[test]
    fn the_audio_side_never_waits_for_the_main_side() {
        let x = Exclusive::new(1);
        assert_eq!(x.try_audio(|v| *v + 1), Some(2));
        x.main(|v| {
            *v = 5;
            assert!(x.is_busy());
            assert_eq!(x.try_audio(|v| *v), None, "the main side holds it");
        });
        assert!(!x.is_busy());
        assert_eq!(x.try_audio(|v| *v), Some(5));
    }

    #[test]
    fn a_panic_inside_releases_it() {
        let x = Exclusive::new(0);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            x.main(|_| panic!("boom"));
        }));
        assert!(r.is_err());
        assert_eq!(x.try_audio(|v| *v), Some(0));
    }
}
