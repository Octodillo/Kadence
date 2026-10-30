//! CLAP plugin hosting.
//!
//! [`ClapFormat`] implements [`PluginFormat`]: it finds `.clap` files, lists the plugins in each
//! one and instantiates them as [`ClapInstance`]s, which implement
//! [`daw_core::plugin::PluginInstance`].
//!
//! # Threads
//!
//! CLAP splits a plugin's functions between a main thread and an audio thread. Here the main
//! thread is the thread that called [`PluginFormat::instantiate`]. Every [`ClapInstance`] method
//! except [`Node::process`](daw_core::node::Node::process) and
//! [`Node::reset`](daw_core::node::Node::reset) must be called on that thread, and the host must
//! call [`PluginInstance::on_main_thread`](daw_core::plugin::PluginInstance::on_main_thread)
//! regularly (about 60 times a second) so that plugin callbacks, timers, file descriptors and
//! editor events are serviced.

mod editor;
mod events;
mod host;
mod instance;

use std::ffi::CString;
use std::path::{Path, PathBuf};

use clack_extensions::audio_ports::{AudioPortFlags, AudioPortInfoBuffer, PluginAudioPorts};
use clack_host::prelude::{HostInfo, PluginEntry};
use daw_core::plugin::{
    PluginDescriptor, PluginError, PluginFormat, PluginFormatKind, PluginInstance,
};

pub use instance::ClapInstance;

/// The CLAP plugin format.
#[derive(Debug, Default, Clone, Copy)]
pub struct ClapFormat;

/// The CLAP format, boxed for a plugin registry.
pub fn format() -> Box<dyn PluginFormat> {
    Box::new(ClapFormat)
}

impl PluginFormat for ClapFormat {
    fn kind(&self) -> PluginFormatKind {
        PluginFormatKind::Clap
    }

    /// `$CLAP_PATH`, then `~/.clap` and `/usr/lib/clap`, as the CLAP spec lays out.
    fn default_search_paths(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = std::env::var_os("CLAP_PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        if let Some(home) = std::env::var_os("HOME") {
            paths.push(PathBuf::from(home).join(".clap"));
        }
        paths.push(PathBuf::from("/usr/lib/clap"));
        paths
    }

    /// Lists the plugins in one `.clap` file. Each plugin is instantiated briefly to read its
    /// audio ports, so call this in a scanner process that may crash.
    fn scan(&self, path: &Path) -> Vec<PluginDescriptor> {
        scan_file(path).unwrap_or_default()
    }

    fn instantiate(
        &self,
        d: &PluginDescriptor,
        sample_rate: f64,
        max_frames: u32,
    ) -> Result<Box<dyn PluginInstance>, PluginError> {
        if d.format != PluginFormatKind::Clap {
            return Err(PluginError::Load(format!("{} is not a CLAP plugin", d.id)));
        }
        let entry = load_entry(&d.path)?;
        let mut instance = ClapInstance::new(&entry, d.clone())?;
        instance.try_prepare(sample_rate, max_frames)?;
        Ok(Box::new(instance))
    }
}

pub(crate) fn load_entry(path: &Path) -> Result<PluginEntry, PluginError> {
    // SAFETY: loading a plugin runs its code. That is the point of a plugin host; scans run in a
    // separate process so a misbehaving library cannot take the application down.
    unsafe { PluginEntry::load(path) }
        .map_err(|e| PluginError::Load(format!("{}: {e}", path.display())))
}

pub(crate) fn host_info() -> HostInfo {
    HostInfo::new(
        "Kadence",
        "Kadence",
        "https://github.com/Octodillo/Kadence",
        env!("CARGO_PKG_VERSION"),
    )
    .expect("host info has no NUL bytes")
}

fn scan_file(path: &Path) -> Option<Vec<PluginDescriptor>> {
    let entry = load_entry(path).ok()?;
    let factory = entry.get_plugin_factory()?;
    let mut found = Vec::new();
    for desc in factory.plugin_descriptors() {
        let Some(id) = desc.id().and_then(|id| id.to_str().ok()) else {
            continue;
        };
        let is_instrument = desc
            .features()
            .any(|f| f.to_bytes() == clack_host::plugin::features::INSTRUMENT.to_bytes());
        let mut d = PluginDescriptor {
            format: PluginFormatKind::Clap,
            id: id.to_owned(),
            name: lossy(desc.name()).unwrap_or_else(|| id.to_owned()),
            vendor: lossy(desc.vendor()).unwrap_or_default(),
            path: path.to_owned(),
            is_instrument,
            ports: (0, 0),
        };
        d.ports = main_ports(&entry, id, is_instrument).unwrap_or((0, 0));
        found.push(d);
    }
    Some(found)
}

/// Which port the graph connects to: the one flagged main, else the first. Many effects leave
/// the main flag off, but an instrument's unflagged input is a sidechain (Surge XT's is), not the
/// signal to process.
pub(crate) fn main_port(
    ports: &[(AudioPortFlags, u32)],
    is_input: bool,
    is_instrument: bool,
) -> Option<usize> {
    ports
        .iter()
        .position(|(flags, _)| flags.contains(AudioPortFlags::IS_MAIN))
        .or_else(|| (!ports.is_empty() && !(is_input && is_instrument)).then_some(0))
}

fn lossy(s: Option<&std::ffi::CStr>) -> Option<String> {
    s.map(|s| s.to_string_lossy().into_owned())
}

/// Channel counts of the main input and output ports, read from a short-lived instance.
fn main_ports(entry: &PluginEntry, id: &str, is_instrument: bool) -> Option<(u16, u16)> {
    let id = CString::new(id).ok()?;
    let mut instance = host::new_instance(entry, &id).ok()?;
    let ports: PluginAudioPorts = instance.plugin_shared_handle().get_extension()?;
    let handle = instance.plugin_handle();
    let main = |is_input| {
        let mut buf = AudioPortInfoBuffer::new();
        let infos: Vec<_> = (0..ports.count(&handle, is_input))
            .map(|i| {
                ports
                    .get(&handle, i, is_input, &mut buf)
                    .map_or((AudioPortFlags::empty(), 0), |p| (p.flags, p.channel_count))
            })
            .collect();
        main_port(&infos, is_input, is_instrument)
            .map_or(0, |i| u16::try_from(infos[i].1).unwrap_or(u16::MAX))
    };
    Some((main(true), main(false)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unflagged_ports_count_except_an_instruments_inputs() {
        let main = AudioPortFlags::IS_MAIN;
        let none = AudioPortFlags::empty();
        assert_eq!(main_port(&[(none, 2), (main, 2)], true, true), Some(1));
        assert_eq!(main_port(&[(none, 2)], true, true), None);
        assert_eq!(main_port(&[(none, 2)], true, false), Some(0));
        assert_eq!(main_port(&[(none, 2), (none, 1)], false, true), Some(0));
        assert_eq!(main_port(&[], false, false), None);
    }
}
