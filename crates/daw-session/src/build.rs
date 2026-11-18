//! Turning a project snapshot into an engine graph, parameter values and
//! clip sets.
//!
//! Every node that stands for a model object is keyed by that object's id,
//! so a rebuilt graph reuses the running node (keeping plugin and synth
//! state, meters and players) whenever the object is still there.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use daw_core::node::{Node, PortLayout, ProcessContext};
use daw_core::time::{TickTime, engine_to_file_frames, file_to_engine_frames};
use daw_engine::mixer::SoloState;
use daw_engine::nodes::Strip;
use daw_engine::{GraphDesc, NodeId, NodeKey, NodeKind, NodeMeter};
use daw_model::{
    AutomationTarget, ClipContent, Device, DeviceId, DeviceSource, LaneId, Project,
    ProjectSnapshot, SendId, Track, TrackId, TrackKind, TrackOutput,
};

use daw_midi::model::compile_track;
use daw_midi::player::midi_player;

use crate::Warning;
use crate::capture::input_tap;
use crate::media::{Loaded, Seen};
use crate::player::{ClipSet, ClipSource, PLAYER_CHANNELS, PlayClip, PlayerHandle, player};
use crate::{Built, MetronomeSettings, Session, db_to_gain};

/// Every channel strip in the graph is stereo.
const STRIP_CHANNELS: u16 = 2;

/// The key of a track's strip. The master track's strip is the engine's
/// master.
pub(crate) fn strip_key(project: &Project, track: TrackId) -> NodeKey {
    if track == project.master.id {
        NodeKey::MASTER
    } else {
        NodeKey(track.0.as_u128())
    }
}

/// The key of an audio track's clip player. Ids are random UUIDs, so
/// flipping a bit can't collide with another object's id in practice.
fn player_key(track: TrackId) -> NodeKey {
    NodeKey(track.0.as_u128() ^ 1)
}

/// The key of an instrument track's MIDI player.
fn midi_key(track: TrackId) -> NodeKey {
    NodeKey(track.0.as_u128() ^ 2)
}

/// The key of the metronome. Model ids are random (version 4) UUIDs, which
/// never have every bit set but the last.
pub const METRONOME_KEY: NodeKey = NodeKey(u128::MAX - 1);

/// The metronome's parameters for `settings`.
pub(crate) fn metronome_params(settings: &MetronomeSettings) -> [((NodeKey, u32), f64); 2] {
    use daw_devices::devices::metronome::{PLAYBACK, VOLUME};
    [
        (
            (METRONOME_KEY, PLAYBACK),
            if settings.enabled { 1.0 } else { 0.0 },
        ),
        ((METRONOME_KEY, VOLUME), settings.volume_db),
    ]
}

/// The key of an audio track's input tap.
fn tap_key(track: TrackId) -> NodeKey {
    NodeKey(track.0.as_u128() ^ 3)
}

/// The key of a track's pre-fader stage: the point in front of its strip
/// that its pre-fader sends tap.
fn pre_fader_key(track: TrackId) -> NodeKey {
    NodeKey(track.0.as_u128() ^ 4)
}

/// Whether a built-in device kind is the one whose state names sample files.
fn is_sampler(kind: &str) -> bool {
    kind == daw_devices::devices::sampler::INFO.id
}

/// How many captured input events can wait for the recorder.
const MIDI_CAPTURE: usize = 4096;

pub(crate) fn device_key(device: DeviceId) -> NodeKey {
    NodeKey(device.0.as_u128())
}

pub(crate) fn send_key(send: SendId) -> NodeKey {
    NodeKey(send.0.as_u128())
}

/// What decides the shape of the graph. When this is unchanged between two
/// snapshots, the running graph can be updated in place.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Structure {
    tracks: Vec<TrackShape>,
}

#[derive(Clone, Debug, PartialEq)]
struct TrackShape {
    id: TrackId,
    kind: TrackKind,
    output: TrackOutput,
    devices: Vec<(DeviceId, DeviceSource, bool)>,
    sends: Vec<(SendId, TrackId, bool)>,
    /// Automation is compiled into the graph, so a lane edit rebuilds it.
    lanes: Vec<daw_model::AutomationLane>,
    /// Where an instrument track's live MIDI comes from.
    live: LiveInput,
}

/// Which external event sources reach an instrument track's MIDI player.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum LiveInput {
    /// Not an instrument track.
    None,
    /// Every source.
    All,
    /// One source, by engine source index.
    Source(usize),
    /// The chosen device is not connected.
    Missing,
}

impl Structure {
    /// The shape of `project`, leaving out the automation lanes in
    /// `suspended`. `sources` maps MIDI source port names to engine event
    /// sources.
    pub fn of(
        project: &Project,
        suspended: &HashSet<LaneId>,
        sources: &HashMap<String, usize>,
    ) -> Self {
        Self {
            tracks: project
                .all_tracks()
                .map(|t| TrackShape {
                    id: t.id,
                    kind: t.kind,
                    output: t.output,
                    devices: t
                        .devices
                        .iter()
                        .map(|d| (d.id, d.source.clone(), d.enabled))
                        .collect(),
                    sends: t
                        .sends
                        .iter()
                        .map(|s| (s.id, s.target, s.pre_fader))
                        .collect(),
                    lanes: t
                        .automation
                        .iter()
                        .filter(|l| !suspended.contains(&l.id))
                        .cloned()
                        .collect(),
                    live: match (&t.kind, &t.midi_input.device) {
                        (TrackKind::Instrument, None) => LiveInput::All,
                        (TrackKind::Instrument, Some(d)) => sources
                            .get(d)
                            .map_or(LiveInput::Missing, |&i| LiveInput::Source(i)),
                        _ => LiveInput::None,
                    },
                })
                .collect(),
        }
    }

    fn device_source(&self, id: DeviceId) -> Option<&DeviceSource> {
        self.tracks
            .iter()
            .flat_map(|t| t.devices.iter())
            .find(|d| d.0 == id)
            .map(|d| &d.1)
    }

    pub(crate) fn live_input(&self, track: TrackId) -> LiveInput {
        self.tracks
            .iter()
            .find(|t| t.id == track)
            .map_or(LiveInput::None, |t| t.live)
    }

    pub fn has_automation(&self) -> bool {
        self.tracks.iter().any(|t| !t.lanes.is_empty())
    }

    pub fn has_sampler(&self) -> bool {
        let mut devices = self.tracks.iter().flat_map(|t| t.devices.iter());
        devices.any(|d| matches!(&d.1, DeviceSource::Builtin { kind } if is_sampler(kind)))
    }
}

/// Builds the whole graph for `snapshot`, reusing what `old` had running.
/// New players are sent their clips; reused ones keep theirs (the caller
/// sends changes afterwards).
pub(crate) fn graph(
    session: &mut Session,
    snapshot: &ProjectSnapshot,
    structure: Structure,
    old: Built,
) -> (GraphDesc, Build) {
    let mut b = Builder::new(session, snapshot, structure, old);
    for track in snapshot.all_tracks() {
        b.track(track);
    }
    b.outputs();
    b.sends();
    b.metronome();
    b.automate();
    b.stopped_lanes();
    let build = Build {
        built: b.built,
        old: b.old,
        taken: b.taken,
    };
    (b.g, build)
}

/// A graph's bookkeeping, until the engine has the graph.
pub(crate) struct Build {
    /// What the session keeps once the engine took the graph.
    pub built: Built,
    /// What the running graph was built from, less what `built` took over.
    old: Built,
    taken: Taken,
}

/// What a build took over from the running graph's bookkeeping, or changed
/// outside its own: all of it goes back if the engine refuses the graph.
#[derive(Default)]
struct Taken {
    players: Vec<TrackId>,
    midi: Vec<TrackId>,
    taps: Vec<TrackId>,
    /// Plugins this build marked as no longer fresh.
    fresh: Vec<DeviceId>,
}

impl Build {
    /// The engine refused the graph: gives back the running graph's
    /// bookkeeping as it was, handles included, so the next build reuses
    /// the running nodes like this one did.
    pub fn revert(self, plugins: &mut crate::Plugins) -> Built {
        let Self {
            mut built,
            mut old,
            taken,
        } = self;
        fn back<K: Copy + Eq + std::hash::Hash, V>(
            keys: &[K],
            from: &mut HashMap<K, V>,
            to: &mut HashMap<K, V>,
        ) {
            for k in keys {
                if let Some(v) = from.remove(k) {
                    to.insert(*k, v);
                }
            }
        }
        back(&taken.players, &mut built.players, &mut old.players);
        back(&taken.players, &mut built.clips, &mut old.clips);
        back(&taken.midi, &mut built.midi, &mut old.midi);
        back(&taken.midi, &mut built.midi_clips, &mut old.midi_clips);
        back(&taken.taps, &mut built.taps, &mut old.taps);
        for device in taken.fresh {
            if let Some(live) = plugins.live_mut(device) {
                live.fresh = true;
            }
        }
        old
    }
}

/// Where a track's nodes are in the graph being built.
struct Chain {
    strip: NodeId,
    /// Where audio routed into the track (from outputs and sends) enters.
    head: NodeId,
    /// What the track's pre-fader sends tap, if it has any.
    pre_fader: Option<NodeId>,
}

/// A track's signal on its way down the chain, while the chain is built.
#[derive(Default)]
struct Signal {
    /// The nodes whose outputs, summed, are the signal so far.
    sources: Vec<NodeId>,
    /// The first node the signal went into: where audio routed into the
    /// track joins it.
    head: Option<NodeId>,
    /// Whether other tracks can be routed into this one (a bus or the
    /// master), so that it needs a `head` that passes audio on.
    takes_routes: bool,
}

/// A graph being built for a snapshot, one pass at a time: each track's
/// chain, then the routing between tracks, then automation.
struct Builder<'a> {
    session: &'a mut Session,
    project: &'a Project,
    g: GraphDesc,
    built: Built,
    old: Built,
    /// What was last sent to each node of the running graph.
    old_params: HashMap<NodeKey, Vec<(u32, f64)>>,
    muted: HashSet<TrackId>,
    /// The keyed nodes added so far.
    nodes: HashMap<NodeKey, NodeId>,
    devices: HashMap<NodeKey, &'a Device>,
    chains: HashMap<TrackId, Chain>,
    routes: Routes,
    taken: Taken,
}

impl<'a> Builder<'a> {
    fn new(
        session: &'a mut Session,
        project: &'a Project,
        structure: Structure,
        old: Built,
    ) -> Self {
        let (inputs, outputs) = (session.config.inputs, session.config.outputs);
        let g = session.controller.graph(inputs, outputs);
        let mut old_params: HashMap<NodeKey, Vec<(u32, f64)>> = HashMap::new();
        for (&(key, param), &value) in &old.params {
            old_params.entry(key).or_default().push((param, value));
        }
        Self {
            nodes: HashMap::from([(NodeKey::MASTER, g.master())]),
            g,
            built: Built {
                structure,
                ..Default::default()
            },
            old,
            old_params,
            muted: effective_mutes(project),
            devices: project
                .all_tracks()
                .flat_map(|t| t.devices.iter())
                .map(|d| (device_key(d.id), d))
                .collect(),
            chains: HashMap::new(),
            routes: Routes::default(),
            taken: Taken::default(),
            session,
            project,
        }
    }

    /// Takes the running node with `key` into this graph, if there is one.
    fn reuse(&mut self, key: NodeKey) -> Option<NodeId> {
        let id = self.g.reuse(key)?;
        self.nodes.insert(key, id);
        Some(id)
    }

    /// Like [`reuse`](Self::reuse), for a node that is sent only what
    /// changed: what the running node was last sent counts as sent.
    fn reuse_with_params(&mut self, key: NodeKey) -> Option<NodeId> {
        let id = self.reuse(key)?;
        self.carry_params(key);
        Some(id)
    }

    fn add(
        &mut self,
        kind: NodeKind,
        name: impl Into<String>,
        node: Box<dyn Node>,
        key: NodeKey,
    ) -> NodeId {
        let id = self.g.add_node(kind, name, node);
        self.g.set_key(id, key);
        self.nodes.insert(key, id);
        id
    }

    /// Copies what was last sent to a node of the running graph, so only
    /// real changes are sent after the swap.
    fn carry_params(&mut self, key: NodeKey) {
        for &(param, value) in self.old_params.get(&key).into_iter().flatten() {
            self.built.params.insert((key, param), value);
        }
    }

    /// A new strip with the values `params` gives it, which count as sent.
    fn new_strip(&mut self, mut strip: Strip, params: &[((NodeKey, u32), f64)]) -> Box<dyn Node> {
        for &((key, param), value) in params {
            match param {
                Strip::GAIN => strip.set_gain(value as f32),
                Strip::PAN => strip.set_pan(value as f32),
                Strip::MUTE => strip.set_muted(value != 0.0),
                _ => unreachable!("strips have no parameter {param}"),
            }
            self.built.params.insert((key, param), value);
        }
        Box::new(strip)
    }

    /// One track's chain: [player, input] -> devices -> [pre-fader stage]
    /// -> strip.
    fn track(&mut self, track: &'a Track) {
        let strip = self.strip(track);
        let mut signal = Signal {
            takes_routes: matches!(track.kind, TrackKind::Bus | TrackKind::Master),
            ..Signal::default()
        };
        if track.kind == TrackKind::Audio {
            signal.sources.push(self.clip_player(track));
            // Monitored input goes through the track's devices like its
            // clips do.
            if self.session.config.inputs > 0 {
                signal.sources.push(self.input_tap(track));
            }
        }
        // An instrument track's MIDI player feeds a device events, not
        // audio, so it stays out of the audio chain.
        let midi = (track.kind == TrackKind::Instrument).then(|| self.midi_player(track));
        let mut takes_notes = None;
        for d in &track.devices {
            let id = self.device(d);
            if takes_notes.is_none() && self.ports(id).event_in {
                takes_notes = Some(id);
            }
            // A switched-off device stays in the graph, so it keeps its
            // state, but audio goes around it.
            if d.enabled {
                self.pass_through(&mut signal, id, &d.name);
            }
        }
        // Notes go to the first device that takes events (one that takes
        // none, a fixed utility effect, would have the engine refuse the
        // graph), even when it is switched off: switching the instrument
        // off silences it, and it still gets the note-offs of notes that
        // were sounding.
        match (midi, takes_notes) {
            (Some(player), Some(device)) => self.g.connect_events(player, device),
            (Some(_), None) if !track.devices.is_empty() => {
                self.session.warnings.push(Warning::NotesUnplayed {
                    track: track.name.clone(),
                });
            }
            _ => {}
        }
        let pre_fader = track.sends.iter().any(|s| s.pre_fader).then(|| {
            let stage = self.pre_fader_stage(track);
            self.pass_through(&mut signal, stage, &track.name);
            stage
        });
        for &source in &signal.sources {
            self.link(source, strip);
        }
        self.chains.insert(
            track.id,
            Chain {
                strip,
                // A track with nothing before its strip takes routed audio
                // straight into it.
                head: signal.head.unwrap_or(strip),
                pre_fader,
            },
        );
    }

    fn ports(&self, id: NodeId) -> PortLayout {
        self.g.ports(id).unwrap_or_default()
    }

    /// Sends `signal` through `node`, whatever its channel counts. A node
    /// without audio inputs (an instrument) adds its sound to the signal
    /// instead, and one without outputs (an analyser) only listens.
    fn pass_through(&mut self, signal: &mut Signal, node: NodeId, name: &str) {
        let ports = self.ports(node);
        if ports.audio_in > 0 {
            // A listener can't be where routed audio joins the track, as
            // that audio would end in it: it joins at a stage in front.
            if ports.audio_out == 0 && signal.head.is_none() && signal.takes_routes {
                let stage = Box::new(Strip::send(STRIP_CHANNELS, 1.0));
                let stage = self.g.add_device(format!("{name} input"), stage);
                signal.head = Some(stage);
                signal.sources.push(stage);
            }
            let entry = self.stereo_entry(node, name);
            for &source in &signal.sources {
                self.link(source, entry);
            }
            signal.head.get_or_insert(entry);
        }
        if ports.audio_out > 0 {
            if ports.audio_in > 0 {
                signal.sources.clear();
            }
            signal.sources.push(node);
        }
    }

    /// Where a stereo signal goes into `node`: the node itself, or for a
    /// node with one input a stage in front of it that mixes both channels
    /// into that one, so a mono effect does not drop the right channel.
    fn stereo_entry(&mut self, node: NodeId, name: &str) -> NodeId {
        if self.ports(node).audio_in != 1 {
            return node;
        }
        let name = format!("{name} mono mix");
        let mix = self.g.add_device(name, Box::new(MonoMix));
        self.g.connect_audio(mix, 0, node, 0);
        mix
    }

    /// Connects the first two channels of `from` to those of `to`. A mono
    /// `from` goes to both, so it plays in the middle and not on the left.
    fn link(&mut self, from: NodeId, to: NodeId) {
        let outs = self.ports(from).audio_out;
        let ins = self.ports(to).audio_in.min(STRIP_CHANNELS);
        if outs == 0 {
            return;
        }
        for ch in 0..ins {
            self.g.connect_audio(from, ch.min(outs - 1), to, ch);
        }
    }

    fn strip(&mut self, track: &Track) -> NodeId {
        let key = strip_key(self.project, track.id);
        if track.kind == TrackKind::Master {
            let id = self.g.master();
            if self.g.meter(id).is_none() {
                let channels = self.session.config.outputs as usize;
                self.g.set_meter(id, Arc::new(NodeMeter::new(channels)));
            }
            // The engine creates the master at unity gain and the old one
            // may have any; send the model's value after the swap.
            self.carry_params(key);
            return id;
        }
        if let Some(id) = self.reuse_with_params(key) {
            self.g.rename(id, &track.name);
            return id;
        }
        let kind = if track.kind == TrackKind::Bus {
            NodeKind::Bus
        } else {
            NodeKind::Track
        };
        let params = strip_params(key, track, &self.muted);
        let strip = self.new_strip(Strip::track(STRIP_CHANNELS), &params);
        let id = self.add(kind, &track.name, strip, key);
        self.g
            .set_meter(id, Arc::new(NodeMeter::new(STRIP_CHANNELS as usize)));
        id
    }

    /// An audio track's clip player.
    fn clip_player(&mut self, track: &Track) -> NodeId {
        let key = player_key(track.id);
        if self.old.players.contains_key(&track.id)
            && let Some(id) = self.reuse(key)
        {
            let handle = self.old.players.remove(&track.id).expect("checked");
            self.taken.players.push(track.id);
            self.built.players.insert(track.id, handle);
            if let Some(files) = self.old.unread.get(&track.id) {
                self.built.unread.insert(track.id, files.clone());
            }
            if let Some(c) = self.old.clips.remove(&track.id) {
                self.built.clips.insert(track.id, c);
            }
            return id;
        }
        let (node, mut handle) = player(
            self.session.loop_points.clone(),
            self.session.offline.clone(),
        );
        let set = clip_set(self.session, track, &handle);
        note_unread(&mut self.built.unread, track.id, &set);
        // A fresh queue has room for the first set.
        let _ = handle.send(set);
        self.built.clips.insert(track.id, track.clips.clone());
        self.built.players.insert(track.id, handle);
        let name = format!("{} clips", track.name);
        self.add(NodeKind::Device, name, Box::new(node), key)
    }

    /// An audio track's input tap, fed every hardware input.
    fn input_tap(&mut self, track: &Track) -> NodeId {
        let key = tap_key(track.id);
        let inputs = self.session.config.inputs;
        let reused = self
            .old
            .taps
            .contains_key(&track.id)
            .then(|| self.reuse(key));
        let reused = reused.flatten().map(|id| {
            self.taken.taps.push(track.id);
            (id, self.old.taps.remove(&track.id).expect("checked"))
        });
        let (id, handle) = reused.unwrap_or_else(|| {
            let (node, handle) = input_tap(inputs);
            let name = format!("{} input", track.name);
            (
                self.add(NodeKind::Device, name, Box::new(node), key),
                handle,
            )
        });
        self.built.taps.insert(track.id, handle);
        for ch in 0..inputs {
            self.g.connect_audio(self.g.input(), ch, id, ch);
        }
        id
    }

    /// An instrument track's MIDI player, with the live MIDI it passes on
    /// (what its input gate lets through, see `Session::apply_gates`).
    fn midi_player(&mut self, track: &Track) -> NodeId {
        let key = midi_key(track.id);
        let reused = self
            .old
            .midi
            .contains_key(&track.id)
            .then(|| self.reuse(key));
        let reused = reused.flatten().map(|id| {
            self.taken.midi.push(track.id);
            (id, self.old.midi.remove(&track.id).expect("checked"))
        });
        let id = match reused {
            Some((id, handle)) => {
                self.built.midi.insert(track.id, handle);
                if let Some(c) = self.old.midi_clips.remove(&track.id) {
                    self.built.midi_clips.insert(track.id, c);
                }
                id
            }
            None => {
                let (node, mut handle) = midi_player(MIDI_CAPTURE);
                let rate = self.session.sample_rate();
                let seq = compile_track(track, &self.session.tempo, rate);
                // A fresh queue has room for the first sequence.
                let _ = handle.set_sequence(seq);
                self.built.midi_clips.insert(track.id, track.clips.clone());
                self.built.midi.insert(track.id, handle);
                let name = format!("{} notes", track.name);
                self.add(NodeKind::Device, name, Box::new(node), key)
            }
        };
        match self.built.structure.live_input(track.id) {
            LiveInput::All => self.g.route_external_events(id),
            LiveInput::Source(i) => self.g.route_external_events_from(id, i),
            LiveInput::None | LiveInput::Missing => {}
        }
        id
    }

    fn device(&mut self, d: &Device) -> NodeId {
        let key = device_key(d.id);
        let id = match &d.source {
            // A plugin keeps its state in its handle; every graph gets a
            // fresh node of it, with its current ports and latency.
            DeviceSource::Plugin { .. } => self.plugin(d),
            DeviceSource::Builtin { kind } => {
                let same_source = self.old.structure.device_source(d.id) == Some(&d.source);
                // A sampler finds relative sample paths in the project
                // bundle: when that moved, it loads its state again.
                let keep = same_source && !(self.session.root_moved && is_sampler(kind));
                match keep.then(|| self.reuse_with_params(key)).flatten() {
                    Some(id) => {
                        self.g.rename(id, &d.name);
                        if let Some(m) = self.old.device_meters.get(&d.id) {
                            self.built.device_meters.insert(d.id, m.clone());
                        }
                        id
                    }
                    None => self.builtin(d, kind),
                }
            }
        };
        // Silencing (`Session::silence`) cuts its tail or held voices.
        self.g.set_tail(id);
        id
    }

    /// A new node for a built-in device, or a bypass (and a warning) when
    /// this build has no such device.
    fn builtin(&mut self, d: &Device, kind: &str) -> NodeId {
        let key = device_key(d.id);
        let Some(mut dev) = daw_devices::create(kind) else {
            self.session.warnings.push(Warning::UnknownDevice {
                kind: kind.to_owned(),
            });
            // The saved values count as sent: a bypass has no use for them.
            self.built
                .params
                .extend(d.params.iter().map(|(&p, &v)| ((key, p), v)));
            return self.add(NodeKind::Device, &d.name, Box::new(Bypass), key);
        };
        // Samplers find relative sample paths in the project bundle.
        if let Some(state) = &d.state
            && let Err(e) = load_device_state(
                dev.as_mut(),
                &state.0,
                self.session.media.root(),
                &mut self.session.warnings,
            )
        {
            self.session.warnings.push(Warning::DeviceStateRejected {
                device: d.name.clone(),
                detail: e.to_string(),
            });
        }
        for (&p, &v) in &d.params {
            dev.set_param(p, v);
            self.built.params.insert((key, p), v);
        }
        if let Some(m) = dev.gain_reduction_meter() {
            self.built.device_meters.insert(d.id, m);
        }
        self.add(NodeKind::Device, &d.name, dev, key)
    }

    /// The node of a plugin device: a fresh node of its loaded plugin, or
    /// a bypass when it can't play.
    fn plugin(&mut self, d: &Device) -> NodeId {
        let key = device_key(d.id);
        let session = &mut *self.session;
        let (sample_rate, max_frames) = (session.sample_rate(), session.config.engine.max_frames);
        let live = session.plugins.ensure(d, sample_rate, max_frames);
        let node: Box<dyn Node> = match live {
            Some(live) => {
                // A fresh plugin has had none of the document's values:
                // the session sends them all once the graph is in.
                let fresh = std::mem::take(&mut live.fresh);
                let node = live.handle.node(sample_rate, max_frames);
                if fresh {
                    self.taken.fresh.push(d.id);
                } else if !session.sent.maybe_unplayed(&session.controller, key) {
                    // A value sent just before this graph may never reach the
                    // plugin (its node is new in every graph, and the engine
                    // hands the old node's waiting changes to no one), so
                    // such a node's values are all sent again.
                    self.carry_params(key);
                }
                Box::new(node)
            }
            None => Box::new(Bypass),
        };
        self.add(NodeKind::Device, &d.name, node, key)
    }

    /// The stage a track's pre-fader sends tap: everything the strip gets,
    /// before the fader. It is silent while the track is (muted, or not
    /// part of a solo), so the sends are too.
    fn pre_fader_stage(&mut self, track: &Track) -> NodeId {
        let key = pre_fader_key(track.id);
        self.reuse_with_params(key).unwrap_or_else(|| {
            let params = pre_fader_params(track, &self.muted);
            let stage = self.new_strip(Strip::track(STRIP_CHANNELS), &params);
            self.add(
                NodeKind::Send,
                format!("{} pre-fader", track.name),
                stage,
                key,
            )
        })
    }

    /// Connects each track's strip to where its output goes.
    fn outputs(&mut self) {
        for track in self.project.tracks.iter() {
            let target = match track.output {
                TrackOutput::Master => self.project.master.id,
                TrackOutput::Bus(b) => b,
            };
            let Some(head) = self.chains.get(&target).map(|c| c.head) else {
                continue;
            };
            if self.route(track, target) {
                self.g.connect(self.chains[&track.id].strip, head);
            }
        }
    }

    /// Adds every send's node and connects it.
    fn sends(&mut self) {
        for track in self.project.all_tracks() {
            let chain = &self.chains[&track.id];
            let (strip, pre_fader) = (chain.strip, chain.pre_fader);
            for s in &track.sends {
                // Every send gets its node, connected or not, so its level
                // and automation always have somewhere to go.
                let key = send_key(s.id);
                let node = self.reuse_with_params(key).unwrap_or_else(|| {
                    let stage = self.new_strip(Strip::send(STRIP_CHANNELS, 1.0), &[send_param(s)]);
                    self.add(NodeKind::Send, "send", stage, key)
                });
                let Some(head) = self.chains.get(&s.target).map(|c| c.head) else {
                    continue;
                };
                let tap = match pre_fader {
                    Some(stage) if s.pre_fader => stage,
                    _ => strip,
                };
                if self.route(track, s.target) {
                    self.g.connect(tap, node);
                    self.g.connect(node, head);
                }
            }
        }
    }

    /// Whether `track` may feed `target`. Commands and loading never let
    /// routing loop, but the engine would refuse the whole graph if it did,
    /// so a route that would close a loop is left out and reported instead.
    fn route(&mut self, track: &Track, target: TrackId) -> bool {
        let ok = self.routes.add(track.id, target);
        if !ok {
            self.session.warnings.push(Warning::RoutingLoop {
                track: track.name.clone(),
            });
        }
        ok
    }

    /// The click goes through the master, so its meter shows it, and stays
    /// in every graph so turning it on and off needs no rebuild.
    fn metronome(&mut self) {
        let click = self.reuse_with_params(METRONOME_KEY).unwrap_or_else(|| {
            let mut node = daw_devices::Metronome::new();
            for ((key, param), value) in metronome_params(&self.session.metronome) {
                daw_devices::Device::set_param(&mut node, param, value);
                self.built.params.insert((key, param), value);
            }
            self.add(NodeKind::Device, "metronome", Box::new(node), METRONOME_KEY)
        });
        self.g.connect(click, self.g.master());
    }

    /// Adds the model's automation lanes to the graph, in engine samples.
    fn automate(&mut self) {
        for t in self.project.all_tracks() {
            for lane in t
                .automation
                .iter()
                .filter(|l| l.enabled && !l.points.is_empty())
            {
                let strip = strip_key(self.project, t.id);
                let (key, param, to_value): (NodeKey, u32, fn(f64) -> f64) = match lane.target {
                    AutomationTarget::Volume => (strip, Strip::GAIN, db_to_gain),
                    AutomationTarget::Pan => (strip, Strip::PAN, |v| v),
                    AutomationTarget::SendLevel { send } => {
                        (send_key(send), Strip::GAIN, db_to_gain)
                    }
                    AutomationTarget::DeviceParam { device, param } => {
                        (device_key(device), param, |v| v)
                    }
                };
                let Some(&node) = self.nodes.get(&key) else {
                    continue;
                };
                // Nothing to drive, and the engine would refuse the graph:
                // only a damaged project has a lane on a device without
                // parameters.
                if !self.ports(node).event_in {
                    self.session.warnings.push(Warning::LaneSkipped {
                        device: self.g.name(node).unwrap_or_default().to_owned(),
                    });
                    continue;
                }
                if self.session.suspended.contains(&lane.id) {
                    // Overwritten by hand for now: the value from before
                    // the lane still is what removing the lane goes back to.
                    if let Some(&v) = self.old.rest.get(&(key, param)) {
                        self.built.rest.insert((key, param), v);
                    }
                    continue;
                }
                self.built.automated.insert((key, param));
                if let Some(rest) = self.rest_value(key, param) {
                    self.built.rest.insert((key, param), rest);
                }
                let lane = self.session.playback_lane(lane, to_value);
                self.g.automate(node, param, lane);
            }
        }
    }

    /// What the device parameter `param` of the device node `key` goes back
    /// to when its lane stops, if the document has no value for it (with
    /// one, the document's value is sent again). `None` for other nodes.
    ///
    /// A built-in device goes back to its default, as when the document's
    /// value is reset. A plugin goes back to what it had before the lane
    /// took over: the value taken when the lane started (or still waiting
    /// to be put back), else what it had before a take held it by hand,
    /// else the value last sent to it, else what it reports now (a lane
    /// that plays from the start).
    fn rest_value(&self, key: NodeKey, param: u32) -> Option<f64> {
        let d = self.devices.get(&key)?;
        if d.params.contains_key(&param) {
            return None;
        }
        let (old, session) = (&self.old, &*self.session);
        match &d.source {
            DeviceSource::Builtin { kind } => daw_devices::param_specs(kind)?
                .iter()
                .find(|s| s.id == param)
                .map(|s| s.default),
            DeviceSource::Plugin { .. } => old
                .rest
                .get(&(key, param))
                .or(old.restore.get(&(key, param)))
                .or(session.pre_live.get(&(key, param)))
                .or(old.params.get(&(key, param)))
                .copied()
                .or_else(|| session.with_plugin(d.id, |p| p.param_value(param)))
                .filter(|v| v.is_finite()),
        }
    }

    /// Queues what goes back into parameters whose lane no longer plays.
    fn stopped_lanes(&mut self) {
        // A lane that stopped playing (switched off, removed, emptied) left
        // its last value behind: send the control's own value again, or the
        // one it had before the lane, when the document has none.
        let stopped: Vec<(NodeKey, u32)> = self
            .old
            .automated
            .difference(&self.built.automated)
            .copied()
            .collect();
        for (key, param) in stopped {
            self.built.params.remove(&(key, param));
            if let Some(v) = self.rest_value(key, param) {
                self.built.restore.insert((key, param), v);
            }
        }
        // Not sent yet (a full queue), not taken over by a lane again, and
        // the device still there.
        for (k, &v) in &self.old.restore {
            if !self.built.automated.contains(k) && self.nodes.contains_key(&k.0) {
                self.built.restore.entry(*k).or_insert(v);
            }
        }
    }
}

/// The routes between tracks added to a graph so far.
#[derive(Default)]
struct Routes(HashMap<TrackId, Vec<TrackId>>);

impl Routes {
    /// Adds a route from `from` to `to` unless it would close a loop.
    fn add(&mut self, from: TrackId, to: TrackId) -> bool {
        let mut seen = HashSet::new();
        let mut stack = vec![to];
        while let Some(t) = stack.pop() {
            if t == from {
                return false;
            }
            if seen.insert(t) {
                stack.extend(self.0.get(&t).into_iter().flatten().copied());
            }
        }
        self.0.entry(from).or_default().push(to);
        true
    }
}

/// What a track's strip should be set to. The master has neither pan nor
/// mute.
fn strip_params(key: NodeKey, t: &Track, muted: &HashSet<TrackId>) -> Vec<((NodeKey, u32), f64)> {
    let mut out = vec![((key, Strip::GAIN), db_to_gain(t.volume_db))];
    if t.kind != TrackKind::Master {
        out.push(((key, Strip::PAN), t.pan.clamp(-1.0, 1.0)));
        let m = if muted.contains(&t.id) { 1.0 } else { 0.0 };
        out.push(((key, Strip::MUTE), m));
    }
    out
}

/// What a track's pre-fader stage should be set to: nothing for a track
/// without pre-fader sends, which has none.
fn pre_fader_params(t: &Track, muted: &HashSet<TrackId>) -> Vec<((NodeKey, u32), f64)> {
    if !t.sends.iter().any(|s| s.pre_fader) {
        return Vec::new();
    }
    let m = if muted.contains(&t.id) { 1.0 } else { 0.0 };
    vec![((pre_fader_key(t.id), Strip::MUTE), m)]
}

/// What a send's node should be set to.
fn send_param(s: &daw_model::Send) -> ((NodeKey, u32), f64) {
    ((send_key(s.id), Strip::GAIN), db_to_gain(s.level_db))
}

/// The parameter value every strip, send and device should have.
pub(crate) fn params(project: &Project) -> Vec<((NodeKey, u32), f64)> {
    let muted = effective_mutes(project);
    let mut out = Vec::new();
    for t in project.all_tracks() {
        out.extend(strip_params(strip_key(project, t.id), t, &muted));
        out.extend(pre_fader_params(t, &muted));
        out.extend(t.sends.iter().map(send_param));
        for d in &t.devices {
            for (&p, &v) in &d.params {
                out.push(((device_key(d.id), p), v));
            }
        }
    }
    out
}

/// The default of every built-in device parameter the project leaves at
/// its default. A parameter returned to its default (`SetParam` with
/// `None`) is no longer in [`params`], so the session sends it this.
pub(crate) fn param_defaults(project: &Project) -> Vec<((NodeKey, u32), f64)> {
    let mut out = Vec::new();
    for d in project.all_tracks().flat_map(|t| t.devices.iter()) {
        let DeviceSource::Builtin { kind } = &d.source else {
            continue;
        };
        let Some(specs) = daw_devices::param_specs(kind) else {
            continue;
        };
        for s in specs.iter().filter(|s| !d.params.contains_key(&s.id)) {
            out.push(((device_key(d.id), s.id), s.default));
        }
    }
    out
}

/// Tracks that are silent: muted ones, and while anything is soloed, every
/// track that neither is soloed nor carries a soloed track's signal. Uses
/// the mixer's rule (`daw_engine::mixer::effective_mutes`).
pub fn effective_mutes(project: &Project) -> HashSet<TrackId> {
    let tracks: Vec<&Arc<Track>> = project.all_tracks().collect();
    let index: HashMap<TrackId, usize> =
        tracks.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
    let states: Vec<SoloState> = tracks
        .iter()
        .map(|t| SoloState {
            muted: t.mute && t.kind != TrackKind::Master,
            soloed: t.solo,
            // The master carries everything.
            solo_safe: t.kind == TrackKind::Master,
        })
        .collect();
    let mut feeds = Vec::new();
    for t in &project.tracks {
        let out = match t.output {
            TrackOutput::Master => project.master.id,
            TrackOutput::Bus(b) => b,
        };
        for target in std::iter::once(out).chain(t.sends.iter().map(|s| s.target)) {
            if let (Some(&a), Some(&b)) = (index.get(&t.id), index.get(&target)) {
                feeds.push((a, b));
            }
        }
    }
    daw_engine::mixer::effective_mutes(&states, &feeds)
        .into_iter()
        .zip(&tracks)
        .filter(|(m, _)| *m)
        .map(|(_, t)| t.id)
        .collect()
}

/// The clips of an audio track as a player plays them.
pub(crate) fn clip_set(session: &mut Session, track: &Track, handle: &PlayerHandle) -> ClipSet {
    let mut set = ClipSet::default();
    // Where each clip of the set ends in its file, in the file's frames.
    let mut file_ends: Vec<i64> = Vec::new();
    for c in &track.clips {
        let ClipContent::Audio(a) = &c.content else {
            continue;
        };
        if c.muted {
            continue;
        }
        let path = session.media.resolve(&a.source);
        let engine_rate = session.sample_rate();
        let mut file_rate = engine_rate;
        let (loaded, stamp) = session.media.get(&path);
        let source = match loaded {
            Loaded::Memory(data, rate) => {
                file_rate = f64::from(rate);
                ClipSource::Memory(data)
            }
            Loaded::Stream(rate) if handle.has_stream(c.id, &path, stamp) => {
                file_rate = f64::from(rate);
                ClipSource::Stream(None)
            }
            Loaded::Stream(rate) => match session.media.open_streams(&path) {
                Ok(s) => {
                    file_rate = f64::from(rate);
                    ClipSource::Stream(Some(Box::new(s)))
                }
                Err(e) => {
                    session.warnings.push(Warning::CannotStream {
                        path: path.clone(),
                        detail: e.to_string(),
                    });
                    ClipSource::Missing
                }
            },
            Loaded::Missing => {
                session
                    .warnings
                    .push(Warning::CannotRead { path: path.clone() });
                ClipSource::Missing
            }
        };
        let start = session.ticks_to_samples(c.start).0;
        let end_tick = c.start.0 + c.length.0;
        let end = session.ticks_to_samples(TickTime(end_tick)).0;
        // Fades are in ticks, so they follow tempo changes like the clip.
        let fade_in = session
            .ticks_to_samples(TickTime(
                c.start.0.saturating_add(a.fade_in.length.0).min(end_tick),
            ))
            .0
            - start;
        let fade_out = end
            - session
                .ticks_to_samples(TickTime(
                    end_tick.saturating_sub(a.fade_out.length.0).max(c.start.0),
                ))
                .0;
        let (fade_in, fade_out) = daw_model::fit_fades(fade_in, fade_out, end - start);
        let file_rate = file_rate.max(1.0);
        let mut clip = PlayClip {
            id: c.id,
            stamp,
            start,
            end,
            // The model keeps the offset in the file's own frames.
            file_start: file_to_engine_frames(a.offset.0, file_rate, engine_rate),
            fade_in: true,
            fade_out: true,
            user_fade_in: (fade_in, a.fade_in.shape),
            user_fade_out: (fade_out, a.fade_out.shape),
            gain: 10f64.powf(a.gain_db / 20.0) as f32,
            path,
            source,
        };
        // Where the file is when the clip ends, in its own frames, as a
        // split works out the offset of its right half.
        let file_end = a.offset.0 + engine_to_file_frames(end - start, file_rate, engine_rate);
        // At another rate each part's length is rounded on its own, so the
        // parts of a file cut more than once can be a frame out.
        let slack = i64::from(file_rate != engine_rate);
        join_contiguous(&mut set.clips, &file_ends, &mut clip, a.offset.0, slack);
        file_ends.push(file_end);
        set.clips.push(clip);
    }
    set
}

/// Keeps which of a track's files `set` could not read.
pub(crate) fn note_unread(unread: &mut HashMap<TrackId, Vec<Seen>>, track: TrackId, set: &ClipSet) {
    match set.unread() {
        files if files.is_empty() => unread.remove(&track),
        files => unread.insert(track, files),
    };
}

/// A clip that continues an earlier one (the two halves of a split: same
/// file, one ends where the next starts, the file running on) plays without
/// the edge fades between them, so a cut is inaudible.
///
/// Whether the file runs on is asked in the file's own frames (`file_ends`
/// of `clips`, `offset` of `next`), as the model has them, give or take
/// `slack` frames. Each clip keeps the file position its own offset gives
/// it: one that followed its neighbour would have its stream seek, and
/// drop out, whenever the neighbour is edited.
fn join_contiguous(
    clips: &mut [PlayClip],
    file_ends: &[i64],
    next: &mut PlayClip,
    offset: i64,
    slack: i64,
) {
    let prev = clips.iter_mut().zip(file_ends).rev().find(|(p, file_end)| {
        p.end == next.start
            && p.path == next.path
            && (**file_end - offset).abs() <= slack
            && p.gain == next.gain
    });
    if let Some((prev, _)) = prev {
        prev.fade_out = false;
        next.fade_in = false;
    }
}

/// Restores a built-in device's saved state. A sampler's relative sample
/// paths are relative to `root`; a sample that will not load leaves its zone
/// silent and a warning in `warnings`.
fn load_device_state(
    dev: &mut dyn daw_devices::Device,
    data: &[u8],
    root: Option<&Path>,
    warnings: &mut Vec<Warning>,
) -> Result<(), daw_devices::StateError> {
    let Some(sampler) = dev.as_any_mut().downcast_mut::<daw_devices::Sampler>() else {
        return dev.load_state(data);
    };
    sampler.load_state_with(data, |path| {
        let path = match root {
            Some(root) if path.is_relative() => root.join(path),
            _ => path.to_path_buf(),
        };
        match daw_devices::SampleBuffer::load(&path) {
            Ok(sample) => Some(Arc::new(sample)),
            Err(_) => {
                warnings.push(Warning::CannotRead { path });
                None
            }
        }
    })
}

/// Mixes a stereo signal into one channel for a mono device, at half
/// level each, so a sound in the middle keeps its level.
struct MonoMix;

impl Node for MonoMix {
    fn ports(&self) -> PortLayout {
        PortLayout {
            audio_in: STRIP_CHANNELS,
            audio_out: 1,
            ..Default::default()
        }
    }

    fn prepare(&mut self, _sample_rate: f64, _max_frames: u32) {}

    fn process(&mut self, ctx: &mut ProcessContext) {
        let ([left, right], [out]) = (ctx.audio_in, &mut ctx.audio_out[..]) else {
            return;
        };
        for ((o, l), r) in out.iter_mut().zip(left.iter()).zip(right.iter()) {
            *o = 0.5 * (l + r);
        }
    }
}

/// Stands in for a device that can't be loaded: stereo through.
struct Bypass;

impl Node for Bypass {
    fn ports(&self) -> PortLayout {
        PortLayout {
            audio_in: PLAYER_CHANNELS,
            audio_out: PLAYER_CHANNELS,
            event_in: true,
            event_out: false,
        }
    }

    fn prepare(&mut self, _sample_rate: f64, _max_frames: u32) {}

    fn process(&mut self, ctx: &mut ProcessContext) {
        for (out, inp) in ctx.audio_out.iter_mut().zip(ctx.audio_in.iter()) {
            out.copy_from_slice(inp);
        }
    }
}
