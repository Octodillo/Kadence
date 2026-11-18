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
use std::sync::Arc;
use std::time::{Duration, Instant};

use daw_core::plugin::{ParamInfo, PluginDescriptor, PluginError, PluginFormatKind};
use daw_engine::PluginHandle;
use daw_model::{Device, DeviceId, DeviceSource, StateBlob};
use daw_plugin_host::{SandboxConfig, SandboxFormat, SandboxMonitor};

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
    /// The loader has not been told which plugins are installed yet (see
    /// [`PluginLoader::set_known`]), so it cannot say whether this one is.
    #[error("the installed plugins are not known yet")]
    NotKnownYet,
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

    /// The installed plugins, as a scan found them (the app's plugin catalog). Replaces what
    /// the loader knew. A loader that depends on being told answers
    /// [`LoadError::NotKnownYet`] until then.
    fn set_known(&mut self, _plugins: &[PluginDescriptor]) {}
}

/// Loads every plugin in its own sandbox host process.
///
/// The loader does not look for plugins itself: it loads the ones it has been told about, as
/// found by a [`PluginCatalog`](crate::catalog::PluginCatalog) scan, and reports any other as
/// not installed. So playback and an export find exactly the plugins the plugin browser lists,
/// in the user's folders too. Until it is told, it reports [`LoadError::NotKnownYet`]: the
/// first scan may still run when a project is opened.
pub struct SandboxLoader {
    config: SandboxConfig,
    /// `None` until a scan's result arrives.
    known: Option<Vec<PluginDescriptor>>,
}

impl SandboxLoader {
    /// A loader that sandboxes with `config` and has not been told the installed plugins.
    pub fn new(config: SandboxConfig) -> Self {
        Self {
            config,
            known: None,
        }
    }

    /// Starts out knowing `plugins`, for a loader made once the scan is over (an export's).
    pub fn with_known(mut self, plugins: Vec<PluginDescriptor>) -> Self {
        self.known = Some(plugins);
        self
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
        let known = self.known.as_ref().ok_or(LoadError::NotKnownYet)?;
        let d = known
            .iter()
            .find(|d| d.is(format, id))
            .ok_or(LoadError::NotFound)?;
        let sandbox = SandboxFormat::new(d.format.clone(), Vec::new(), self.config.clone());
        let instance = sandbox.instantiate_sandboxed(d, sample_rate, max_frames)?;
        Ok(LoadedPlugin {
            monitor: Some(instance.monitor()),
            instance: Box::new(instance),
        })
    }

    fn set_known(&mut self, plugins: &[PluginDescriptor]) {
        self.known = Some(plugins.to_vec());
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
    /// The scan for installed plugins has not finished, so whether this one is installed is
    /// not known; the device is bypassed until
    /// [`Session::set_known_plugins`](crate::Session::set_known_plugins) loads it or finds it
    /// [`Missing`](Self::Missing).
    Waiting,
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
    /// What is said only once, when it happens, for the next sync that succeeds to report
    /// ahead of its own warnings: plugins that would not load
    /// ([`ensure`](Self::ensure)), or would not hand over their state when they were freed
    /// ([`detach`](Self::detach), [`retain`](Self::retain)). A sync the engine refuses has
    /// its error shown instead, so these wait.
    pub(crate) owed: Vec<Warning>,
    /// Editors that were open in an earlier session, by their window title, to open again
    /// when the plugin is loaded again.
    reopen: HashMap<DeviceId, String>,
    /// [`Warning::PluginsNotRealtime`] was given: it is about the system, not one plugin, so
    /// it is said once.
    pub(crate) realtime_warned: bool,
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

/// Whether a plugin holds the state it was last given.
///
/// Only a plugin that does has work of the user's to save. What a plugin holds in place of a
/// state it refused (its defaults, most likely) must not replace the document's state, which
/// another version of the plugin may still read.
pub(crate) enum Given {
    /// It took the state, or there was none to give. Its own state may go to the document.
    Taken,
    /// Not known: the plugin was busy, or its sandbox host was down or went down, so the
    /// state is only remembered for the reload. [`Session::tick`](crate::Session::tick)
    /// offers it again once the plugin runs.
    Pending(StateBlob),
    /// It refused the state. Over once it takes another, or reports a change made in its
    /// editor.
    Refused,
}

/// How long a [pending](Given::Pending) state waits before it is offered again.
const CONFIRM_EVERY: Duration = Duration::from_secs(1);

pub(crate) enum Slot {
    Live(Box<Live>),
    /// Tried and failed; not tried again until the device's source changes, the plugins are
    /// cleared or, unless there is no loader, a scan's result arrives.
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
    /// The document's state for the device as of the last sync: what the plugin was last
    /// given, or what was saved from it. Whether it has it is in [`given`](Self::given).
    pub state: Option<StateBlob>,
    pub given: Given,
    /// When a [pending](Given::Pending) state is next offered.
    pub confirm_at: Instant,
    /// Drop the plugin's next report of a state change: it refused a state since, and the
    /// report may be from before or about that.
    pub ignore_report: bool,
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
    /// warning ([`owed`](Self::owed)) says why, the first time.
    pub(crate) fn ensure(
        &mut self,
        d: &Device,
        sample_rate: f64,
        max_frames: u32,
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
            let mut said = Vec::new();
            let slot = self.load(d, format, id, sample_rate, max_frames, &mut said);
            self.owed.append(&mut said);
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
            // Not worth a warning: the scan is on its way, and its result tries again.
            Err(LoadError::NotKnownYet) => return failed(PluginStatus::Waiting),
            Err(LoadError::Failed(e)) => {
                warnings.push(Warning::PluginLoadFailed {
                    device: d.name.clone(),
                    plugin: plugin(),
                    detail: e.to_string(),
                });
                return failed(PluginStatus::Failed(e.to_string()));
            }
        };
        let LoadedPlugin { instance, monitor } = loaded;
        let mut live = Live {
            source: d.source.clone(),
            name: d.name.clone(),
            handle: PluginHandle::new(instance, sample_rate, max_frames),
            monitor,
            crashed: false,
            state: d.state.clone(),
            given: Given::Taken,
            confirm_at: Instant::now(),
            ignore_report: false,
            saved: None,
            fresh: true,
            params: Vec::new(),
            editor_open: false,
            editor_title: None,
        };
        // State from a session this one replaced, or from before the device was removed, is
        // newer than the document's.
        let kept = self.carried.remove(&d.id).or(self.removed.take(d.id));
        let kept = kept.filter(|(source, _)| *source == d.source);
        let mut restored = false;
        if let Some((_, state)) = kept {
            match live.give(StateBlob(state.into())) {
                Ok(()) => restored = true,
                // What the user changed since the document's state is lost; that state is
                // still to try.
                Err(e) => warnings.push(Warning::PluginStateNotSaved {
                    device: live.label(),
                    detail: e.to_string(),
                }),
            }
        }
        if !restored
            && let Some(state) = &d.state
            && let Err(e) = live.give(state.clone())
        {
            warnings.push(Warning::DeviceStateRejected {
                device: live.label(),
                detail: e.to_string(),
            });
        }
        live.params = live.handle.with(|p| p.params());
        // An editor that was open before audio restarted.
        if let Some(title) = self.reopen.remove(&d.id) {
            let opened = live.handle.with(|p| {
                p.set_editor_title(&title);
                p.open_editor()
            });
            match opened {
                Ok(()) => live.editor_open = true,
                Err(e) => warnings.push(Warning::editor_failed(d.name.clone(), &e)),
            }
            live.editor_title = Some(title);
        }
        Slot::Live(Box::new(live))
    }

    /// Hands the loader the plugins a scan found. Devices whose plugin did not load are tried
    /// again on the next build: the scan may have found a plugin that was missing or waited
    /// for, or a new place for one whose bundle had moved.
    pub(crate) fn set_known(&mut self, plugins: &[PluginDescriptor]) {
        let Some(loader) = &mut self.loader else {
            return;
        };
        loader.set_known(plugins);
        self.slots.retain(|_, s| matches!(s, Slot::Live(_)));
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
    /// brings the device back.
    pub(crate) fn retain(&mut self, keep: impl Fn(DeviceId) -> bool) {
        let gone: Vec<DeviceId> = self.slots.keys().copied().filter(|&id| !keep(id)).collect();
        for id in gone {
            if let Some(Slot::Live(l)) = self.slots.remove(&id) {
                // The device is gone, so is its window.
                if !l.crashed {
                    l.handle.with(|p| p.close_editor());
                }
                if let Some(state) = l.state_to_keep(&mut self.owed) {
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
        self.owed.clear();
    }

    /// Frees the plugins, keeping the state of each that has one of its own (see
    /// [`Live::state_to_keep`]) for the next session to restore.
    pub(crate) fn detach(&mut self) -> Plugins {
        let mut carried = std::mem::take(&mut self.carried);
        let mut reopen = std::mem::take(&mut self.reopen);
        let mut owed = std::mem::take(&mut self.owed);
        for (id, slot) in self.slots.drain() {
            let Slot::Live(l) = slot else {
                continue;
            };
            if let Some(state) = l.state_to_keep(&mut owed) {
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
            owed,
            realtime_warned: self.realtime_warned,
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
    /// What messages call the device: its name, or for an unnamed one its plugin.
    pub(crate) fn label(&self) -> String {
        if self.name.is_empty() {
            crate::plugin_id(&self.source)
        } else {
            self.name.clone()
        }
    }

    fn host_down(&self) -> bool {
        self.crashed || self.monitor.as_ref().is_some_and(|m| m.is_crashed())
    }

    /// Loads `state` into the plugin and records in [`given`](Self::given) whether it has
    /// it. The error is the plugin's refusal; a plugin that could not be asked (busy, or its
    /// host down) is offered the state again by [`Session::tick`](crate::Session::tick).
    pub(crate) fn give(&mut self, state: StateBlob) -> Result<(), PluginError> {
        let loaded = self.handle.with(|p| p.load_state(&state.0));
        // A sandbox whose host is down only remembers the state for its reload, and so does
        // one whose host died of the load.
        let unasked = self.host_down() || matches!(loaded, Err(PluginError::Busy(_)));
        if unasked {
            self.given = Given::Pending(state);
            self.confirm_at = Instant::now() + CONFIRM_EVERY;
            return Ok(());
        }
        match loaded {
            Ok(()) => {
                self.given = Given::Taken;
                Ok(())
            }
            Err(e) => {
                self.given = Given::Refused;
                // A report from before the load, or of what the plugin did in refusing it,
                // is not the user's doing.
                self.handle.with(|p| p.take_state_changed());
                self.ignore_report = true;
                Err(e)
            }
        }
    }

    /// The pending state, when it is time to offer it again.
    pub(crate) fn due(&self) -> Option<StateBlob> {
        match &self.given {
            Given::Pending(state) if !self.host_down() && Instant::now() >= self.confirm_at => {
                Some(state.clone())
            }
            _ => None,
        }
    }

    /// The host went down: what it remembers (the state last saved or loaded) is what the
    /// reload restores, if the plugin still takes it. Offered again after the reload, to
    /// know.
    pub(crate) fn host_lost(&mut self) {
        if matches!(self.given, Given::Taken)
            && let Ok(state) = self.handle.with(|p| p.save_state())
        {
            self.given = Given::Pending(StateBlob(state.into()));
        }
    }

    /// The plugin's state, if it differs from the document's. Nothing while the plugin does
    /// not hold the state it was given (see [`Given`]): what it has then is not the user's.
    pub(crate) fn changed_state(&self) -> Option<Result<StateBlob, String>> {
        if self.crashed || !matches!(self.given, Given::Taken) {
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
        let state = match &self.given {
            // What it holds is not the state it was given: the document's is tried again.
            Given::Refused => return None,
            Given::Pending(state) => state.0.to_vec(),
            // A sandbox whose crash went unnoticed so far answers with the state it last
            // saved or loaded, if any.
            Given::Taken => match self.handle.with(|p| p.save_state()) {
                Ok(state) => state,
                Err(_) if self.host_down() => return None,
                Err(e) => {
                    warnings.push(Warning::PluginStateNotSaved {
                        device: self.label(),
                        detail: e.to_string(),
                    });
                    return None;
                }
            },
        };
        Some((self.source.clone(), state))
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
