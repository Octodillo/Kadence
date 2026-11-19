//! Plays a [`daw_model`] project on the [`daw_engine`].
//!
//! A [`Session`] owns the engine's controller half. Each time the document
//! changes, the app hands it the new [`ProjectSnapshot`] through
//! [`Session::sync`], and the session works out the cheapest way to make the
//! engine match:
//!
//! * Routing, device chains or the track list changed: build a new
//!   [`GraphDesc`] and swap it in. Running nodes are kept; a built-in
//!   device whose saved state changed (a sampler given a new sample) gets
//!   a new node, its samples decoded here before the swap, so the audio
//!   thread is only handed the finished node.
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
pub mod catalog;
mod drops;
pub mod export;
pub mod host;
mod media;
pub mod player;
pub mod plugins;
pub mod record;
mod samples;
mod state_files;
mod tempo;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use daw_core::level;
use daw_core::meter::PeakMeter;
use daw_core::plugin::PluginError;
use daw_core::time::{SampleTime, TempoMap, TickTime};
use daw_engine::{
    AutomationLane, Curve, EngineConfig, EngineController, EngineError, EngineProcessor, LoopRange,
    NodeKey, NodeMeter,
};
use daw_model::{ClipId, DeviceId, LaneId, ProjectSnapshot, SendId, TrackId};
use daw_plugin_host::Realtime;

pub use build::METRONOME_KEY;
pub use build::effective_mutes;
pub use media::{MEMORY_LIMIT_SECONDS, MediaPool};
use player::{LoopPoints, PlayerHandle};
pub use plugins::{
    LoadError, LoadedPlugin, PluginLoader, PluginState, PluginStatus, Plugins, SandboxLoader,
};
pub use state_files::{BuiltinStateFiles, is_sampler};
pub use tempo::tempo_map;

/// Length of the straight pieces a curved automation segment is played
/// as, in samples, and the most pieces one segment gets.
const PIECE_SAMPLES: i64 = 256;
const MAX_PIECES: i64 = 256;

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

/// The click. Not part of the document: whether you hear it is a playing
/// preference, so it is neither undone nor saved with the project.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct MetronomeSettings {
    /// Click while playing. A count-in clicks either way.
    pub enabled: bool,
    pub volume_db: f64,
}

impl Default for MetronomeSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            volume_db: -6.0,
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
    /// A device refused its saved state; it runs with its defaults. A plugin's refused
    /// state stays in the project until the plugin takes another or is changed in its editor.
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
    /// The system grants the sandboxed plugins' hosts no real-time priority, neither directly
    /// nor through RealtimeKit or the desktop portal: one was refused and none got it. Plugin
    /// audio may then drop out while the machine is busy. Said once, as it is about the
    /// system, not one plugin. A host refused while others got it is RealtimeKit's rationing,
    /// which this does not say; [`Session::plugin_realtime`] tells each plugin's.
    #[error("plugin hosts get no real-time priority; their audio may drop out")]
    PluginsNotRealtime,
    /// A plugin would not hand over its state (or take back the one it
    /// handed over before audio restarted or its device was removed), so
    /// the project keeps the one it had and what was changed in the plugin
    /// since is lost. `detail` is the plugin's or the format's own message.
    #[error("{device} did not save its plugin state ({detail})")]
    PluginStateNotSaved { device: String, detail: String },
    /// A plugin's own editor would not open. `detail` is the plugin's or
    /// the format's own message.
    #[error("the editor of {device} would not open ({detail})")]
    PluginEditorFailed { device: String, detail: String },
    /// A plugin's own editor would not open because no X11 display can be
    /// reached: plugin editors are X11 windows (XWayland under Wayland).
    #[error("the editor of {device} needs an X11 display, and none is reachable")]
    NoPluginDisplay { device: String },
    /// An audio file could not be streamed; its clips are silent.
    #[error("can't stream {}: {detail}", path.display())]
    CannotStream { path: PathBuf, detail: String },
    /// An audio file could not be read; the clips or sampler zones that
    /// play it are silent.
    #[error("can't read {}", path.display())]
    CannotRead { path: PathBuf },
    /// A track's notes did not reach the engine; the next sync retries.
    #[error("note update for track {track} was dropped")]
    NotesDropped { track: String },
    /// A track's clips did not reach the engine; the next sync retries.
    #[error("clip update for track {track} was dropped")]
    ClipsDropped { track: String },
    /// A track's output or send would have made audio loop back into it,
    /// so it is left out. Edits never allow this; a damaged project could.
    #[error("routing from track {track} would loop; it is left out")]
    RoutingLoop { track: String },
    /// None of an instrument track's devices takes notes, so its notes
    /// play nothing.
    #[error("no device on track {track} takes notes")]
    NotesUnplayed { track: String },
    /// A device takes no parameter changes while it plays, so a lane on
    /// one of its parameters does nothing.
    #[error("device {device} can't be automated; its lane is left out")]
    LaneSkipped { device: String },
}

impl Warning {
    /// Why `device`'s editor would not open, from the plugin's error.
    pub fn editor_failed(device: String, e: &PluginError) -> Self {
        match e {
            PluginError::NoDisplay(_) => Self::NoPluginDisplay { device },
            e => Self::PluginEditorFailed {
                device,
                detail: e.to_string(),
            },
        }
    }
}

/// What the running graph was built from, and what was last sent to it.
#[derive(Default)]
pub(crate) struct Built {
    pub structure: build::Structure,
    pub players: HashMap<TrackId, PlayerHandle>,
    /// Last value sent per (node, param), so unchanged values aren't resent.
    pub params: HashMap<(NodeKey, u32), f64>,
    /// Parameters an automation lane drives in this graph.
    pub automated: HashSet<(NodeKey, u32)>,
    /// What each automated (or, during touch write, suspended) device
    /// parameter the document leaves unset goes back to when its lane
    /// stops. See `build::rest_value`.
    pub rest: HashMap<(NodeKey, u32), f64>,
    /// Parameters whose lane stopped in this graph, with their [`rest`]
    /// values; sent by the next parameter sync unless the document has a
    /// value of its own for them.
    ///
    /// [`rest`]: Self::rest
    pub restore: HashMap<(NodeKey, u32), f64>,
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
    /// The files of audio tracks' clips that could not be read, as they
    /// were then: every sync looks whether one is there now, or another.
    pub unread: HashMap<TrackId, Vec<media::Seen>>,
}

/// The engine as the model sees it. See the crate docs.
pub struct Session {
    controller: EngineController,
    config: SessionConfig,
    media: MediaPool,
    samples: samples::SamplePool,
    tempo: daw_core::time::SegmentTempoMap,
    tempo_data: Option<daw_model::TempoData>,
    loop_points: Arc<LoopPoints>,
    loop_ticks: Option<(TickTime, TickTime)>,
    built: Option<Built>,
    warnings: Vec<Warning>,
    drop_log: drops::DropLog,
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
    /// What each plugin parameter in `live` had before it was first held:
    /// for a lane written by hand, the value removing the lane goes back
    /// to. Kept only while held, or until the next rebuild after.
    pub(crate) pre_live: HashMap<(NodeKey, u32), f64>,
    /// MIDI source port names, by the engine event source their own port
    /// feeds. Tracks that pick one of these devices get only its events.
    midi_sources: HashMap<String, usize>,
    /// `midi_sources` changed since the graph was built.
    midi_sources_changed: bool,
    /// Each instrument track's arm state and channel filter, as of the
    /// last sync, for [`apply_gates`](Self::apply_gates).
    gates: Vec<(TrackId, bool, Option<u8>)>,
    /// The track that takes live MIDI when none is armed.
    input_focus: Option<TrackId>,
    metronome: MetronomeSettings,
    /// The tick the playhead was last sent to. Until playback moves it,
    /// this is where it is, even before the audio thread says so.
    parked: Option<TickTime>,
    /// Rendering to a file rather than playing: players wait for the disk.
    pub(crate) offline: Option<Arc<player::Offline>>,
    plugins: Plugins,
    /// The snapshot last synced, for rebuilds the plugins ask for.
    last: Option<ProjectSnapshot>,
    /// Rebuild the graph on the next sync even if the project's shape is
    /// unchanged.
    force_rebuild: bool,
    /// The tempo changed and what is kept in samples has not all followed
    /// yet: clips, notes and automation.
    retime: bool,
    /// The loop has not followed the tempo (or the engine refused it).
    loop_stale: bool,
    /// Where the playhead still has to go to stay put over a tempo change.
    relocate: Option<TickTime>,
    /// The project bundle moved and samplers have not loaded from the new
    /// place yet.
    pub(crate) root_moved: bool,
    pub(crate) sent: SentParams,
    /// After the engine refused a graph: how long [`tick`](Self::tick)
    /// waits before it tries again, and when.
    retry: Option<(std::time::Duration, std::time::Instant)>,
    /// Audio tracks that monitor automatically (armed, monitoring on
    /// auto), as of the last sync.
    auto_monitor: HashSet<TrackId>,
}

/// What the session has sent the running graph's nodes lately, to notice
/// values that did not arrive.
///
/// The engine keeps [`PER_NODE`](Self::PER_NODE) parameter changes per
/// node between two audio cycles and drops the rest. So a node is sent no
/// more than that until the audio thread has surely taken them (see
/// [`EngineController::cycles`]), and the rest is left for a later sync.
#[derive(Default)]
pub(crate) struct SentParams {
    /// Per node, the changes the audio thread may not have taken yet: how
    /// many were sent during which cycle, oldest first.
    waiting: HashMap<NodeKey, Vec<(u64, usize)>>,
    /// There is more to send (a node had no room, or the engine's queue
    /// was full): the cycle that was found in.
    deferred: Option<u64>,
    /// How many graphs the audio thread had swapped in when `since_swap`
    /// was started.
    swaps: u64,
    /// Nodes sent a change since that swap, which the running graph may
    /// not have played yet.
    since_swap: HashSet<NodeKey>,
}

impl SentParams {
    /// Changes the engine keeps per node between two cycles
    /// (`PARAM_QUEUE` in `daw_engine::compile`).
    const PER_NODE: usize = 64;

    /// Starts a round of sends: forgets what the audio thread has taken
    /// by now.
    fn begin(&mut self, controller: &EngineController) {
        self.deferred = None;
        let now = controller.cycles();
        self.waiting.retain(|_, sent| {
            sent.retain(|&(cycle, _)| now < cycle + 2);
            !sent.is_empty()
        });
    }

    /// Whether `key` can be sent one more change. Without room, there is
    /// more to send.
    fn room(&mut self, controller: &EngineController, key: NodeKey) -> bool {
        let now = controller.cycles();
        let waiting: usize = self.waiting.get(&key).map_or(0, |sent| {
            let sent = sent.iter().filter(|&&(cycle, _)| now < cycle + 2);
            sent.map(|&(_, n)| n).sum()
        });
        let room = waiting < Self::PER_NODE;
        if !room {
            self.more_to_send(controller);
        }
        room
    }

    /// Something was left for a later sync, which is worth a try once the
    /// audio thread has moved on.
    fn more_to_send(&mut self, controller: &EngineController) {
        self.deferred = Some(controller.cycles());
    }

    fn wants_sync(&self, controller: &EngineController) -> bool {
        self.deferred
            .is_some_and(|then| controller.cycles() != then)
    }

    fn record(&mut self, controller: &EngineController, key: NodeKey) {
        let now = controller.cycles();
        let sent = self.waiting.entry(key).or_default();
        match sent.last_mut() {
            Some((cycle, n)) if *cycle == now => *n += 1,
            _ => sent.push((now, 1)),
        }
        self.since_last_swap(controller).insert(key);
    }

    /// The nodes sent a change since the last graph swap this side saw.
    fn since_last_swap(&mut self, controller: &EngineController) -> &mut HashSet<NodeKey> {
        let swaps = controller.graphs_swapped();
        if std::mem::replace(&mut self.swaps, swaps) != swaps {
            self.since_swap.clear();
        }
        &mut self.since_swap
    }

    /// Whether `key` was sent a change that the audio thread may not have
    /// played before it swaps the next graph in: one sent since the last
    /// swap this side saw.
    pub(crate) fn maybe_unplayed(&mut self, controller: &EngineController, key: NodeKey) -> bool {
        self.since_last_swap(controller).contains(&key)
    }
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
    /// Plugin devices whose plugin said its state changed while its editor was open (a change
    /// the plugin noticed as its editor closed included), since the last tick. The app can record their new state
    /// ([`plugin_state`](Session::plugin_state)) as an edit.
    pub state_changed: Vec<DeviceId>,
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
            samples: Default::default(),
            config,
            tempo: Default::default(),
            tempo_data: None,
            loop_points: Arc::default(),
            loop_ticks: None,
            built: None,
            warnings: Vec::new(),
            drop_log: Default::default(),
            playing: false,
            master: None,
            suspended: HashSet::new(),
            live: HashMap::new(),
            pre_live: HashMap::new(),
            midi_sources: HashMap::new(),
            midi_sources_changed: false,
            gates: Vec::new(),
            input_focus: None,
            metronome: MetronomeSettings::default(),
            parked: None,
            offline: None,
            plugins: Plugins::default(),
            last: None,
            force_rebuild: false,
            retime: false,
            loop_stale: false,
            relocate: None,
            root_moved: false,
            sent: SentParams::default(),
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
        // A graph the engine refused is [`tick`](Self::tick)'s to send
        // again, when its wait is over.
        if self.retry.is_some() {
            return false;
        }
        self.midi_sources_changed
            || self.retime
            || self.loop_stale
            || self.sent.wants_sync(&self.controller)
            || self.built.as_ref().is_some_and(|b| {
                b.midi.keys().any(|t| !b.midi_clips.contains_key(t))
                    || b.players.keys().any(|t| !b.clips.contains_key(t))
            })
    }

    /// Tells the session which engine event source each MIDI device's own
    /// port feeds (see [`host::AudioHost`]). A track whose MIDI input names
    /// a device takes only that source; one naming a device not in `sources`
    /// takes nothing. Takes effect on the next [`sync`](Self::sync), which
    /// [`wants_sync`](Self::wants_sync) asks for.
    pub fn set_midi_sources(&mut self, sources: HashMap<String, usize>) {
        if sources != self.midi_sources {
            self.midi_sources = sources;
            self.midi_sources_changed = true;
        }
    }

    pub fn midi_sources(&self) -> &HashMap<String, usize> {
        &self.midi_sources
    }

    /// The instrument track that takes live MIDI while no track is armed
    /// (the selected one, say). With neither, every instrument track does.
    pub fn set_input_focus(&mut self, track: Option<TrackId>) {
        if self.input_focus != track {
            self.input_focus = track;
            self.apply_gates();
        }
    }

    /// Opens the input gate of each instrument track that should hear live
    /// MIDI: the armed ones, else the focus track, else all of them.
    fn apply_gates(&mut self) {
        let Some(built) = self.built.as_mut() else {
            return;
        };
        let any_armed = self.gates.iter().any(|&(_, armed, _)| armed);
        let focus = self
            .input_focus
            .filter(|f| self.gates.iter().any(|(t, _, _)| t == f));
        for &(track, armed, channel) in &self.gates {
            let open = if any_armed {
                armed
            } else {
                focus.is_none_or(|f| f == track)
            };
            if let Some(player) = built.midi.get(&track) {
                player.set_input(daw_midi::player::InputGate { open, channel });
            }
        }
    }

    /// The MIDI player of an instrument track, for record arming and
    /// collecting what was played.
    pub fn midi_player(&mut self, track: TrackId) -> Option<&mut daw_midi::player::PlayerHandle> {
        self.built.as_mut()?.midi.get_mut(&track)
    }

    pub fn media_mut(&mut self) -> &mut MediaPool {
        &mut self.media
    }

    /// Takes `sample` as the file at `path` already decoded, so a sampler
    /// given that file by the next [`sync`](Self::sync) plays this buffer
    /// instead of reading the file again. Kept for a while after nothing
    /// holds it any more, as every sample a sampler plays is.
    pub fn hold_sample(&mut self, path: PathBuf, sample: &Arc<daw_devices::SampleBuffer>) {
        self.samples.hold(path, sample);
    }

    /// The directory relative clip paths are resolved against (the project
    /// bundle). Takes effect on the next [`sync`](Self::sync).
    pub fn set_root(&mut self, root: Option<PathBuf>) {
        if self.media.root() == root.as_deref() {
            return;
        }
        self.media.set_root(root);
        // Only what depends on the root starts over, so saving a new
        // project while it plays or records is not heard: every player
        // gets its clips again (and keeps the streams of files that did
        // not move), and samplers load their samples again.
        if let Some(b) = &mut self.built {
            b.clips.clear();
        }
        self.root_moved = true;
    }

    /// Reads every audio file again on the next [`sync`](Self::sync), for
    /// a new document: files that could not be read before, or that
    /// changed on disk, are looked at afresh.
    pub fn reload_media(&mut self) {
        self.media.clear();
        self.samples.forget_unused();
        if let Some(b) = &mut self.built {
            b.clips.clear();
        }
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
            let keep = self
                .tempo_data
                .is_some()
                .then(|| self.kept_tick())
                .flatten();
            // Tempo and time signatures, so the metronome and plugins see
            // the project's bars and beats.
            let timeline = daw_midi::model::timeline(&snapshot.tempo);
            // Marked as sent only once it is, so a full queue retries next sync.
            self.controller.set_timeline(timeline.clone())?;
            self.tempo = timeline.tempo;
            self.tempo_data = Some(snapshot.tempo.clone());
            // Everything kept in samples follows, in the steps below. Each
            // stays owed until it is done, should the engine refuse one.
            self.retime = true;
            self.loop_stale = true;
            self.relocate = keep;
        }
        if self.loop_stale {
            self.apply_loop()?;
        }
        if let Some(at) = self.relocate {
            self.locate(at)?;
        }
        self.last = Some(snapshot.clone());
        let structure = build::Structure::of(snapshot, &self.suspended, &self.midi_sources);
        let sources_changed = std::mem::take(&mut self.midi_sources_changed);
        // Automation is in samples, so a tempo change re-times it.
        let rebuild = self.force_rebuild
            || (self.root_moved && structure.has_sampler())
            || self.built.as_ref().is_none_or(|b| {
                b.structure != structure || (self.retime && structure.has_automation())
            });
        if rebuild {
            let old = self.built.take().unwrap_or_default();
            let old_live: Vec<(TrackId, build::LiveInput)> = old
                .midi
                .keys()
                .map(|&t| (t, old.structure.live_input(t)))
                .collect();
            let (desc, build) = build::graph(self, snapshot, structure, old);
            if let Err(e) = self.controller.set_graph(desc) {
                let backoff = self
                    .retry
                    .map_or(RETRY_MIN, |(b, _)| (b * 2).min(RETRY_MAX));
                self.retry = Some((backoff, std::time::Instant::now() + backoff));
                // The old graph plays on: its bookkeeping is kept, with the
                // handles of its players, for the next try to build on.
                self.built = Some(build.revert(&mut self.plugins));
                self.midi_sources_changed = sources_changed;
                self.force_rebuild = true;
                return Err(e.into());
            }
            let built = build.built;
            self.retry = None;
            self.force_rebuild = false;
            // A track whose live MIDI now comes from elsewhere would never
            // get the note-offs of what it holds.
            for (track, was) in old_live {
                if (sources_changed || built.structure.live_input(track) != was)
                    && let Some(player) = built.midi.get(&track)
                {
                    player.end_live();
                }
            }
            self.built = Some(built);
            // Taken into `rest` by this build, if a lane came of it.
            let live = &self.live;
            self.pre_live.retain(|k, _| live.contains_key(k));
            let ids: HashSet<DeviceId> = snapshot
                .all_tracks()
                .flat_map(|t| t.devices.iter().map(|d| d.id))
                .collect();
            // After the new graph is queued, so the nodes of removed plugins aren't
            // silenced while it is being built. Until the audio thread swaps it in (a
            // block at most), the old graph plays them as silence.
            self.plugins.retain(|id| ids.contains(&id));
        }
        // Samplers, if there are any, went into that graph.
        self.root_moved = false;
        self.sync_plugin_states(snapshot);
        self.gates = snapshot
            .tracks
            .iter()
            .filter(|t| t.kind == daw_model::TrackKind::Instrument)
            .map(|t| (t.id, t.armed, t.midi_input.channel))
            .collect();
        self.apply_gates();
        // A parameter the engine would not take must not keep the clips
        // from being sent.
        let params = self.send_params(snapshot);
        self.send_clips(snapshot, self.retime);
        self.retime = false;
        self.apply_inputs(snapshot);
        params?;
        // Plugin state that was lost, first: said once, and only by a sync
        // whose warnings are shown.
        let mut warnings = std::mem::take(&mut self.plugins.owed);
        warnings.append(&mut self.warnings);
        self.warnings = warnings;
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
        self.drop_log.report(&self.controller);
        let mut news = Vec::new();
        // A graph the engine refused is sent again.
        let mut rebuild = self
            .retry
            .is_some_and(|(_, at)| std::time::Instant::now() >= at);
        let mut state_changed = Vec::new();
        for (&device, slot) in &mut self.plugins.slots {
            let plugins::Slot::Live(l) = slot else {
                continue;
            };
            // Also runs a reload that was asked for.
            if l.handle.pump() {
                rebuild = true;
            }
            let (changed, open) = l
                .handle
                .with(|p| (p.take_state_changed(), p.is_editor_open()));
            // Without an editor, a report is more likely the host's doing (automation) than
            // the user's, and saving the document picks the state up anyway. Closing the
            // editor is no report of its own: the state then also holds what the host set
            // (parameters, automation), which is no change of the plugin's.
            let was_open = std::mem::replace(&mut l.editor_open, open);
            let changed = !std::mem::take(&mut l.ignore_report) && changed;
            if changed && (was_open || open) && !l.crashed {
                // The user's work from here on, whatever state it started from.
                l.given = plugins::Given::Taken;
                state_changed.push(device);
            }
            let crashed = l.monitor.as_ref().is_some_and(|m| m.is_crashed());
            if crashed != l.crashed {
                l.crashed = crashed;
                rebuild = true;
                // Back from a reload: it missed what was sent to the bypass meanwhile.
                l.fresh = !crashed;
                if crashed {
                    l.host_lost();
                    news.push(Warning::PluginCrashed {
                        device: l.name.clone(),
                        plugin: plugin_id(&l.source),
                    });
                }
            }
        }
        if self.plugins.says_not_realtime() {
            news.push(Warning::PluginsNotRealtime);
        }
        // States a plugin could not be asked about before (busy, or its host
        // down): now that it runs, it takes or refuses them.
        let due: Vec<(DeviceId, daw_model::StateBlob)> = self
            .plugins
            .slots
            .iter()
            .filter_map(|(&device, slot)| match slot {
                plugins::Slot::Live(l) => Some((device, l.due()?)),
                plugins::Slot::Failed { .. } => None,
            })
            .collect();
        for (device, state) in due {
            if let Some(refusal) = self.give_plugin_state(device, state) {
                news.push(refusal);
            }
            // The state set the plugin's parameters: the document's go on top.
            rebuild = true;
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
        state_changed.sort_by_key(|d| d.0);
        TickReport {
            warnings: news,
            error,
            state_changed,
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

    /// Tells the plugin loader which plugins are installed, from a scan the
    /// app ran (see [`catalog`]). Devices whose plugin did not load are tried
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

    /// How the audio thread of a device's sandboxed plugin is scheduled.
    /// `None` for built-in devices, plugins that are not loaded and plugins
    /// without a sandbox.
    pub fn plugin_realtime(&self, device: DeviceId) -> Option<Realtime> {
        Some(self.plugins.live(device)?.monitor.as_ref()?.realtime())
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

    /// Whether a device's plugin has an editor of its own. False for a plugin that is not
    /// running (crashed, missing) and for built-in devices.
    pub fn plugin_has_editor(&self, device: DeviceId) -> bool {
        self.with_plugin(device, |p| p.has_editor())
            .unwrap_or(false)
    }

    /// Opens a device's plugin editor in its own window, titled `title`. An editor that is
    /// open already stays as it is. The error is the plugin's or the format's own;
    /// [`Warning::editor_failed`] words it for the user.
    pub fn open_plugin_editor(&mut self, device: DeviceId, title: &str) -> Result<(), PluginError> {
        let Some(l) = self.plugins.live_mut(device).filter(|l| !l.crashed) else {
            return Err(PluginError::Editor("the plugin is not running".into()));
        };
        // Every format leaves an open editor as it is. Asked even when it reads as open: a
        // sandboxed editor the user just closed reads as open until the next tick.
        let opened = l.handle.with(|p| {
            p.set_editor_title(title);
            p.open_editor()
        });
        opened?;
        l.editor_open = true;
        l.editor_title = Some(title.to_owned());
        Ok(())
    }

    /// Closes a device's plugin editor.
    pub fn close_plugin_editor(&mut self, device: DeviceId) {
        self.with_plugin(device, |p| p.close_editor());
    }

    /// Whether a device's plugin editor is open.
    pub fn plugin_editor_open(&self, device: DeviceId) -> bool {
        self.with_plugin(device, |p| p.is_editor_open())
            .unwrap_or(false)
    }

    /// Retitles a device's plugin editor window, for a renamed device or track. Cheap when
    /// the title is unchanged.
    pub fn set_plugin_editor_title(&mut self, device: DeviceId, title: &str) {
        let Some(l) = self.plugins.live_mut(device).filter(|l| !l.crashed) else {
            return;
        };
        if l.editor_title.as_deref() != Some(title) {
            l.editor_title = Some(title.to_owned());
            l.handle.with(|p| p.set_editor_title(title));
        }
    }

    /// Saves one device's plugin state, if it differs from the document's, for the app to
    /// record as an edit (`SetDevice` with `DeviceField::State`). Nothing for a plugin that
    /// refused the document's state: what it holds instead is not the user's work. Like
    /// [`plugin_states`](Self::plugin_states), the plugin is not given the state back when
    /// the document comes back with it.
    pub fn plugin_state(
        &mut self,
        device: DeviceId,
    ) -> Option<Result<daw_model::StateBlob, String>> {
        let l = self.plugins.live_mut(device)?;
        let state = l.changed_state()?;
        if let Ok(s) = &state {
            l.saved = Some(s.clone());
        }
        Some(state)
    }

    /// Saves every running plugin's state and returns those that differ
    /// from the document's, for the app to store (`DeviceField::State`)
    /// before saving the document (`Document::store_plugin_state`). Once
    /// stored and synced, they count as the document's, and are not loaded
    /// back into the plugin. Only devices of the last synced snapshot, and
    /// not those whose plugin refused the document's state: what such a
    /// plugin holds instead (its defaults) must not replace it.
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
            if own {
                continue;
            }
            // A state taken away (undone back to none) leaves the plugin as it is: there is
            // nothing to go back to, and nothing of the document's to protect.
            let Some(state) = d.state.clone() else {
                l.given = plugins::Given::Taken;
                continue;
            };
            if let Some(refusal) = self.give_plugin_state(d.id, state) {
                self.warnings.push(refusal);
            }
        }
    }

    /// Loads `state` into a device's plugin, returning the warning if the plugin refuses
    /// it. A plugin that cannot be asked now is offered it again by [`tick`](Self::tick).
    fn give_plugin_state(
        &mut self,
        device: DeviceId,
        state: daw_model::StateBlob,
    ) -> Option<Warning> {
        let l = self.plugins.live_mut(device)?;
        if let Err(e) = l.give(state) {
            return Some(Warning::DeviceStateRejected {
                device: l.label(),
                detail: e.to_string(),
            });
        }
        // The state set the plugin's parameters; the document's go on top again, as when
        // it loads, and again after a rebuild in case the load changed its ports and the
        // values went to a retired node.
        l.fresh = true;
        let key = build::device_key(device);
        if let Some(b) = &mut self.built {
            b.params.retain(|(k, _), _| *k != key);
            // The loaded state is what a lane now goes back to.
            b.restore.retain(|(k, _), _| *k != key);
            for (&(_, p), v) in b.rest.iter_mut().filter(|((k, _), _)| *k == key) {
                let now = l.handle.with(|i| i.param_value(p));
                if now.is_finite() {
                    *v = now;
                }
            }
        }
        None
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
        if !self.live.contains_key(&(key, param))
            && let Some(before) = self.plugin_param(key, param)
        {
            self.pre_live.entry((key, param)).or_insert(before);
        }
        self.live.insert((key, param), value);
        // Without room for it now, the next sync sends it.
        self.send_param(key, param, value)?;
        Ok(())
    }

    /// Sends one parameter value to the running graph and records it as
    /// sent. Returns false, leaving it for the next sync, when the node has
    /// had as many changes as the engine keeps for it between two cycles.
    fn send_param(&mut self, key: NodeKey, param: u32, value: f64) -> Result<bool, EngineError> {
        if !self.sent.room(&self.controller, key) {
            return Ok(false);
        }
        if let Err(e) = self.controller.set_param_by_key(key, param, value) {
            if matches!(e, EngineError::QueueFull) {
                self.sent.more_to_send(&self.controller);
            }
            return Err(e);
        }
        self.sent.record(&self.controller, key);
        if let Some(b) = &mut self.built {
            b.params.insert((key, param), value);
        }
        Ok(true)
    }

    /// What a plugin device's parameter is now, by node key: what the
    /// session last sent it, else what the plugin reports. `None` for other
    /// nodes.
    fn plugin_param(&self, key: NodeKey, param: u32) -> Option<f64> {
        let d = self
            .last
            .as_ref()?
            .all_tracks()
            .flat_map(|t| t.devices.iter())
            .find(|d| build::device_key(d.id) == key)?;
        if !matches!(d.source, daw_model::DeviceSource::Plugin { .. }) {
            return None;
        }
        let sent = self
            .built
            .as_ref()
            .and_then(|b| b.params.get(&(key, param)));
        sent.copied()
            .or_else(|| self.with_plugin(d.id, |p| p.param_value(param)))
            .filter(|v| v.is_finite())
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

    /// A lane as the engine plays it, values turned into the engine's unit
    /// by `to_value`. The engine only steps or ramps in straight lines, so
    /// curved segments are played as short straight pieces, and so is
    /// every ramp on a level (drawn in dB from
    /// [`FADER_FLOOR_DB`](level::FADER_FLOOR_DB) up, which the engine
    /// would otherwise ramp in linear gain). What plays then follows the
    /// line the arrange view draws.
    pub fn playback_lane(
        &self,
        lane: &daw_model::AutomationLane,
        to_value: fn(f64) -> f64,
    ) -> AutomationLane {
        let dense = lane.target.is_level();
        let floor = lane.target.ramp_floor();
        let mut out = Vec::with_capacity(lane.points.len());
        for (i, p) in lane.points.iter().enumerate() {
            let time = self.ticks_to_samples(p.tick);
            let next = lane.points.get(i + 1);
            let ramp = !p.step && next.is_some_and(|n| n.value != p.value);
            out.push(daw_engine::AutomationPoint {
                time,
                value: to_value(p.value),
                curve: if p.step { Curve::Step } else { Curve::Linear },
            });
            let Some(next) = next.filter(|_| ramp && (dense || p.curve != 0.0)) else {
                continue;
            };
            let span = self.ticks_to_samples(next.tick).0 - time.0;
            let pieces = (span / PIECE_SAMPLES).clamp(1, MAX_PIECES);
            for k in 1..pieces {
                let x = k as f64 / pieces as f64;
                let tick = TickTime(p.tick.0 + ((next.tick.0 - p.tick.0) as f64 * x) as i64);
                out.push(daw_engine::AutomationPoint {
                    time: self.ticks_to_samples(tick),
                    value: to_value(p.towards(next.value, x, floor)),
                    curve: Curve::Linear,
                });
            }
        }
        AutomationLane::new(out)
    }

    /// Sets the click. Takes effect at once, without a graph rebuild: the
    /// metronome is always in the graph and only its parameters change.
    /// When a parameter can't be sent (a full queue), the error says so and
    /// [`metronome`](Self::metronome) keeps the old settings, so calling
    /// again retries.
    pub fn set_metronome(&mut self, settings: MetronomeSettings) -> Result<(), SessionError> {
        if self.built.is_some() {
            for ((key, param), value) in build::metronome_params(&settings) {
                if self.last_sent(key, param) != Some(value) {
                    self.send_param(key, param, value)?;
                }
            }
        }
        self.metronome = settings;
        Ok(())
    }

    pub fn metronome(&self) -> MetronomeSettings {
        self.metronome
    }

    /// The value a node's parameter was last sent.
    fn last_sent(&self, key: NodeKey, param: u32) -> Option<f64> {
        self.built.as_ref()?.params.get(&(key, param)).copied()
    }

    fn send_params(&mut self, snapshot: &ProjectSnapshot) -> Result<(), SessionError> {
        self.sent.begin(&self.controller);
        let metronome = build::metronome_params(&self.metronome);
        for ((key, param), value) in build::params(snapshot).into_iter().chain(metronome) {
            let value = self.live.get(&(key, param)).copied().unwrap_or(value);
            if self.last_sent(key, param) != Some(value) {
                self.send_param(key, param, value)?;
            }
        }
        // Held values the document has no parameter for (a plugin's, during
        // touch write), should a rebuild have sent their node's again.
        let held: Vec<_> = self.live.iter().map(|(&k, &v)| (k, v)).collect();
        for ((key, param), value) in held {
            if self.last_sent(key, param) != Some(value) {
                match self.send_param(key, param, value) {
                    // Its device is gone.
                    Ok(_) | Err(EngineError::UnknownKey(_)) => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        // Only parameters that were sent another value: one the document
        // never set keeps what the device has, which may come from its
        // saved state.
        for ((key, param), value) in build::param_defaults(snapshot) {
            let value = self.live.get(&(key, param)).copied().unwrap_or(value);
            if self.last_sent(key, param).is_some_and(|v| v != value) {
                self.send_param(key, param, value)?;
            }
        }
        // A lane that stopped driving a parameter the document doesn't set
        // left its last value in the device: put back what it had before.
        // Kept until sent, so a full queue retries on the next sync.
        let built = self.built.as_ref().expect("built before params");
        let restore: Vec<_> = built.restore.iter().map(|(&k, &v)| (k, v)).collect();
        for ((key, param), value) in restore {
            if self.last_sent(key, param).is_none() {
                let value = self.live.get(&(key, param)).copied().unwrap_or(value);
                match self.send_param(key, param, value) {
                    Ok(true) => {}
                    // No room this time: still owed.
                    Ok(false) => continue,
                    // The device is gone; nothing to put back.
                    Err(EngineError::UnknownKey(_)) => {}
                    Err(e) => return Err(e.into()),
                }
            }
            if let Some(b) = &mut self.built {
                b.restore.remove(&(key, param));
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
            let readable = built.unread.get(&track.id).is_some_and(|files| {
                let mut files = files.iter();
                files.any(|(path, then)| media::stamp(path) != *then)
            });
            if !all && !readable && same_clips(built.clips.get(&track.id), &track.clips) {
                continue;
            }
            let set = build::clip_set(self, track, handle);
            build::note_unread(&mut built.unread, track.id, &set);
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

    /// Where the playhead should stay, in ticks, when the tempo changes. A
    /// stopped (or counting-in) playhead stays on its bar and beat. A
    /// playing one stays on its sample, so playback doesn't jump, unless it
    /// is inside the loop: the loop moves with the tempo and the playhead
    /// must stay inside it.
    fn kept_tick(&self) -> Option<TickTime> {
        if !self.playing || self.controller.is_counting_in() {
            // The audio thread may not have applied the last locate yet.
            return Some(self.parked.unwrap_or_else(|| self.position()));
        }
        let at = self.position();
        let (start, end) = self.loop_ticks?;
        (start <= at && at < end).then_some(at)
    }

    pub fn play(&mut self) -> Result<(), SessionError> {
        self.controller.play()?;
        self.relocate = None;
        self.playing = true;
        self.update_monitors();
        Ok(())
    }

    /// Plays after counting in `bars` bars of the time signature at the
    /// play position, with the metronome clicking them. The position holds
    /// until the count-in ends, and so does recording, which only captures
    /// while the transport moves. Already playing: carries on.
    pub fn play_with_count_in(&mut self, bars: u32) -> Result<(), SessionError> {
        self.controller.play_with_count_in(bars)?;
        self.relocate = None;
        self.playing = true;
        self.update_monitors();
        Ok(())
    }

    /// Whether the audio thread last reported a count-in under way.
    pub fn is_counting_in(&self) -> bool {
        self.controller.is_counting_in()
    }

    /// Stops the transport. Effects and instruments ring out as they
    /// would; [`silence`](Self::silence) cuts them off.
    pub fn stop(&mut self) -> Result<(), SessionError> {
        self.controller.stop()?;
        self.relocate = None;
        self.playing = false;
        // Wherever playback got to; the audio thread reports it.
        self.parked = None;
        self.update_monitors();
        Ok(())
    }

    /// Stops the transport and cuts off everything still sounding: every
    /// device is reset (reverb and delay tails, held voices, plugins
    /// through their own reset) and latency compensation is cleared.
    /// Players and recordings are left alone. LV2 has no reset a host may
    /// call while audio runs, so an LV2 plugin only gets all notes and
    /// sounds off.
    pub fn silence(&mut self) -> Result<(), SessionError> {
        self.stop()?;
        self.controller.silence()?;
        Ok(())
    }

    /// Whether the transport was last told to play.
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn locate(&mut self, t: TickTime) -> Result<(), SessionError> {
        let s = self.ticks_to_samples(t);
        self.controller.locate(s)?;
        // Where a tempo change still meant to put the playhead is no
        // longer where it belongs.
        self.relocate = None;
        self.parked = Some(t);
        Ok(())
    }

    /// The play position the audio thread last reported.
    pub fn position(&self) -> TickTime {
        self.samples_to_ticks(self.controller.position())
    }

    /// Bars and beats at the play position, as the engine gives them to
    /// the metronome and plugins.
    pub fn music(&self) -> daw_core::time::MusicalPosition {
        self.controller
            .timeline()
            .position(self.controller.position(), self.sample_rate())
    }

    /// Loops `[start, end)` while playing. `None` turns looping off, and
    /// so does a range the engine does not loop: empty, or shorter than
    /// [`MIN_LOOP_SECONDS`](daw_engine::MIN_LOOP_SECONDS) at the current
    /// tempo. Check [`loop_range`](Self::loop_range) to see which it was.
    pub fn set_loop(&mut self, range: Option<(TickTime, TickTime)>) -> Result<(), SessionError> {
        self.loop_ticks = range;
        self.apply_loop()
    }

    /// The loop being played, if any. A loop that a tempo change made too
    /// short to play is not reported until the tempo makes it playable
    /// again.
    pub fn loop_range(&self) -> Option<(TickTime, TickTime)> {
        self.loop_ticks.filter(|_| self.loop_points.get().is_some())
    }

    /// Sends the loop to the engine in samples at the current tempo, unless
    /// the engine would not play it.
    fn apply_loop(&mut self) -> Result<(), SessionError> {
        let range = self
            .loop_ticks
            .map(|(s, e)| LoopRange {
                start: self.ticks_to_samples(s),
                end: self.ticks_to_samples(e),
            })
            .filter(|r| r.is_playable(self.sample_rate()));
        self.loop_stale = true;
        // The players follow only once the transport does.
        self.controller.set_loop(range)?;
        self.loop_points.set(range.map(|r| (r.start, r.end)));
        self.loop_stale = false;
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

/// Linear strip gain for a stored level in dB: silent at
/// [`SILENCE_DB`](level::SILENCE_DB), and never above the strip's range.
pub fn strip_gain(db: f64) -> f64 {
    level::db_to_gain_above(db, level::SILENCE_DB)
        .min(f64::from(daw_engine::nodes::Strip::MAX_GAIN))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_gain_is_silent_at_the_stored_silence_and_stays_in_the_strips_range() {
        assert_eq!(strip_gain(level::SILENCE_DB), 0.0);
        assert_eq!(strip_gain(f64::NAN), 0.0);
        assert!(strip_gain(level::SILENCE_DB + 1.0) > 0.0);
        assert_eq!(strip_gain(0.0), 1.0);
        assert_eq!(strip_gain(20.0), 4.0);
    }
}
