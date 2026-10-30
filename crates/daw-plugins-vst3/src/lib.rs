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
    fn scan(&self, path: &Path) -> Vec<PluginDescriptor> {
        let mut out = Vec::new();
        if is_module(path) {
            out.extend(instance::scan_module(path));
        } else if path.is_dir() {
            for module in find_modules(path) {
                out.extend(instance::scan_module(&module));
            }
        }
        out
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

    #[test]
    fn scanning_garbage_finds_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Broken.vst3"), b"not an elf").unwrap();
        assert!(Vst3Format.scan(dir.path()).is_empty());
    }
}
