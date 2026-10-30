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
            .map(|v| std::env::split_paths(&v).collect())
            .unwrap_or_default();
        if let Some(home) = std::env::var_os("HOME") {
            paths.push(PathBuf::from(home).join(".vst3"));
        }
        paths.push("/usr/lib/vst3".into());
        paths.push("/usr/local/lib/vst3".into());
        paths
    }

    /// Scans one `.vst3` module, or every module found under a directory.
    ///
    /// This loads plugin code, so the registry runs it in a separate process.
    ///
    /// A module that does not load fails the scan. Under a directory, modules that load still
    /// count; the scan fails only when every module failed, with the first one's error.
    fn scan(&self, path: &Path) -> Result<Vec<PluginDescriptor>, PluginError> {
        if is_module(path) {
            return instance::scan_module(path);
        }
        let mut out = Vec::new();
        let mut first_error = None;
        if path.is_dir() {
            for module in find_modules(path) {
                match instance::scan_module(&module) {
                    Ok(found) => out.extend(found),
                    Err(e) => {
                        log::warn!("vst3 scan: {e}");
                        first_error.get_or_insert(e);
                    }
                }
            }
        }
        match first_error {
            Some(e) if out.is_empty() => Err(e),
            _ => Ok(out),
        }
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

fn is_module(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "vst3")
}

/// Finds `.vst3` modules below `dir` without descending into the bundles themselves.
fn find_modules(dir: &Path) -> Vec<PathBuf> {
    const MAX_DEPTH: usize = 8;
    let mut found = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        for path in entries {
            if is_module(&path) {
                found.push(path);
            } else if path.is_dir() && depth < MAX_DEPTH {
                stack.push((path, depth + 1));
            }
        }
    }
    found.sort();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_bundles_but_not_inside_them() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("vendor/A.vst3/Contents/x86_64-linux");
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::create_dir_all(bundle.join("Nested.vst3")).unwrap();
        std::fs::write(dir.path().join("B.vst3"), b"").unwrap();
        std::fs::write(dir.path().join("readme.txt"), b"").unwrap();
        let found = find_modules(dir.path());
        assert_eq!(
            found,
            vec![dir.path().join("B.vst3"), dir.path().join("vendor/A.vst3")]
        );
    }

    /// A module that does not load is a failure that says why, not an empty bundle.
    #[test]
    fn scanning_garbage_reports_why() {
        let dir = tempfile::tempdir().unwrap();
        let module = dir.path().join("Broken.vst3");
        std::fs::write(&module, b"not an elf").unwrap();
        for path in [&module, &dir.path().to_path_buf()] {
            match Vst3Format.scan(path) {
                Err(PluginError::Load(msg)) => assert!(msg.contains("Broken.vst3"), "{msg}"),
                other => panic!("expected a load error, got {other:?}"),
            }
        }
        // A directory with no modules is just empty.
        let empty = tempfile::tempdir().unwrap();
        assert!(Vst3Format.scan(empty.path()).unwrap().is_empty());
    }
}
