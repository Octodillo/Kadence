//! LV2 plugin hosting.
//!
//! [`Lv2Format`] implements [`PluginFormat`]: it scans LV2 bundles and creates
//! [`Lv2Instance`]s, which implement [`PluginInstance`] on top of `livi`/`lilv`.
//!
//! What is covered: audio and CV ports, atom MIDI in and out, control ports as
//! parameters (sample accurate, by splitting the block at parameter changes),
//! `time:Position` for tempo-synced plugins, the latency port, the worker and
//! state extensions, and plugin UIs as floating windows. UIs are hosted
//! natively: `ui:X11UI` gets its own top-level X11 window (XWayland is fine on
//! Plasma Wayland) and kxstudio external UIs open their own window. suil is not
//! used: there is no host toolkit for it to embed into, and the Gtk/Qt5 UIs it
//! could wrap cannot live in a Qt 6 process anyway.

mod atom;
mod instance;
mod midi;
mod ring;
mod scan;
mod ui;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use daw_core::plugin::{
    PluginDescriptor, PluginError, PluginFormat, PluginFormatKind, PluginInstance,
};

pub use instance::Lv2Instance;

/// Returns the LV2 format, ready to be registered with the plugin registry.
pub fn format() -> Box<dyn PluginFormat> {
    Box::new(Lv2Format::new())
}

/// The LV2 plugin format.
pub struct Lv2Format {
    shared: Arc<Shared>,
}

impl Lv2Format {
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared::default()),
        }
    }
}

impl Default for Lv2Format {
    fn default() -> Self {
        Self::new()
    }
}

/// Caches shared by the format and every instance it creates. Only touched off
/// the audio thread.
#[derive(Default)]
pub(crate) struct Shared {
    /// One lilv world per bundle, so a plugin's own bundle is all that gets
    /// parsed when it is instantiated.
    worlds: Mutex<HashMap<PathBuf, Arc<livi::World>>>,
    /// Host features (URID map, options, worker thread) keyed by max block
    /// size. Sharing them gives every instance the same URID map and one worker
    /// thread instead of one each.
    features: Mutex<HashMap<u32, Arc<livi::Features>>>,
    /// UI libraries stay loaded once opened: unloading toolkit-based UI
    /// libraries is a well-known source of crashes in LV2 hosts.
    pub(crate) ui_libraries: Mutex<HashMap<PathBuf, Arc<libloading::Library>>>,
}

impl Shared {
    pub(crate) fn world(&self, bundle: &Path) -> Arc<livi::World> {
        let mut worlds = self.worlds.lock().unwrap_or_else(|e| e.into_inner());
        worlds
            .entry(bundle.to_path_buf())
            .or_insert_with(|| Arc::new(livi::World::with_load_bundle(&bundle_uri(bundle))))
            .clone()
    }

    pub(crate) fn features(
        &self,
        world: &livi::World,
        max_frames: u32,
    ) -> Result<Arc<livi::Features>, PluginError> {
        let mut features = self.features.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(f) = features.get(&max_frames) {
            return Ok(f.clone());
        }
        let f = livi::FeaturesBuilder {
            min_block_length: 1,
            max_block_length: max_frames.max(1) as usize,
        }
        .try_build(world)
        .map_err(|e| PluginError::Load(e.to_string()))?;
        features.insert(max_frames, f.clone());
        Ok(f)
    }
}

impl PluginFormat for Lv2Format {
    fn kind(&self) -> PluginFormatKind {
        PluginFormatKind::Lv2
    }

    fn default_search_paths(&self) -> Vec<PathBuf> {
        scan::default_search_paths()
    }

    fn scan(&self, path: &Path) -> Vec<PluginDescriptor> {
        scan::scan(path)
    }

    fn instantiate(
        &self,
        d: &PluginDescriptor,
        sample_rate: f64,
        max_frames: u32,
    ) -> Result<Box<dyn PluginInstance>, PluginError> {
        Ok(Box::new(Lv2Instance::new(
            self.shared.clone(),
            d,
            sample_rate,
            max_frames,
        )?))
    }
}

/// `file:///path/to/bundle.lv2/`, the form lilv wants for bundle URIs.
pub(crate) fn bundle_uri(bundle: &Path) -> String {
    let mut uri = String::from("file://");
    for b in bundle.to_string_lossy().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                uri.push(b as char)
            }
            _ => uri.push_str(&format!("%{b:02X}")),
        }
    }
    if !uri.ends_with('/') {
        uri.push('/');
    }
    uri
}

/// Turns a `file://` URI from lilv back into a path.
pub(crate) fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    use std::os::unix::ffi::OsStringExt;
    Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_uris_round_trip() {
        let p = Path::new("/home/me/My Plugins/ünï.lv2");
        let uri = bundle_uri(p);
        assert_eq!(uri, "file:///home/me/My%20Plugins/%C3%BCn%C3%AF.lv2/");
        assert_eq!(
            uri_to_path(&uri).unwrap(),
            Path::new("/home/me/My Plugins/ünï.lv2/")
        );
        assert_eq!(
            uri_to_path("file://localhost/usr/lib/lv2/a.so").unwrap(),
            Path::new("/usr/lib/lv2/a.so")
        );
    }
}
