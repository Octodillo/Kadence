//! Checked no-allocation sections for real-time code.
//!
//! The workspace builds `assert_no_alloc` in its warn mode, so that one
//! allocation does not abort the whole test binary. In that mode
//! `assert_no_alloc::assert_no_alloc` on its own only prints a line: a test
//! that calls it and nothing else cannot fail. [`assert_no_alloc`] here
//! compares the violation count before and after and panics, and
//! [`allocations_in`] returns the count for tests that want to assert on it.
//!
//! A test binary that uses either must install the checking allocator, with
//! [`install_checker!`](crate::install_checker). Both functions verify on
//! first use that it is installed, so a check can never pass just because
//! nothing was counting.

use std::cell::Cell;

pub use assert_no_alloc::AllocDisabler;
use assert_no_alloc::violation_count;

/// Installs the allocator that [`assert_no_alloc`] and [`allocations_in`]
/// count with. Call once at the top level of a test binary.
#[macro_export]
macro_rules! install_checker {
    () => {
        #[global_allocator]
        static NO_ALLOC_CHECKER: $crate::no_alloc::AllocDisabler = $crate::no_alloc::AllocDisabler;
    };
}

/// Runs `f` with allocator calls forbidden on this thread and returns the
/// number of calls (allocations, reallocations and frees) it made anyway.
/// For tests that expect a count other than zero, or add counts up; the
/// usual check is [`assert_no_alloc`].
///
/// # Panics
/// If the binary has not installed the checker.
pub fn allocations_in(f: impl FnOnce()) -> u32 {
    assert_checker_installed();
    count(f).1
}

/// Runs `f` with allocator calls forbidden on this thread and returns its
/// result. `what` names the code under test in the failure message.
///
/// # Panics
/// If `f` allocated, reallocated or freed, or if the binary has not installed
/// the checker.
#[track_caller]
pub fn assert_no_alloc<R>(what: &str, f: impl FnOnce() -> R) -> R {
    assert_checker_installed();
    let (result, violations) = count(f);
    assert_eq!(
        violations, 0,
        "{what} called the allocator {violations} time(s)"
    );
    result
}

fn count<R>(f: impl FnOnce() -> R) -> (R, u32) {
    let before = violation_count();
    let result = assert_no_alloc::assert_no_alloc(f);
    (result, violation_count().wrapping_sub(before))
}

/// Panics unless an allocation inside a forbidden section is counted, which
/// it is only when the test binary's global allocator is [`AllocDisabler`].
/// Checked once per thread: the count is per thread.
fn assert_checker_installed() {
    thread_local! {
        static CHECKED: Cell<bool> = const { Cell::new(false) };
    }
    if CHECKED.get() {
        return;
    }
    // Allocated out here, so the forbidden section holds exactly one call.
    let probe = std::hint::black_box(Box::new(0u8));
    let ((), seen) = count(|| drop(probe));
    eprintln!(
        "daw-test-support: the allocator warning above (if any) is the check that the \
         no-alloc checker is installed, not a failure"
    );
    assert!(
        seen > 0,
        "the no-alloc checker is not installed in this test binary; \
         add `daw_test_support::install_checker!();`"
    );
    CHECKED.set(true);
}
