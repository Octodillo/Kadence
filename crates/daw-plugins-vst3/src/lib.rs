//! VST3 plugin hosting.
//!
//! Implements [`PluginFormat`] and [`PluginInstance`] on top of the `vst3` crate, which ships
//! bindings generated from the MIT-licensed VST 3 SDK. It covers `.vst3` bundles, single and
//! split component/controller plugins, audio, note and MIDI events, parameters, state, latency
//! and editors in a floating X11 window.

// The bindings' enum types change width between platforms, so casts that are no-ops on Linux
// are kept for clarity and portability.
#![allow(clippy::unnecessary_cast)]

mod editor;
mod host;
mod instance;
mod module;
mod runloop;
mod util;

/// The integration tests' helper that builds and bundles the example plugins.
#[cfg(test)]
#[path = "../tests/support/mod.rs"]
mod test_support;

use std::path::{Path, PathBuf};

use daw_core::plugin::{
    PluginDescriptor, PluginError, PluginFormat, PluginFormatKind, PluginInstance,
};

pub use instance::Vst3Instance;
pub use module::shutdown;

/// Entry point for the plugin registry.
pub fn format() -> Box<dyn PluginFormat> {
    Box::new(Vst3Format)
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Vst3Format;

impl PluginFormat for Vst3Format {
    fn kind(&self) -> PluginFormatKind {
        PluginFormatKind::Vst3
    }

    /// `$VST3_PATH` (colon separated) first, then the locations from the VST 3 spec for Linux.
    fn default_search_paths(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = std::env::var_os("VST3_PATH")
            .map(|v| {
                // An empty entry (`a::b`, or an empty variable) names no folder.
                std::env::split_paths(&v)
                    .filter(|p| !p.as_os_str().is_empty())
                    .collect()
            })
            .unwrap_or_default();
        if let Some(home) = std::env::var_os("HOME") {
            paths.push(PathBuf::from(home).join(".vst3"));
        }
        paths.push("/usr/lib/vst3".into());
        paths.push("/usr/local/lib/vst3".into());
        paths
    }

    /// Scans one `.vst3` module; the registry finds the modules under the search paths.
    ///
    /// This loads plugin code, so the registry runs it in a separate process.
    fn scan(&self, path: &Path) -> Result<Vec<PluginDescriptor>, PluginError> {
        if !is_module(path) {
            return Err(PluginError::Load(format!(
                "{} is not a VST3 module",
                path.display()
            )));
        }
        instance::scan_module(path)
    }

    /// Runs ModuleExit for the modules no instance uses any more; see [`shutdown`].
    fn shutdown(&self) {
        shutdown();
    }

    fn instantiate(
        &self,
        d: &PluginDescriptor,
        sample_rate: f64,
        max_frames: u32,
    ) -> Result<Box<dyn PluginInstance>, PluginError> {
        if d.format != PluginFormatKind::Vst3 {
            return Err(PluginError::Load(format!(
                "{} is not a VST3 plugin",
                d.name
            )));
        }
        let mut instance = Vst3Instance::new(d)?;
        daw_core::node::Node::prepare(&mut instance, sample_rate, max_frames);
        Ok(Box::new(instance))
    }
}

/// Whether `path` is named as a module, in any case, as the registry's search finds them.
fn is_module(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("vst3"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A module that does not load is a failure that says why, not an empty bundle.
    #[test]
    fn scanning_garbage_reports_why() {
        let dir = tempfile::tempdir().unwrap();
        let module = dir.path().join("Broken.vst3");
        std::fs::write(&module, b"not an elf").unwrap();
        // The extension counts in any case, and a folder is not a module.
        let shouting = dir.path().join("Loud.VST3");
        std::fs::write(&shouting, b"not an elf").unwrap();
        for (path, names) in [
            (module.as_path(), "Broken.vst3"),
            (shouting.as_path(), "Loud.VST3"),
            (dir.path(), "not a VST3 module"),
        ] {
            match Vst3Format.scan(path) {
                Err(PluginError::Load(msg)) => assert!(msg.contains(names), "{msg}"),
                other => panic!("expected a load error, got {other:?}"),
            }
        }
    }
}
