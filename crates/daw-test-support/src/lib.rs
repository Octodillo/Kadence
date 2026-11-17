//! Helpers shared by the workspace's tests, so every crate checks the same
//! things the same way. Only ever a dev-dependency.
//!
//! - [`no_alloc`]: run real-time code with the allocator forbidden, and fail
//!   the test if it allocates.
//! - [`golden`]: compare rendered audio with a stored file.
//! - [`lv2`]: find the LV2 plugins some tests load, or skip.
//!
//! Environment switches read here are listed in `docs/testing.md`.

pub mod golden;
pub mod lv2;
pub mod no_alloc;
