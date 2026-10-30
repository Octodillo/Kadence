//! The plugin registry shared by all formats.
//!
//! - [`PluginRegistry`] holds one [`PluginFormat`] per format and the plugins found so far, and
//!   instantiates them.
//! - [`Scanner`] scans each plugin bundle in a child process, so a plugin that crashes or hangs
//!   while loading cannot take the application down. The child is the `daw-plugin-scan` binary,
//!   or the application itself if it forwards `--scan` to [`run_scan_child`].
//! - [`ScanCache`] remembers each bundle's result, keyed by path and modification time, so
//!   unchanged bundles are not loaded again. Failures are cached too, so a crashing plugin is
//!   skipped until it is updated.

mod cache;
mod candidates;
mod registry;
mod scanner;

pub use cache::{CacheEntry, ScanCache, modified_time};
pub use candidates::{bundle_extension, find_candidates};
pub use registry::{PluginRegistry, ScanReport, default_registry};
pub use scanner::{ScanError, Scanner, kind_from_name, kind_name, run_scan_child};
