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

mod build;
pub mod host;
mod media;
pub mod player;
mod tempo;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use daw_core::meter::PeakMeter;
use daw_core::time::{SampleTime, TempoMap, TickTime};
use daw_engine::{
    EngineConfig, EngineController, EngineError, EngineProcessor, LoopRange, NodeKey, NodeMeter,
};
use daw_model::{ClipId, DeviceId, ProjectSnapshot, TrackId};

pub use build::effective_mutes;
pub use media::{MEMORY_LIMIT_SECONDS, MediaPool};
use player::{LoopPoints, PlayerHandle};
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
    warnings: Vec<String>,
    /// The project's master track, as of the last sync.
    master: Option<TrackId>,
    /// What the transport was last told, so the UI doesn't wait a period
    /// for the audio thread to confirm.
    playing: bool,
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
    pub fn warnings(&self) -> &[String] {
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
            self.tempo_data = Some(snapshot.tempo.clone());
            self.controller.set_timeline(timeline)?;
            self.apply_loop()?;
        }
        let structure = build::Structure::of(snapshot);
        // Automation is in samples, so a tempo change re-times it.
        let rebuild = self.built.as_ref().is_none_or(|b| {
            b.structure != structure || (tempo_changed && structure.has_automation())
        });
        if rebuild {
            let old = self.built.take().unwrap_or_default();
            let (desc, built) = build::graph(self, snapshot, structure, old);
            self.built = Some(built);
            self.controller.set_graph(desc)?;
        }
        self.send_params(snapshot)?;
        self.send_clips(snapshot, tempo_changed);
        Ok(())
    }

    fn send_params(&mut self, snapshot: &ProjectSnapshot) -> Result<(), SessionError> {
        let built = self.built.as_mut().expect("built before params");
        for ((key, param), value) in build::params(snapshot) {
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
            let Some(handle) = built.players.get_mut(&track.id) else {
                continue;
            };
            let same = built.clips.get(&track.id).is_some_and(|old| {
                old.len() == track.clips.len()
                    && old.iter().zip(&track.clips).all(|(a, b)| Arc::ptr_eq(a, b))
            });
            if same && !all {
                continue;
            }
            let set = build::clip_set(self, track, handle);
            if handle.send(set).is_err() {
                self.warnings
                    .push(format!("clip update for track {} was dropped", track.name));
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
