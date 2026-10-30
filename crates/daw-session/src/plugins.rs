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

use std::collections::HashMap;
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
    carried: HashMap<DeviceId, (DeviceSource, Vec<u8>)>,
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
    /// The state last handed out for the document to store; when the
    /// document comes back with it, the plugin needn't load it.
    pub saved: Option<StateBlob>,
    /// Created since the last build: its parameters have not been sent yet.
    pub fresh: bool,
    pub params: Vec<ParamInfo>,
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
        // State from a session this one replaced is newer than the document's.
        let carried = self
            .carried
            .remove(&d.id)
            .filter(|(source, _)| *source == d.source)
            .map(|(_, s)| s);
        let restore = carried.as_deref().or(d.state.as_ref().map(|s| &s.0[..]));
        if let Some(state) = restore
            && let Err(e) = instance.load_state(state)
        {
            warnings.push(Warning::DeviceStateRejected {
                device: d.name.clone(),
                detail: e.to_string(),
            });
        }
        let params = instance.params();
        Slot::Live(Box::new(Live {
            source: d.source.clone(),
            name: d.name.clone(),
            handle: PluginHandle::new(instance, sample_rate, max_frames),
            monitor,
            crashed: false,
            state: d.state.clone(),
            saved: None,
            fresh: true,
            params,
        }))
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

    /// Frees the plugins of devices that are gone, keeping their states in
    /// case an undo brings the device back.
    pub(crate) fn retain(&mut self, keep: impl Fn(DeviceId) -> bool) {
        let gone: Vec<DeviceId> = self.slots.keys().copied().filter(|&id| !keep(id)).collect();
        for id in gone {
            if let Some(Slot::Live(l)) = self.slots.remove(&id)
                && !l.crashed
                && let Ok(state) = l.handle.with(|p| p.save_state())
            {
                self.carried.insert(id, (l.source.clone(), state));
            }
        }
    }

    /// Frees every plugin, keeping the loader.
    pub(crate) fn clear(&mut self) {
        self.slots.clear();
        self.carried.clear();
    }

    /// Saves every running plugin's state and frees the plugins, keeping the states for the
    /// next session to restore.
    pub(crate) fn detach(&mut self) -> Plugins {
        let mut carried = std::mem::take(&mut self.carried);
        for (id, slot) in self.slots.drain() {
            if let Slot::Live(l) = slot
                && !l.crashed
                && let Ok(state) = l.handle.with(|p| p.save_state())
            {
                carried.insert(id, (l.source.clone(), state));
            }
        }
        Plugins {
            loader: self.loader.take(),
            slots: HashMap::new(),
            carried,
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
        if self.crashed {
            return None;
        }
        match self.handle.with(|p| p.save_state()) {
            Ok(s) if self.state.as_ref().is_some_and(|d| *d.0 == *s) => None,
            Ok(s) => Some(Ok(StateBlob(Arc::from(s)))),
            Err(e) => Some(Err(e.to_string())),
        }
    }
}
