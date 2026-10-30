//! A broken VST3 module for this crate's tests: it has ModuleEntry and ModuleExit but no
//! GetPluginFactory. `nofactory_module_counts` reports how often the first two ran.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};

static ENTRIES: AtomicU32 = AtomicU32::new(0);
static EXITS: AtomicU32 = AtomicU32::new(0);

#[allow(non_snake_case)]
#[unsafe(no_mangle)]
extern "system" fn ModuleEntry(_handle: *mut c_void) -> bool {
    ENTRIES.fetch_add(1, Ordering::Relaxed);
    true
}

#[allow(non_snake_case)]
#[unsafe(no_mangle)]
extern "system" fn ModuleExit() -> bool {
    EXITS.fetch_add(1, Ordering::Relaxed);
    true
}

/// Writes entries and exits so far to `out[0..2]`.
///
/// # Safety
/// `out` must point at two writable `u32`s.
#[unsafe(no_mangle)]
unsafe extern "C" fn nofactory_module_counts(out: *mut u32) {
    let counts = [
        ENTRIES.load(Ordering::Relaxed),
        EXITS.load(Ordering::Relaxed),
    ];
    unsafe { std::ptr::copy_nonoverlapping(counts.as_ptr(), out, 2) };
}
