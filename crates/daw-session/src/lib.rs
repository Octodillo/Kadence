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
pub mod catalog;
pub mod host;
mod media;
pub mod player;
pub mod plugins;
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
    plugins: Plugins,
    /// The snapshot last synced, for rebuilds the plugins ask for.
    last: Option<ProjectSnapshot>,
    /// Rebuild the graph on the next sync even if the project's shape is
    /// unchanged.
    force_rebuild: bool,
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
            plugins: Plugins::default(),
            last: None,
            force_rebuild: false,
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
        // Paths may resolve differently now; rebuild everything.
        self.built = None;
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
            self.controller.set_graph(desc)?;
            self.built = Some(built);
            let ids: HashSet<DeviceId> = snapshot
                .all_tracks()
                .flat_map(|t| t.devices.iter().map(|d| d.id))
                .collect();
            // After the swap, so the running graph's nodes of removed plugins aren't
            // silenced while it still plays.
            self.plugins.retain(|id| ids.contains(&id));
        }
        self.sync_plugin_states(snapshot);
        self.send_params(snapshot)?;
        self.send_clips(snapshot, tempo_changed);
        Ok(())
    }

    /// Runs the plugins' main-thread work and frees what the audio thread
    /// is done with. Call it about 60 times a second, on the session's
    /// thread.
    ///
    /// Rebuilds the graph when a plugin asks for a new layout or latency,
    /// when a sandboxed plugin crashed (it is bypassed) or came back, and
    /// when the last graph could not be sent. Returns what the user should
    /// hear about: plugins that crashed since the last tick.
    pub fn tick(&mut self) -> Result<Vec<Warning>, SessionError> {
        self.collect_garbage();
        let mut news = Vec::new();
        let mut rebuild = self.built.is_none();
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
        if rebuild && let Some(snapshot) = self.last.clone() {
            self.force_rebuild = true;
            self.sync(&snapshot)?;
        }
        Ok(news)
    }

    /// Sets how plugin devices are loaded. Devices that could not load for
    /// want of a loader are tried again on the next sync.
    pub fn set_plugin_loader(&mut self, loader: Option<Box<dyn PluginLoader>>) {
        self.plugins.set_loader(loader);
        self.force_rebuild = true;
    }

    /// Tells the plugin loader which plugins are installed, from a scan the
    /// app ran (see [`catalog`]). Devices whose plugin was missing are tried
    /// again on the next sync.
    pub fn set_known_plugins(&mut self, plugins: &[daw_core::plugin::PluginDescriptor]) {
        self.plugins.set_known(plugins);
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
    /// before saving the document. Once stored and synced, they count as
    /// the document's.
    pub fn plugin_states(&self) -> Vec<PluginState> {
        let mut out: Vec<PluginState> = self
            .plugins
            .slots
            .iter()
            .filter_map(|(&device, slot)| match slot {
                plugins::Slot::Live(l) => {
                    l.changed_state().map(|state| PluginState { device, state })
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
            let same = match (&l.state, &d.state) {
                (Some(a), Some(b)) => Arc::ptr_eq(&a.0, &b.0) || a.0 == b.0,
                (None, None) => true,
                _ => false,
            };
            if same {
                continue;
            }
            l.state = d.state.clone();
            // A state taken away (undone back to none) leaves the plugin as it is: there is
            // nothing to go back to.
            if let Some(state) = &d.state
                && !l.crashed
                && let Err(e) = l.handle.with(|p| p.load_state(&state.0))
            {
                self.warnings.push(Warning::DeviceStateRejected {
                    device: d.name.clone(),
                    detail: e.to_string(),
                });
            }
        }
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
        }
    }

    pub fn play(&mut self) -> Result<(), SessionError> {
        self.controller.play()?;
        self.playing = true;
        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), SessionError> {
        self.controller.stop()?;
        self.playing = false;
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
