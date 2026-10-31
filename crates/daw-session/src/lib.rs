//! Plays a [`daw_model`] project on the [`daw_engine`].
//!
//! A [`Session`] owns the engine's controller half. Each time the document
//! changes, the app hands it the new [`ProjectSnapshot`] through
//! [`Session::sync`], and the session works out the cheapest way to make the
//! engine match:
//!
//! * Routing, device chains or the track list changed: build a new
//!   [`GraphDesc`] and swap it in.
//! * Only fader, pan, mute, solo, send level or device parameters changed:
//!   send parameter changes to the running graph.
//! * Only clips (or the tempo) changed: send each affected track's player a
//!   new [`player::ClipSet`]. No graph change, so playback doesn't stutter.
//!
//! The session diffs snapshots itself instead of trusting a change list, so
//! undo, redo and loading a file all take the same path.
//!
//! Plugin devices are loaded through a [`PluginLoader`] and kept across
//! graph builds; see [`plugins`]. Call [`Session::tick`] about 60 times a
//! second on the session's thread.

mod build;
pub mod capture;
pub mod export;
pub mod host;
mod media;
pub mod player;
pub mod plugins;
pub mod record;
mod tempo;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use daw_core::meter::PeakMeter;
use daw_core::time::{SampleTime, TempoMap, TickTime};
use daw_engine::{
    AutomationLane, Curve, EngineConfig, EngineController, EngineError, EngineProcessor, LoopRange,
    NodeKey, NodeMeter,
};
use daw_model::{ClipId, DeviceId, LaneId, ProjectSnapshot, SendId, TrackId};

pub use build::effective_mutes;
pub use media::{MEMORY_LIMIT_SECONDS, MediaPool};
use player::{LoopPoints, PlayerHandle};
pub use plugins::{
    LoadError, LoadedPlugin, PluginLoader, PluginState, PluginStatus, Plugins, SandboxLoader,
};
pub use tempo::tempo_map;

#[derive(Clone, Debug)]
pub struct SessionConfig {
    pub engine: EngineConfig,
    /// Hardware input channels the graph's input node exposes.
    pub inputs: u16,
    /// Hardware output channels (the master strip's width).
    pub outputs: u16,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            engine: EngineConfig::default(),
            inputs: 2,
            outputs: 2,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Engine(#[from] EngineError),
}

/// Something a sync could not do fully; playback goes on without it. The
/// variants carry data so a front end can word them in the user's
/// language; [`Display`](std::fmt::Display) is English, for logs.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Warning {
    /// A built-in device refused its saved state; it runs with defaults.
    #[error("{device}: {detail}")]
    DeviceStateRejected { device: String, detail: String },
    /// The project names a built-in device this build does not have.
    #[error("unknown built-in device {kind}")]
    UnknownDevice { kind: String },
    /// A plugin device is bypassed: the session has no plugin loader.
    #[error("plugin {plugin} is not loaded yet; it is bypassed")]
    PluginNotLoaded { device: String, plugin: String },
    /// A plugin device is bypassed: its plugin is not installed.
    #[error("plugin {plugin} is not installed; {device} is bypassed")]
    PluginMissing { device: String, plugin: String },
    /// A plugin device is bypassed: its plugin would not load. `detail` is
    /// the plugin's or the format's own message.
    #[error("plugin {plugin} would not load ({detail}); {device} is bypassed")]
    PluginLoadFailed {
        device: String,
        plugin: String,
        detail: String,
    },
    /// A sandboxed plugin crashed or hung; its device is bypassed until it
    /// is reloaded.
    #[error("plugin {plugin} of {device} crashed; it is bypassed")]
    PluginCrashed { device: String, plugin: String },
    /// A plugin would not hand over its state, so the project keeps the
    /// one it had. `detail` is the plugin's or the format's own message.
    #[error("{device} did not save its plugin state ({detail})")]
    PluginStateNotSaved { device: String, detail: String },
    /// An audio file could not be streamed; its clips are silent.
    #[error("can't stream {}: {detail}", path.display())]
    CannotStream { path: PathBuf, detail: String },
    /// An audio file could not be read; its clips are silent.
    #[error("can't read {}", path.display())]
    CannotRead { path: PathBuf },
    /// A track's notes did not reach the engine; the next sync retries.
    #[error("note update for track {track} was dropped")]
    NotesDropped { track: String },
    /// A track's clips did not reach the engine; the next sync retries.
    #[error("clip update for track {track} was dropped")]
    ClipsDropped { track: String },
}

/// What the running graph was built from, and what was last sent to it.
#[derive(Default)]
pub(crate) struct Built {
    pub structure: build::Structure,
    pub players: HashMap<TrackId, PlayerHandle>,
    /// Last value sent per (node, param), so unchanged values aren't resent.
    pub params: HashMap<(NodeKey, u32), f64>,
    /// The clips each player was last given, to spot clip edits.
    pub clips: HashMap<TrackId, Vec<Arc<daw_model::Clip>>>,
    /// Gain reduction meters of devices that have one.
    pub device_meters: HashMap<DeviceId, Arc<PeakMeter>>,
    /// MIDI players of instrument tracks.
    pub midi: HashMap<TrackId, daw_midi::player::PlayerHandle>,
    /// The clips each MIDI player's sequence was last compiled from.
    pub midi_clips: HashMap<TrackId, Vec<Arc<daw_model::Clip>>>,
    /// Input taps of audio tracks.
    pub taps: HashMap<TrackId, capture::TapHandle>,
}

/// The engine as the model sees it. See the crate docs.
pub struct Session {
    controller: EngineController,
    config: SessionConfig,
    media: MediaPool,
    tempo: daw_core::time::SegmentTempoMap,
    tempo_data: Option<daw_model::TempoData>,
    loop_points: Arc<LoopPoints>,
    loop_ticks: Option<(TickTime, TickTime)>,
    built: Option<Built>,
    warnings: Vec<Warning>,
    /// The project's master track, as of the last sync.
    master: Option<TrackId>,
    /// What the transport was last told, so the UI doesn't wait a period
    /// for the audio thread to confirm.
    playing: bool,
    /// Automation lanes left out of the graph while the user overwrites
    /// them (touch write).
    suspended: HashSet<LaneId>,
    /// Parameters held at a value the document doesn't have yet.
    live: HashMap<(NodeKey, u32), f64>,
    /// Rendering to a file rather than playing: players wait for the disk.
    pub(crate) offline: Option<Arc<player::Offline>>,
    plugins: Plugins,
    /// The snapshot last synced, for rebuilds the plugins ask for.
    last: Option<ProjectSnapshot>,
    /// Rebuild the graph on the next sync even if the project's shape is
    /// unchanged.
    force_rebuild: bool,
    /// After the engine refused a graph: how long [`tick`](Self::tick)
    /// waits before it tries again, and when.
    retry: Option<(std::time::Duration, std::time::Instant)>,
    /// Audio tracks that monitor automatically (armed, monitoring on
    /// auto), as of the last sync.
    auto_monitor: HashSet<TrackId>,
}

/// How long [`Session::tick`] waits before resending a refused graph, at
/// first and at most.
const RETRY_MIN: std::time::Duration = std::time::Duration::from_millis(15);
const RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(1);

/// What a [`Session::tick`] has for the user.
#[derive(Debug, Default)]
pub struct TickReport {
    /// Plugins that crashed, and problems a rebuild found (a plugin that
    /// would not load).
    pub warnings: Vec<Warning>,
    /// The rebuild failed; it is retried on later ticks.
    pub error: Option<SessionError>,
}

impl Session {
    /// Creates a session and the engine processor to move into the audio
    /// callback.
    pub fn new(config: SessionConfig) -> (Self, EngineProcessor) {
        let (controller, processor) = EngineController::new(config.engine.clone());
        let rate = config.engine.sample_rate.round() as u32;
        let session = Self {
            controller,
            media: MediaPool::new(rate),
            config,
            tempo: Default::default(),
            tempo_data: None,
            loop_points: Arc::default(),
            loop_ticks: None,
            built: None,
            warnings: Vec::new(),
            playing: false,
            master: None,
            suspended: HashSet::new(),
            live: HashMap::new(),
            offline: None,
            plugins: Plugins::default(),
            last: None,
            force_rebuild: false,
            retry: None,
            auto_monitor: HashSet::new(),
        };
        (session, processor)
    }

    pub fn config(&self) -> &SessionConfig {
        &self.config
    }

    pub fn sample_rate(&self) -> f64 {
        self.config.engine.sample_rate
    }

    pub fn controller(&self) -> &EngineController {
        &self.controller
    }

    /// Whether the last sync couldn't send everything (a player's queue was
    /// full) and should run again even if the document hasn't changed.
    pub fn wants_sync(&self) -> bool {
        self.built.as_ref().is_some_and(|b| {
            b.midi.keys().any(|t| !b.midi_clips.contains_key(t))
                || b.players.keys().any(|t| !b.clips.contains_key(t))
        })
    }

    /// The MIDI player of an instrument track, for record arming and
    /// collecting what was played.
    pub fn midi_player(&mut self, track: TrackId) -> Option<&mut daw_midi::player::PlayerHandle> {
        self.built.as_mut()?.midi.get_mut(&track)
    }

    pub fn media_mut(&mut self) -> &mut MediaPool {
        &mut self.media
    }

    /// The directory relative clip paths are resolved against (the project
    /// bundle). Takes effect on the next [`sync`](Self::sync).
    pub fn set_root(&mut self, root: Option<PathBuf>) {
        if self.media.root() == root.as_deref() {
            return;
        }
        self.media.set_root(root);
        // Paths may resolve differently now; rebuild everything but the
        // input taps, whose nodes hold takes being recorded (saving a new
        // project while recording moves its root).
        let taps = self.built.take().map(|b| b.taps).unwrap_or_default();
        self.built = Some(Built {
            taps,
            ..Default::default()
        });
    }

    /// Problems found by the last sync, such as audio files that could not
    /// be read. For the UI to show.
    pub fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    pub fn tempo(&self) -> &daw_core::time::SegmentTempoMap {
        &self.tempo
    }

    /// Makes the engine play `snapshot`. See the crate docs for what this
    /// sends.
    pub fn sync(&mut self, snapshot: &ProjectSnapshot) -> Result<(), SessionError> {
        self.collect_garbage();
        self.warnings.clear();
        self.master = Some(snapshot.master.id);
        let tempo_changed = self.tempo_data.as_ref() != Some(&snapshot.tempo);
        if tempo_changed {
            // Tempo and time signatures, so the metronome and plugins see
            // the project's bars and beats.
            let timeline = daw_midi::model::timeline(&snapshot.tempo);
            self.tempo = timeline.tempo.clone();
            // Marked as sent only once it is, so a full queue retries next sync.
            self.controller.set_timeline(timeline)?;
            self.tempo_data = Some(snapshot.tempo.clone());
            self.apply_loop()?;
        }
        self.last = Some(snapshot.clone());
        let structure = build::Structure::of(snapshot, &self.suspended);
        // Automation is in samples, so a tempo change re-times it.
        let rebuild = std::mem::take(&mut self.force_rebuild)
            || self.built.as_ref().is_none_or(|b| {
                b.structure != structure || (tempo_changed && structure.has_automation())
            });
        if rebuild {
            let old = self.built.take().unwrap_or_default();
            let (desc, built) = build::graph(self, snapshot, structure, old);
            // Kept only once the engine has the graph: a refused graph's players never
            // ran, so the next build must not take their handles for the running ones.
            if let Err(e) = self.controller.set_graph(desc) {
                let backoff = self
                    .retry
                    .map_or(RETRY_MIN, |(b, _)| (b * 2).min(RETRY_MAX));
                self.retry = Some((backoff, std::time::Instant::now() + backoff));
                return Err(e.into());
            }
            self.retry = None;
            self.built = Some(built);
            let ids: HashSet<DeviceId> = snapshot
                .all_tracks()
                .flat_map(|t| t.devices.iter().map(|d| d.id))
                .collect();
            // After the new graph is queued, so the nodes of removed plugins aren't
            // silenced while it is being built. Until the audio thread swaps it in (a
            // block at most), the old graph plays them as silence.
            self.plugins.retain(|id| ids.contains(&id));
        }
        self.sync_plugin_states(snapshot);
        self.send_params(snapshot)?;
        self.send_clips(snapshot, tempo_changed);
        self.apply_inputs(snapshot);
        Ok(())
    }

    /// Runs the plugins' main-thread work and frees what the audio thread
    /// is done with. Call it about 60 times a second, on the session's
    /// thread.
    ///
    /// Rebuilds the graph when a plugin asks for a new layout or latency,
    /// when a sandboxed plugin crashed (it is bypassed) or came back, and
    /// when the last graph could not be sent (retried less and less often
    /// while the engine keeps refusing). Returns what the user should hear
    /// about.
    pub fn tick(&mut self) -> TickReport {
        self.collect_garbage();
        let mut news = Vec::new();
        let mut rebuild = self.built.is_none()
            && self
                .retry
                .is_none_or(|(_, at)| std::time::Instant::now() >= at);
        for slot in self.plugins.slots.values_mut() {
            let plugins::Slot::Live(l) = slot else {
                continue;
            };
            // Also runs a reload that was asked for.
            if l.handle.pump() {
                rebuild = true;
            }
            let crashed = l.monitor.as_ref().is_some_and(|m| m.is_crashed());
            if crashed != l.crashed {
                l.crashed = crashed;
                rebuild = true;
                // Back from a reload: it missed what was sent to the bypass meanwhile.
                l.fresh = !crashed;
                if crashed {
                    news.push(Warning::PluginCrashed {
                        device: l.name.clone(),
                        plugin: plugin_id(&l.source),
                    });
                }
            }
        }
        let mut error = None;
        if rebuild && let Some(snapshot) = self.last.clone() {
            self.force_rebuild = true;
            match self.sync(&snapshot) {
                // What loading or rebuilding found, which no app sync will show.
                Ok(()) => news.extend(self.warnings.iter().cloned()),
                Err(e) => error = Some(e),
            }
        }
        TickReport {
            warnings: news,
            error,
        }
    }

    /// Cycles the sandboxed plugins missed so far, by not answering in time:
    /// each played a block of silence.
    pub fn plugin_late_cycles(&self) -> u64 {
        self.plugins
            .slots
            .values()
            .filter_map(|s| match s {
                plugins::Slot::Live(l) => l.monitor.as_ref().map(|m| m.late_cycles()),
                _ => None,
            })
            .sum()
    }

    /// Sets how plugin devices are loaded. Devices that could not load for
    /// want of a loader are tried again on the next sync.
    pub fn set_plugin_loader(&mut self, loader: Option<Box<dyn PluginLoader>>) {
        self.plugins.set_loader(loader);
        self.force_rebuild = true;
    }

    /// Takes the plugins out of this session, saving each one's state, so a
    /// session that replaces this one (audio restarted) can carry on with
    /// them through [`set_plugins`](Self::set_plugins). Plugin devices here
    /// are bypassed after the next sync.
    pub fn take_plugins(&mut self) -> Plugins {
        let plugins = self.plugins.detach();
        self.force_rebuild = true;
        plugins
    }

    /// Takes over the plugins from [`take_plugins`](Self::take_plugins):
    /// the loader, and the states to restore. Plugins load on the next sync.
    pub fn set_plugins(&mut self, plugins: Plugins) {
        self.plugins = plugins;
        self.force_rebuild = true;
    }

    /// Frees every plugin, for a new document: the next sync loads them
    /// from the document's state.
    pub fn clear_plugins(&mut self) {
        self.plugins.clear();
        self.force_rebuild = true;
    }

    /// Restarts a crashed (or any) sandboxed plugin and restores its
    /// state. Happens on the next [`tick`](Self::tick). Returns false if the
    /// device has no sandboxed plugin.
    pub fn reload_plugin(&mut self, device: DeviceId) -> bool {
        match self.plugins.live(device).and_then(|l| l.monitor.as_ref()) {
            Some(m) => {
                m.request_reload();
                true
            }
            None => false,
        }
    }

    /// How a plugin device is doing. `None` for built-in devices and
    /// devices not synced yet.
    pub fn plugin_status(&self, device: DeviceId) -> Option<PluginStatus> {
        self.plugins.status(device)
    }

    /// The sandbox of a device's plugin: its process, and how late its
    /// audio has been.
    pub fn plugin_monitor(&self, device: DeviceId) -> Option<daw_plugin_host::SandboxMonitor> {
        self.plugins.live(device)?.monitor.clone()
    }

    /// A loaded plugin's parameters, as it described them when it loaded.
    pub fn plugin_params(&self, device: DeviceId) -> Option<&[daw_core::plugin::ParamInfo]> {
        self.plugins.live(device).map(|l| &l.params[..])
    }

    /// Runs `f` on a device's plugin, on this thread: for parameter
    /// values, editors and state. Keep it short; see
    /// [`daw_engine::PluginHandle::with`]. Edits that belong in the
    /// document (parameters, state) still go through its commands.
    pub fn with_plugin<R>(
        &self,
        device: DeviceId,
        f: impl FnOnce(&mut dyn daw_core::plugin::PluginInstance) -> R,
    ) -> Option<R> {
        let l = self.plugins.live(device)?;
        if l.crashed {
            return None;
        }
        Some(l.handle.with(f))
    }

    /// Saves every running plugin's state and returns those that differ
    /// from the document's, for the app to store (`DeviceField::State`)
    /// before saving the document (`Document::store_plugin_state`). Once
    /// stored and synced, they count as the document's, and are not loaded
    /// back into the plugin. Only devices of the last synced snapshot.
    pub fn plugin_states(&mut self) -> Vec<PluginState> {
        let Some(snapshot) = &self.last else {
            return Vec::new();
        };
        let ids: HashSet<DeviceId> = snapshot
            .all_tracks()
            .flat_map(|t| t.devices.iter().map(|d| d.id))
            .collect();
        let mut out: Vec<PluginState> = self
            .plugins
            .slots
            .iter_mut()
            .filter(|(id, _)| ids.contains(id))
            .filter_map(|(&device, slot)| match slot {
                plugins::Slot::Live(l) => {
                    let state = l.changed_state()?;
                    if let Ok(s) = &state {
                        l.saved = Some(s.clone());
                    }
                    Some(PluginState { device, state })
                }
                plugins::Slot::Failed { .. } => None,
            })
            .collect();
        out.sort_by_key(|s| s.device.0);
        out
    }

    /// Loads a plugin's state into it when the document's changed under it
    /// (undo or redo of a state change).
    fn sync_plugin_states(&mut self, snapshot: &ProjectSnapshot) {
        for d in snapshot.all_tracks().flat_map(|t| t.devices.iter()) {
            let Some(l) = self.plugins.live_mut(d.id) else {
                continue;
            };
            let same = |a: &Option<daw_model::StateBlob>| match (a, &d.state) {
                (Some(a), Some(b)) => Arc::ptr_eq(&a.0, &b.0) || a.0 == b.0,
                (None, None) => true,
                _ => false,
            };
            if same(&l.state) {
                continue;
            }
            // The state the plugin itself handed over for saving: it has it already.
            let own = l.saved.is_some() && same(&l.saved);
            l.saved = None;
            l.state = d.state.clone();
            // A state taken away (undone back to none) leaves the plugin as it is: there is
            // nothing to go back to.
            let Some(state) = d.state.as_ref().filter(|_| !own) else {
                continue;
            };
            // A crashed sandbox keeps the state for its reload.
            match l.handle.with(|p| p.load_state(&state.0)) {
                Ok(()) => {
                    // The state set the plugin's parameters; the document's go on top
                    // again, as when it loads, and again after a rebuild in case the
                    // load changed its ports and the values went to a retired node.
                    l.fresh = true;
                    let key = build::device_key(d.id);
                    if let Some(b) = &mut self.built {
                        b.params.retain(|(k, _), _| *k != key);
                    }
                }
                Err(e) => self.warnings.push(Warning::DeviceStateRejected {
                    device: d.name.clone(),
                    detail: e.to_string(),
                }),
            }
        }
    }

    /// Gives every input tap its track's channels and works out which
    /// tracks monitor.
    fn apply_inputs(&mut self, snapshot: &ProjectSnapshot) {
        self.auto_monitor.clear();
        let Some(built) = &self.built else { return };
        for track in snapshot.tracks.iter() {
            let Some(tap) = built.taps.get(&track.id) else {
                continue;
            };
            tap.set_channels(track.audio_input.channels);
            if track.armed && track.audio_input.monitor == daw_model::Monitoring::Auto {
                self.auto_monitor.insert(track.id);
            }
        }
        self.update_monitors();
    }

    /// Turns input monitoring on or off on each audio track: on while the
    /// track is armed with monitoring on auto, unless the transport plays
    /// without recording that track, so its takes can be heard back.
    /// [`sync`](Self::sync), [`play`](Self::play), [`stop`](Self::stop) and
    /// [`record::AudioTakes`] call it; call it after starting or stopping a
    /// recording by hand through [`tap_mut`](Self::tap_mut).
    pub fn update_monitors(&mut self) {
        let playing = self.playing;
        let Some(built) = &self.built else { return };
        for (id, tap) in &built.taps {
            let on = self.auto_monitor.contains(id) && (!playing || tap.is_recording());
            tap.set_monitor(on);
        }
    }

    /// The input tap of an audio track, for recording.
    pub fn tap_mut(&mut self, track: TrackId) -> Option<&mut capture::TapHandle> {
        self.built.as_mut()?.taps.get_mut(&track)
    }

    /// Whether an audio track is playing its input through right now.
    pub fn is_monitoring(&self, track: TrackId) -> bool {
        self.built
            .as_ref()
            .and_then(|b| b.taps.get(&track))
            .is_some_and(|t| t.monitoring())
    }

    /// Leaves the automation lanes in `lanes` out of playback, so a control
    /// being recorded over them is heard instead. Takes effect on the next
    /// [`sync`](Self::sync).
    ///
    /// Lanes are compiled into the graph, so this rebuilds it. Every node is
    /// reused, so nothing is heard or lost, but the build costs as much as
    /// any routing edit.
    pub fn set_suspended_lanes(&mut self, lanes: HashSet<LaneId>) {
        self.suspended = lanes;
    }

    pub fn suspended_lanes(&self) -> &HashSet<LaneId> {
        &self.suspended
    }

    /// Holds a parameter at a value that isn't in the document, such as a
    /// fader being recorded as automation: sent now, and kept over the
    /// document's value by every [`sync`](Self::sync) until cleared with
    /// `None`. After clearing, the next sync sends the document's value.
    pub fn set_live_param(
        &mut self,
        key: NodeKey,
        param: u32,
        value: Option<f64>,
    ) -> Result<(), SessionError> {
        let Some(value) = value else {
            self.live.remove(&(key, param));
            return Ok(());
        };
        self.live.insert((key, param), value);
        self.controller.set_param_by_key(key, param, value)?;
        if let Some(b) = &mut self.built {
            b.params.insert((key, param), value);
        }
        Ok(())
    }

    /// The engine node key for a send.
    pub fn send_key(send: SendId) -> NodeKey {
        build::send_key(send)
    }

    /// A model automation lane's points as the engine plays them: in
    /// samples, values in the lane's own unit.
    pub fn engine_lane(&self, points: &[daw_model::AutomationPoint]) -> AutomationLane {
        AutomationLane::new(
            points
                .iter()
                .map(|p| daw_engine::AutomationPoint {
                    time: self.ticks_to_samples(p.tick),
                    value: p.value,
                    curve: if p.step { Curve::Step } else { Curve::Linear },
                })
                .collect(),
        )
    }

    fn send_params(&mut self, snapshot: &ProjectSnapshot) -> Result<(), SessionError> {
        let built = self.built.as_mut().expect("built before params");
        for ((key, param), value) in build::params(snapshot) {
            let value = self.live.get(&(key, param)).copied().unwrap_or(value);
            if built.params.get(&(key, param)) != Some(&value) {
                self.controller.set_param_by_key(key, param, value)?;
                built.params.insert((key, param), value);
            }
        }
        Ok(())
    }

    fn send_clips(&mut self, snapshot: &ProjectSnapshot, all: bool) {
        let mut built = self.built.take().expect("built before clips");
        for track in snapshot.all_tracks() {
            let Some(handle) = built.midi.get_mut(&track.id) else {
                continue;
            };
            if !all && same_clips(built.midi_clips.get(&track.id), &track.clips) {
                continue;
            }
            let seq = daw_midi::model::compile_track(track, &self.tempo, self.sample_rate());
            if handle.set_sequence(seq).is_err() {
                // Left marked as stale, so the next sync tries again.
                self.warnings.push(Warning::NotesDropped {
                    track: track.name.clone(),
                });
                built.midi_clips.remove(&track.id);
                continue;
            }
            built.midi_clips.insert(track.id, track.clips.clone());
        }
        for track in snapshot.all_tracks() {
            let Some(handle) = built.players.get_mut(&track.id) else {
                continue;
            };
            if !all && same_clips(built.clips.get(&track.id), &track.clips) {
                continue;
            }
            let set = build::clip_set(self, track, handle);
            if handle.send(set).is_err() {
                self.warnings.push(Warning::ClipsDropped {
                    track: track.name.clone(),
                });
                built.clips.remove(&track.id);
                continue;
            }
            built.clips.insert(track.id, track.clips.clone());
        }
        let keep: HashSet<PathBuf> = snapshot
            .all_tracks()
            .flat_map(|t| t.clips.iter())
            .filter_map(|c| match &c.content {
                daw_model::ClipContent::Audio(a) => Some(self.media.resolve(&a.source)),
                _ => None,
            })
            .collect();
        self.media.retain(&keep);
        self.built = Some(built);
    }

    /// Frees graphs and clip sets the audio thread has finished with. Call on
    /// a timer (the UI's refresh tick is fine).
    pub fn collect_garbage(&mut self) {
        self.controller.collect_garbage();
        if let Some(b) = &mut self.built {
            for p in b.players.values_mut() {
                p.collect_garbage();
            }
            for p in b.midi.values_mut() {
                p.collect_garbage();
            }
            for t in b.taps.values_mut() {
                t.collect_garbage();
            }
        }
    }

    pub fn play(&mut self) -> Result<(), SessionError> {
        self.controller.play()?;
        self.playing = true;
        self.update_monitors();
        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), SessionError> {
        self.controller.stop()?;
        self.playing = false;
        self.update_monitors();
        Ok(())
    }

    /// Whether the transport was last told to play.
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn locate(&mut self, t: TickTime) -> Result<(), SessionError> {
        let s = self.ticks_to_samples(t);
        Ok(self.controller.locate(s)?)
    }

    /// The play position the audio thread last reported.
    pub fn position(&self) -> TickTime {
        self.samples_to_ticks(self.controller.position())
    }

    /// Loops `[start, end)` while playing. `None` (or an empty range) turns
    /// looping off.
    pub fn set_loop(&mut self, range: Option<(TickTime, TickTime)>) -> Result<(), SessionError> {
        self.loop_ticks = range.filter(|(s, e)| e > s);
        self.apply_loop()
    }

    pub fn loop_range(&self) -> Option<(TickTime, TickTime)> {
        self.loop_ticks
    }

    fn apply_loop(&mut self) -> Result<(), SessionError> {
        let range = self
            .loop_ticks
            .map(|(s, e)| (self.ticks_to_samples(s), self.ticks_to_samples(e)));
        self.loop_points.set(range);
        self.controller
            .set_loop(range.map(|(start, end)| LoopRange { start, end }))?;
        Ok(())
    }

    pub fn ticks_to_samples(&self, t: TickTime) -> SampleTime {
        self.tempo.ticks_to_samples(t, self.sample_rate())
    }

    pub fn samples_to_ticks(&self, s: SampleTime) -> TickTime {
        self.tempo.samples_to_ticks(s, self.sample_rate())
    }

    /// The peak meter on a track's strip (the master's included), for the
    /// mixer and track headers.
    pub fn track_meter(&self, track: TrackId) -> Option<Arc<NodeMeter>> {
        self.controller.meter(self.strip_key(track)?)
    }

    /// A device's gain reduction meter, if it has one (the compressor).
    pub fn device_meter(&self, device: DeviceId) -> Option<Arc<PeakMeter>> {
        self.built.as_ref()?.device_meters.get(&device).cloned()
    }

    /// The engine node key for a track's strip, for direct engine access.
    pub fn strip_key(&self, track: TrackId) -> Option<NodeKey> {
        let master = self.master?;
        Some(if track == master {
            NodeKey::MASTER
        } else {
            NodeKey(track.0.as_u128())
        })
    }

    /// The engine node key for a device.
    pub fn device_key(device: DeviceId) -> NodeKey {
        build::device_key(device)
    }

    /// Whether a clip is currently given to a player (for tests).
    pub fn has_clip(&self, track: TrackId, clip: ClipId) -> bool {
        self.built
            .as_ref()
            .and_then(|b| b.clips.get(&track))
            .is_some_and(|c| c.iter().any(|x| x.id == clip))
    }
}

/// Linear strip gain for a fader in dB, within the strip's range.
pub fn db_to_gain(db: f64) -> f64 {
    if db <= -144.0 {
        0.0
    } else {
        10f64.powf(db / 20.0).min(4.0)
    }
}

/// How warnings name a plugin.
pub(crate) fn plugin_id(source: &daw_model::DeviceSource) -> String {
    match source {
        daw_model::DeviceSource::Plugin { id, .. } => id.clone(),
        daw_model::DeviceSource::Builtin { kind } => kind.clone(),
    }
}

/// Whether `new` holds the very clips `old` did.
fn same_clips(old: Option<&Vec<Arc<daw_model::Clip>>>, new: &[Arc<daw_model::Clip>]) -> bool {
    old.is_some_and(|old| {
        old.len() == new.len() && old.iter().zip(new).all(|(a, b)| Arc::ptr_eq(a, b))
    })
}
