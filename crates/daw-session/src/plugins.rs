//! Plugin devices.
//!
//! Each loaded plugin lives in a [`PluginHandle`] that the session keeps across graph builds,
//! not in a reused graph node: every build adds a fresh [`PluginHandle::node`] under the
//! device's key, so the plugin keeps its state while each graph picks up its current ports
//! and latency. [`Session::tick`](crate::Session::tick) pumps the handles about 60 times a
//! second and rebuilds when one asks for it.
//!
//! A [`PluginLoader`] instantiates plugins; [`SandboxLoader`] runs each one in its own host
//! process (see `daw-plugin-host`), so a plugin that crashes is reported and bypassed instead
//! of taking the application down. Without a loader, plugin devices are bypassed.
//!
//! Everything here runs on the thread that created the session, as the handles require.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use daw_core::plugin::{ParamInfo, PluginDescriptor, PluginError, PluginFormatKind};
use daw_engine::PluginHandle;
use daw_model::{Device, DeviceId, DeviceSource, StateBlob};
use daw_plugin_host::{SandboxConfig, SandboxFormat, SandboxMonitor, sandboxed_registry};
use daw_plugins::{PluginRegistry, ScanCache, Scanner};

use crate::Warning;

/// A plugin fresh from a [`PluginLoader`].
pub struct LoadedPlugin {
    pub instance: Box<dyn daw_core::plugin::PluginInstance>,
    /// Reports whether a sandboxed plugin's host crashed, and reloads it.
    pub monitor: Option<SandboxMonitor>,
}

/// Why a [`PluginLoader`] could not load a plugin.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    /// No plugin with this format and id is installed.
    #[error("not installed")]
    NotFound,
    #[error(transparent)]
    Failed(#[from] PluginError),
}

/// Instantiates the plugins that devices name. Called on the session's thread, while a graph
/// is built.
pub trait PluginLoader {
    fn load(
        &mut self,
        format: &PluginFormatKind,
        id: &str,
        sample_rate: f64,
        max_frames: u32,
    ) -> Result<LoadedPlugin, LoadError>;

    /// The plugins a scan elsewhere found (the app's plugin browser), so the loader can find
    /// them without scanning itself.
    fn set_known(&mut self, _plugins: &[PluginDescriptor]) {}
}

/// Loads every plugin in its own sandbox host process.
///
/// Plugins are found in [`registry`](Self::registry). When a project names one it does not
/// know, the loader scans every format's folders once, in scanner child processes, with the
/// results cached; a plugin installed after that needs a [`rescan`](Self::rescan).
pub struct SandboxLoader {
    config: SandboxConfig,
    registry: PluginRegistry,
    scanner: Scanner,
    cache: ScanCache,
    cache_file: Option<PathBuf>,
    scanned: bool,
}

impl SandboxLoader {
    /// A loader whose registry has every format of this build, sandboxed with `config`, at
    /// their default search paths.
    pub fn new(config: SandboxConfig) -> Self {
        Self {
            registry: sandboxed_registry(&config),
            scanner: config.launcher.scanner(),
            config,
            cache: ScanCache::new(),
            cache_file: None,
            scanned: false,
        }
    }

    /// Keeps the scan cache in `path`, so later runs skip unchanged bundles.
    pub fn with_cache_file(mut self, path: PathBuf) -> Self {
        self.cache = ScanCache::load(&path);
        self.cache_file = Some(path);
        self
    }

    /// The plugins found so far, and the search paths to scan.
    pub fn registry(&self) -> &PluginRegistry {
        &self.registry
    }

    pub fn registry_mut(&mut self) -> &mut PluginRegistry {
        &mut self.registry
    }

    /// Scans every format's folders again, loading only bundles that changed.
    pub fn rescan(&mut self) -> daw_plugins::ScanReport {
        self.scanned = true;
        // Another scan (the app's) may have written the file since it was read; start from
        // that, so saving doesn't drop its entries.
        if let Some(path) = &self.cache_file {
            self.cache = ScanCache::load(path);
        }
        let report = self.registry.scan(&self.scanner, &mut self.cache);
        if let Some(path) = &self.cache_file {
            // The cache only saves time; failing to write it is no reason to stop.
            let _ = self.cache.save(path);
        }
        report
    }

    fn find(&mut self, format: &PluginFormatKind, id: &str) -> Option<PluginDescriptor> {
        if self.registry.find(format, id).is_none() && !self.scanned {
            self.rescan();
        }
        self.registry.find(format, id).cloned()
    }
}

impl PluginLoader for SandboxLoader {
    fn load(
        &mut self,
        format: &PluginFormatKind,
        id: &str,
        sample_rate: f64,
        max_frames: u32,
    ) -> Result<LoadedPlugin, LoadError> {
        let d = self.find(format, id).ok_or(LoadError::NotFound)?;
        let sandbox = SandboxFormat::new(d.format.clone(), Vec::new(), self.config.clone());
        let instance = sandbox.instantiate_sandboxed(&d, sample_rate, max_frames)?;
        Ok(LoadedPlugin {
            monitor: Some(instance.monitor()),
            instance: Box::new(instance),
        })
    }

    fn set_known(&mut self, plugins: &[PluginDescriptor]) {
        self.registry.set_plugins(plugins.to_vec());
        self.scanned = true;
    }
}

/// How a plugin device is doing, for the UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginStatus {
    /// The plugin is loaded and plays.
    Running,
    /// Its sandbox host crashed or hung; the device is bypassed until
    /// [`Session::reload_plugin`](crate::Session::reload_plugin).
    Crashed,
    /// The plugin is not installed; the device is bypassed.
    Missing,
    /// The plugin would not load; the device is bypassed. The text is the plugin's or the
    /// format's own, in English.
    Failed(String),
    /// The session has no [`PluginLoader`]; the device is bypassed.
    NoLoader,
}

/// The plugins a session has loaded, and how to load more. Moves from one session to the next
/// when audio restarts (see [`Session::take_plugins`](crate::Session::take_plugins)).
#[derive(Default)]
pub struct Plugins {
    pub(crate) loader: Option<Box<dyn PluginLoader>>,
    pub(crate) slots: HashMap<DeviceId, Slot>,
    /// Plugin states saved from an earlier session, to restore instead of the document's when
    /// the plugin is loaded again.
    carried: HashMap<DeviceId, KeptState>,
    /// States of plugins whose device was removed, in case an undo brings it back.
    removed: RemovedStates,
    /// Plugins that would not hand over their state when they were [detached](Self::detach),
    /// for the next sync to report.
    pub(crate) unsaved: Vec<Warning>,
    /// Editors that were open in an earlier session, by their window title, to open again
    /// when the plugin is loaded again.
    reopen: HashMap<DeviceId, String>,
}

/// A freed plugin's state, with the plugin it is of: a device that comes back naming another
/// plugin must not be given it.
type KeptState = (DeviceSource, Vec<u8>);

/// The states of plugins whose device was removed, oldest first, to restore when an undo
/// brings the device back.
///
/// The session cannot tell which removals can still be undone, so the states are kept up to
/// a total size instead; past it the oldest go, and a device that comes back after that
/// starts from the document's state. The newest is always kept, whatever its size.
struct RemovedStates {
    states: VecDeque<(DeviceId, KeptState)>,
    bytes: usize,
    limit: usize,
}

impl Default for RemovedStates {
    fn default() -> Self {
        Self::with_limit(Self::LIMIT)
    }
}

impl RemovedStates {
    /// Total size of the states kept, in bytes.
    const LIMIT: usize = 64 << 20;

    fn with_limit(limit: usize) -> Self {
        Self {
            states: VecDeque::new(),
            bytes: 0,
            limit,
        }
    }

    fn insert(&mut self, device: DeviceId, state: KeptState) {
        self.take(device);
        self.bytes += state.1.len();
        self.states.push_back((device, state));
        while self.bytes > self.limit && self.states.len() > 1 {
            let (_, (_, oldest)) = self.states.pop_front().expect("more than one");
            self.bytes -= oldest.len();
        }
    }

    fn take(&mut self, device: DeviceId) -> Option<KeptState> {
        let i = self.states.iter().position(|(id, _)| *id == device)?;
        let (_, state) = self.states.remove(i)?;
        self.bytes -= state.1.len();
        Some(state)
    }

    fn clear(&mut self) {
        self.states.clear();
        self.bytes = 0;
    }
}

pub(crate) enum Slot {
    Live(Box<Live>),
    /// Tried and failed; not tried again until the device's source changes or the plugins are
    /// cleared.
    Failed {
        source: DeviceSource,
        status: PluginStatus,
    },
}

pub(crate) struct Live {
    pub source: DeviceSource,
    pub name: String,
    pub handle: PluginHandle,
    pub monitor: Option<SandboxMonitor>,
    /// The sandbox host is down; the graph has a bypass in the plugin's place.
    pub crashed: bool,
    /// The document state last loaded into the plugin, or saved from it.
    pub state: Option<StateBlob>,
    /// The plugin refused the last state it was given, so what it holds is not the user's
    /// work: its defaults, most likely. That must not replace the document's state, which
    /// another version of the plugin may still read. Over once the plugin takes a state, or
    /// reports a change made in its editor.
    pub state_rejected: bool,
    /// The state last handed out for the document to store; when the
    /// document comes back with it, the plugin needn't load it.
    pub saved: Option<StateBlob>,
    /// Created since the last build: its parameters have not been sent yet.
    pub fresh: bool,
    pub params: Vec<ParamInfo>,
    /// Whether the plugin's editor was open at the last tick.
    pub editor_open: bool,
    /// The editor's window title, once it has been opened.
    pub editor_title: Option<String>,
}

impl Plugins {
    pub(crate) fn set_loader(&mut self, loader: Option<Box<dyn PluginLoader>>) {
        self.loader = loader;
        // Devices that failed for want of a loader may load now.
        self.slots.retain(|_, s| matches!(s, Slot::Live(_)));
    }

    /// The loaded plugin of `d`, loading it first if needed. `None` means bypass it; a
    /// warning says why, the first time.
    pub(crate) fn ensure(
        &mut self,
        d: &Device,
        sample_rate: f64,
        max_frames: u32,
        warnings: &mut Vec<Warning>,
    ) -> Option<&mut Live> {
        let DeviceSource::Plugin { format, id } = &d.source else {
            return None;
        };
        let stale = match self.slots.get(&d.id) {
            Some(Slot::Live(l)) => l.source != d.source,
            Some(Slot::Failed { source, .. }) => *source != d.source,
            None => true,
        };
        if stale {
            let slot = self.load(d, format, id, sample_rate, max_frames, warnings);
            self.slots.insert(d.id, slot);
        }
        match self.slots.get_mut(&d.id) {
            Some(Slot::Live(l)) if !l.crashed => {
                l.name.clone_from(&d.name);
                Some(l)
            }
            _ => None,
        }
    }

    fn load(
        &mut self,
        d: &Device,
        format: &PluginFormatKind,
        id: &str,
        sample_rate: f64,
        max_frames: u32,
        warnings: &mut Vec<Warning>,
    ) -> Slot {
        let plugin = || id.to_owned();
        let failed = |status| Slot::Failed {
            source: d.source.clone(),
            status,
        };
        let Some(loader) = &mut self.loader else {
            warnings.push(Warning::PluginNotLoaded {
                device: d.name.clone(),
                plugin: plugin(),
            });
            return failed(PluginStatus::NoLoader);
        };
        let loaded = match loader.load(format, id, sample_rate, max_frames) {
            Ok(l) => l,
            Err(LoadError::NotFound) => {
                warnings.push(Warning::PluginMissing {
                    device: d.name.clone(),
                    plugin: plugin(),
                });
                return failed(PluginStatus::Missing);
            }
            Err(LoadError::Failed(e)) => {
                warnings.push(Warning::PluginLoadFailed {
                    device: d.name.clone(),
                    plugin: plugin(),
                    detail: e.to_string(),
                });
                return failed(PluginStatus::Failed(e.to_string()));
            }
        };
        let LoadedPlugin {
            mut instance,
            monitor,
        } = loaded;
        // State from a session this one replaced, or from before the device was removed, is
        // newer than the document's.
        let kept = self.carried.remove(&d.id).or(self.removed.take(d.id));
        let kept = kept.filter(|(source, _)| *source == d.source).map(|k| k.1);
        let restore = kept.as_deref().or(d.state.as_ref().map(|s| &s.0[..]));
        let mut state_rejected = false;
        if let Some(state) = restore
            && let Err(e) = instance.load_state(state)
        {
            state_rejected = true;
            warnings.push(Warning::DeviceStateRejected {
                device: d.name.clone(),
                detail: e.to_string(),
            });
        }
        let params = instance.params();
        // An editor that was open before audio restarted.
        let mut editor_open = false;
        let mut editor_title = None;
        if let Some(title) = self.reopen.remove(&d.id) {
            instance.set_editor_title(&title);
            match instance.open_editor() {
                Ok(()) => editor_open = true,
                Err(e) => warnings.push(Warning::editor_failed(d.name.clone(), &e)),
            }
            editor_title = Some(title);
        }
        Slot::Live(Box::new(Live {
            source: d.source.clone(),
            name: d.name.clone(),
            handle: PluginHandle::new(instance, sample_rate, max_frames),
            monitor,
            crashed: false,
            state: d.state.clone(),
            state_rejected,
            saved: None,
            fresh: true,
            params,
            editor_open,
            editor_title,
        }))
    }

    /// Hands the loader the plugins a scan found. Devices whose plugin was missing are tried
    /// again on the next build, in case the scan found it.
    pub(crate) fn set_known(&mut self, plugins: &[PluginDescriptor]) {
        if let Some(loader) = &mut self.loader {
            loader.set_known(plugins);
        }
        self.slots.retain(|_, s| {
            !matches!(
                s,
                Slot::Failed {
                    status: PluginStatus::Missing,
                    ..
                }
            )
        });
    }

    pub(crate) fn live(&self, device: DeviceId) -> Option<&Live> {
        match self.slots.get(&device)? {
            Slot::Live(l) => Some(l),
            Slot::Failed { .. } => None,
        }
    }

    pub(crate) fn live_mut(&mut self, device: DeviceId) -> Option<&mut Live> {
        match self.slots.get_mut(&device)? {
            Slot::Live(l) => Some(l),
            Slot::Failed { .. } => None,
        }
    }

    pub(crate) fn status(&self, device: DeviceId) -> Option<PluginStatus> {
        Some(match self.slots.get(&device)? {
            Slot::Live(l) if l.crashed => PluginStatus::Crashed,
            Slot::Live(_) => PluginStatus::Running,
            Slot::Failed { status, .. } => status.clone(),
        })
    }

    /// Frees the plugins of devices that are gone, keeping their states in case an undo
    /// brings the device back. `warnings` gets the plugins that would not hand theirs over.
    pub(crate) fn retain(&mut self, keep: impl Fn(DeviceId) -> bool, warnings: &mut Vec<Warning>) {
        let gone: Vec<DeviceId> = self.slots.keys().copied().filter(|&id| !keep(id)).collect();
        for id in gone {
            if let Some(Slot::Live(l)) = self.slots.remove(&id) {
                // The device is gone, so is its window.
                if !l.crashed {
                    l.handle.with(|p| p.close_editor());
                }
                if let Some(state) = l.state_to_keep(warnings) {
                    self.removed.insert(id, state);
                }
            }
        }
    }

    /// Frees every plugin, keeping the loader.
    pub(crate) fn clear(&mut self) {
        self.slots.clear();
        self.carried.clear();
        self.removed.clear();
        self.reopen.clear();
        self.unsaved.clear();
    }

    /// Saves every plugin's state and frees the plugins, keeping the states for the next
    /// session to restore.
    pub(crate) fn detach(&mut self) -> Plugins {
        let mut carried = std::mem::take(&mut self.carried);
        let mut reopen = std::mem::take(&mut self.reopen);
        let mut unsaved = std::mem::take(&mut self.unsaved);
        for (id, slot) in self.slots.drain() {
            let Slot::Live(l) = slot else {
                continue;
            };
            if let Some(state) = l.state_to_keep(&mut unsaved) {
                carried.insert(id, state);
            }
            if !l.crashed
                && l.handle.with(|p| p.is_editor_open())
                && let Some(title) = l.editor_title.clone()
            {
                reopen.insert(id, title);
            }
        }
        Plugins {
            loader: self.loader.take(),
            slots: HashMap::new(),
            carried,
            removed: std::mem::take(&mut self.removed),
            reopen,
            unsaved,
        }
    }
}

/// A plugin's state that differs from the document's, from
/// [`Session::plugin_states`](crate::Session::plugin_states).
#[derive(Clone, Debug)]
pub struct PluginState {
    pub device: DeviceId,
    /// The new state, or why the plugin would not save it (in English).
    pub state: Result<StateBlob, String>,
}

impl Live {
    /// The plugin's state, if it differs from the document's.
    pub(crate) fn changed_state(&self) -> Option<Result<StateBlob, String>> {
        // A crashed plugin has nothing newer than the document, and what a plugin holds in
        // place of a state it refused is not for the document.
        if self.crashed || self.state_rejected {
            return None;
        }
        match self.handle.with(|p| p.save_state()) {
            Ok(s) if self.state.as_ref().is_some_and(|d| *d.0 == *s) => None,
            Ok(s) => Some(Ok(StateBlob(Arc::from(s)))),
            Err(e) => Some(Err(e.to_string())),
        }
    }

    /// The state to restore this plugin from once it is freed, if it has one worth more than
    /// the document's. A running plugin that would not save its state goes to `warnings`:
    /// what the user changed in it since the document's state is lost.
    fn state_to_keep(&self, warnings: &mut Vec<Warning>) -> Option<KeptState> {
        // What it holds is not the state it was given: the document's is tried again.
        if self.state_rejected {
            return None;
        }
        // A crashed sandbox answers with the state it last saved or loaded, if any.
        match self.handle.with(|p| p.save_state()) {
            Ok(state) => Some((self.source.clone(), state)),
            Err(_) if self.crashed => None,
            Err(e) => {
                warnings.push(Warning::PluginStateNotSaved {
                    device: self.name.clone(),
                    detail: e.to_string(),
                });
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(len: usize) -> KeptState {
        let source = DeviceSource::Builtin { kind: "x".into() };
        (source, vec![0; len])
    }

    #[test]
    fn removed_states_drop_the_oldest_past_the_limit() {
        let ids: Vec<DeviceId> = (0..4).map(|_| DeviceId::new()).collect();
        let mut removed = RemovedStates::with_limit(10);
        removed.insert(ids[0], kept(4));
        removed.insert(ids[1], kept(4));
        removed.insert(ids[2], kept(4));
        assert!(removed.take(ids[0]).is_none(), "the oldest went");
        assert_eq!(removed.bytes, 8);
        // Removed again: one state, the newer one.
        removed.insert(ids[1], kept(6));
        assert_eq!((removed.states.len(), removed.bytes), (2, 10));
        // One state over the limit pushes the rest out and stays itself.
        removed.insert(ids[3], kept(11));
        assert_eq!(removed.take(ids[3]).map(|s| s.1.len()), Some(11));
        assert_eq!((removed.states.len(), removed.bytes), (0, 0));
    }
}
