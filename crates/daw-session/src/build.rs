//! Turning a project snapshot into an engine graph, parameter values and
//! clip sets.
//!
//! Every node that stands for a model object is keyed by that object's id,
//! so a rebuilt graph reuses the running node (keeping plugin and synth
//! state, meters and players) whenever the object is still there.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use daw_core::meter::PeakMeter;
use daw_core::node::{Node, PortLayout, ProcessContext};
use daw_core::time::TickTime;
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
use crate::media::Loaded;
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

/// The key of a bus's input: a unity gain stage in front of the strip, for
/// pre-fader sends from a bus with no devices to tap.
fn input_key(track: TrackId) -> NodeKey {
    NodeKey(track.0.as_u128() ^ 4)
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
}

/// Builds the whole graph for `snapshot`, reusing what `old` had running.
/// New players are sent their clips; reused ones keep theirs (the caller
/// sends changes afterwards).
pub(crate) fn graph(
    session: &mut Session,
    snapshot: &ProjectSnapshot,
    structure: Structure,
    mut old: Built,
) -> (GraphDesc, Built) {
    let (inputs, outputs) = (session.config.inputs, session.config.outputs);
    let mut g = session.controller.graph(inputs, outputs);
    let mut built = Built {
        structure,
        ..Default::default()
    };
    let muted = effective_mutes(snapshot);
    let master = snapshot.master.id;

    // Each track's chain: [player] -> devices -> strip. `heads` is where
    // audio routed into a track (from outputs and sends) enters.
    let mut heads: HashMap<TrackId, NodeId> = HashMap::new();
    let mut pre_fader: HashMap<TrackId, NodeId> = HashMap::new();
    let mut strips: HashMap<TrackId, NodeId> = HashMap::new();
    for track in snapshot.all_tracks() {
        let key = strip_key(snapshot, track.id);
        let strip = if track.id == master {
            g.master()
        } else if let Some(id) = g.reuse(key) {
            g.rename(id, &track.name);
            carry_params(&mut built, &old, key);
            id
        } else {
            let kind = if track.kind == TrackKind::Bus {
                NodeKind::Bus
            } else {
                NodeKind::Track
            };
            let mut strip = Strip::track(STRIP_CHANNELS)
                .with_gain(db_to_gain(track.volume_db) as f32)
                .with_pan(track.pan as f32);
            strip.set_muted(muted.contains(&track.id));
            let id = g.add_node(kind, &track.name, Box::new(strip));
            g.set_key(id, key);
            g.set_meter(id, Arc::new(NodeMeter::new(STRIP_CHANNELS as usize)));
            remember_strip(&mut built, key, track, &muted);
            id
        };
        if track.id == master {
            if g.meter(strip).is_none() {
                g.set_meter(strip, Arc::new(NodeMeter::new(outputs as usize)));
            }
            // The engine creates the master at unity gain and the old one
            // may have any; send the model's value after the swap.
            carry_params(&mut built, &old, key);
        }
        strips.insert(track.id, strip);

        let mut chain: Vec<NodeId> = Vec::new();
        if track.kind == TrackKind::Audio {
            let pkey = player_key(track.id);
            match old.players.remove(&track.id) {
                Some(handle) if g.can_reuse(pkey) => {
                    let id = g.reuse(pkey).expect("checked");
                    built.players.insert(track.id, handle);
                    if let Some(c) = old.clips.remove(&track.id) {
                        built.clips.insert(track.id, c);
                    }
                    chain.push(id);
                }
                _ => {
                    let (node, mut handle) =
                        player(session.loop_points.clone(), session.offline.clone());
                    let set = clip_set(session, track, &handle);
                    // A fresh queue has room for the first set.
                    let _ = handle.send(set);
                    built.clips.insert(track.id, track.clips.clone());
                    built.players.insert(track.id, handle);
                    let id = g.add_device(format!("{} clips", track.name), Box::new(node));
                    g.set_key(id, pkey);
                    chain.push(id);
                }
            }
        }
        // The input tap joins the chain after the player, so monitored input
        // goes through the track's devices like its clips do.
        let mut tap = None;
        if track.kind == TrackKind::Audio && inputs > 0 {
            let tkey = tap_key(track.id);
            let id = match old.taps.remove(&track.id) {
                Some(handle) if g.can_reuse(tkey) => {
                    built.taps.insert(track.id, handle);
                    g.reuse(tkey).expect("checked")
                }
                _ => {
                    let (node, handle) = input_tap(inputs);
                    built.taps.insert(track.id, handle);
                    let id = g.add_device(format!("{} input", track.name), Box::new(node));
                    g.set_key(id, tkey);
                    id
                }
            };
            for ch in 0..inputs {
                g.connect_audio(g.input(), ch, id, ch);
            }
            tap = Some(id);
        }
        // An instrument track's MIDI player feeds its first device events,
        // not audio, so it stays out of the audio chain.
        let mut midi = None;
        if track.kind == TrackKind::Instrument {
            let mkey = midi_key(track.id);
            midi = Some(match old.midi.remove(&track.id) {
                Some(handle) if g.can_reuse(mkey) => {
                    built.midi.insert(track.id, handle);
                    if let Some(c) = old.midi_clips.remove(&track.id) {
                        built.midi_clips.insert(track.id, c);
                    }
                    g.reuse(mkey).expect("checked")
                }
                _ => {
                    let (node, mut handle) = midi_player(MIDI_CAPTURE);
                    let seq = compile_track(track, &session.tempo, session.sample_rate());
                    // A fresh queue has room for the first sequence.
                    let _ = handle.set_sequence(seq);
                    built.midi_clips.insert(track.id, track.clips.clone());
                    built.midi.insert(track.id, handle);
                    let id = g.add_device(format!("{} notes", track.name), Box::new(node));
                    g.set_key(id, mkey);
                    id
                }
            });
        }
        for d in &track.devices {
            let key = device_key(d.id);
            let same_source = old.structure.device_source(d.id) == Some(&d.source);
            let plugin = matches!(d.source, DeviceSource::Plugin { .. });
            let id = match (same_source, g.can_reuse(key)) {
                // A plugin keeps its state in its handle; every graph gets a fresh node of
                // it, with its current ports and latency.
                _ if plugin => plugin_node(session, &mut g, &mut built, &old, d),
                (true, true) => {
                    let id = g.reuse(key).expect("checked");
                    g.rename(id, &d.name);
                    carry_params(&mut built, &old, key);
                    if let Some(m) = old.device_meters.remove(&d.id) {
                        built.device_meters.insert(d.id, m);
                    }
                    id
                }
                _ => {
                    let (node, meter, warning) = device_node(d, session.media.root());
                    if let Some(w) = warning {
                        session.warnings.push(w);
                    }
                    if let Some(m) = meter {
                        built.device_meters.insert(d.id, m);
                    }
                    let id = g.add_device(&d.name, node);
                    g.set_key(id, key);
                    for (&p, &v) in &d.params {
                        built.params.insert((key, p), v);
                    }
                    id
                }
            };
            // Silencing (`Session::silence`) cuts its tail or held voices.
            g.set_tail(id);
            // A switched-off device stays in the graph, so it keeps its
            // state, but audio goes around it.
            if d.enabled {
                chain.push(id);
            }
        }
        // A bus taps its pre-fader sends from the end of its devices; with
        // none switched on, from a unity stage in front of the strip.
        if chain.is_empty()
            && track.kind == TrackKind::Bus
            && track.sends.iter().any(|s| s.pre_fader)
        {
            let key = input_key(track.id);
            let id = match g.reuse(key) {
                Some(id) => id,
                None => {
                    let id = g.add_node(
                        NodeKind::Bus,
                        format!("{} input", track.name),
                        Box::new(Strip::send(STRIP_CHANNELS, 1.0)),
                    );
                    g.set_key(id, key);
                    id
                }
            };
            chain.push(id);
        }
        for pair in chain.windows(2) {
            g.connect(pair[0], pair[1]);
        }
        if let Some(tap) = tap {
            // The player is first in an audio track's chain.
            let after_player = chain.get(1).copied().unwrap_or(strip);
            g.connect(tap, after_player);
        }
        if let Some(&last) = chain.last() {
            g.connect(last, strip);
            pre_fader.insert(track.id, last);
        }
        // Audio routed into a bus goes through its devices; a track with no
        // player and no devices takes it straight into the strip.
        let head = match track.kind {
            TrackKind::Audio => strip,
            _ => chain.first().copied().unwrap_or(strip),
        };
        heads.insert(track.id, head);
        // Live MIDI goes through the player, which passes on what its
        // input gate lets through (see `Session::apply_gates`).
        if let Some(player) = midi {
            match built.structure.live_input(track.id) {
                LiveInput::All => g.route_external_events(player),
                LiveInput::Source(i) => g.route_external_events_from(player, i),
                LiveInput::None | LiveInput::Missing => {}
            }
            // The first device even when it is switched off: switching the
            // instrument off silences it, and it still gets the note-offs
            // of notes that were sounding.
            if let Some(first) = track.devices.first()
                && let Some(id) = find(&g, device_key(first.id))
            {
                g.connect_events(player, id);
            }
        }
    }

    // Commands and loading never let routing loop, but the engine would
    // refuse the whole graph if it did, so a route that would close a loop
    // is left out and reported instead.
    let mut routes = Routes::default();
    for track in snapshot.tracks.iter() {
        let target = match track.output {
            TrackOutput::Master => master,
            TrackOutput::Bus(b) => b,
        };
        if let Some(&head) = heads.get(&target) {
            if !routes.add(track.id, target) {
                session.warnings.push(Warning::RoutingLoop {
                    track: track.name.clone(),
                });
                continue;
            }
            g.connect(strips[&track.id], head);
        }
    }
    for track in snapshot.all_tracks() {
        for s in &track.sends {
            // Every send gets its node, connected or not, so its level and
            // automation always have somewhere to go.
            let key = send_key(s.id);
            let node = match g.reuse(key) {
                Some(id) => {
                    carry_params(&mut built, &old, key);
                    id
                }
                None => {
                    let gain = db_to_gain(s.level_db);
                    let id = g.add_node(
                        NodeKind::Send,
                        "send",
                        Box::new(Strip::send(STRIP_CHANNELS, gain as f32)),
                    );
                    g.set_key(id, key);
                    built.params.insert((key, Strip::GAIN), gain);
                    id
                }
            };
            let Some(&head) = heads.get(&s.target) else {
                continue;
            };
            let tap = if s.pre_fader {
                // Before the fader: the end of the device chain. A track with
                // nothing before its strip (an instrument track without
                // devices) has no pre-fader signal.
                match pre_fader.get(&track.id) {
                    Some(&n) => n,
                    None => continue,
                }
            } else {
                strips[&track.id]
            };
            if !routes.add(track.id, s.target) {
                session.warnings.push(Warning::RoutingLoop {
                    track: track.name.clone(),
                });
                continue;
            }
            g.connect(tap, node);
            g.connect(node, head);
        }
    }

    // The click goes through the master, so its meter shows it, and stays
    // in every graph so turning it on and off needs no rebuild.
    let click = match g.reuse(METRONOME_KEY) {
        Some(id) => {
            carry_params(&mut built, &old, METRONOME_KEY);
            id
        }
        None => {
            let mut node = daw_devices::Metronome::new();
            for ((key, param), value) in metronome_params(&session.metronome) {
                daw_devices::Device::set_param(&mut node, param, value);
                built.params.insert((key, param), value);
            }
            let id = g.add_device("metronome", Box::new(node));
            g.set_key(id, METRONOME_KEY);
            id
        }
    };
    g.connect(click, g.master());

    automate(session, snapshot, &mut g, &strips, &mut built, &old);
    // A lane that stopped playing (switched off, removed, emptied) left
    // its last value behind: send the control's own value again, or the
    // one it had before the lane, when the document has none.
    for &(key, param) in old.automated.difference(&built.automated) {
        built.params.remove(&(key, param));
        if let Some(v) = rest_value(session, snapshot, key, param, &old) {
            built.restore.insert((key, param), v);
        }
    }
    // Not sent yet (a full queue) and not taken over by a lane again.
    for (k, &v) in &old.restore {
        if !built.automated.contains(k) {
            built.restore.entry(*k).or_insert(v);
        }
    }
    (g, built)
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

/// Copies what was last sent to a reused node, so only real changes are
/// sent after the swap.
fn carry_params(built: &mut Built, old: &Built, key: NodeKey) {
    for (&(k, p), &v) in &old.params {
        if k == key {
            built.params.insert((k, p), v);
        }
    }
}

/// Records the values a new strip was created with.
fn remember_strip(built: &mut Built, key: NodeKey, track: &Track, muted: &HashSet<TrackId>) {
    built
        .params
        .insert((key, Strip::GAIN), db_to_gain(track.volume_db));
    built
        .params
        .insert((key, Strip::PAN), track.pan.clamp(-1.0, 1.0));
    let m = if muted.contains(&track.id) { 1.0 } else { 0.0 };
    built.params.insert((key, Strip::MUTE), m);
}

/// Adds the model's automation lanes to the graph, in engine samples.
fn automate(
    session: &Session,
    project: &Project,
    g: &mut GraphDesc,
    strips: &HashMap<TrackId, NodeId>,
    built: &mut Built,
    old: &Built,
) {
    for t in project.all_tracks() {
        for lane in t
            .automation
            .iter()
            .filter(|l| l.enabled && !l.points.is_empty())
        {
            let (node, param, to_value): (Option<NodeId>, u32, fn(f64) -> f64) = match lane.target {
                AutomationTarget::Volume => (strips.get(&t.id).copied(), Strip::GAIN, db_to_gain),
                AutomationTarget::Pan => (strips.get(&t.id).copied(), Strip::PAN, |v| v),
                AutomationTarget::SendLevel { send } => {
                    (find(g, send_key(send)), Strip::GAIN, db_to_gain)
                }
                AutomationTarget::DeviceParam { device, param } => {
                    (find(g, device_key(device)), param, |v| v)
                }
            };
            let Some(node) = node else { continue };
            let key = g.key(node);
            if session.suspended.contains(&lane.id) {
                // Overwritten by hand for now: the value from before the
                // lane still is what removing the lane goes back to.
                if let Some(k) = key
                    && let Some(&v) = old.rest.get(&(k, param))
                {
                    built.rest.insert((k, param), v);
                }
                continue;
            }
            if let Some(key) = key {
                built.automated.insert((key, param));
                if let Some(rest) = rest_value(session, project, key, param, old) {
                    built.rest.insert((key, param), rest);
                }
            }
            g.automate(node, param, session.playback_lane(lane, to_value));
        }
    }
}

/// What the device parameter `param` of the device node `key` goes back to
/// when its lane stops, if the document has no value for it (with one, the
/// document's value is sent again). `None` for other nodes.
///
/// A built-in device goes back to its default, as when the document's
/// value is reset. A plugin goes back to what it had before the lane took
/// over: the value taken when the lane started, else the value last sent
/// to it, else what it reports now (a lane that plays from the start).
fn rest_value(
    session: &Session,
    project: &Project,
    key: NodeKey,
    param: u32,
    old: &Built,
) -> Option<f64> {
    let d = project
        .all_tracks()
        .flat_map(|t| t.devices.iter())
        .find(|d| device_key(d.id) == key)?;
    if d.params.contains_key(&param) {
        return None;
    }
    match &d.source {
        DeviceSource::Builtin { kind } => daw_devices::param_specs(kind)?
            .iter()
            .find(|s| s.id == param)
            .map(|s| s.default),
        DeviceSource::Plugin { .. } => old
            .rest
            .get(&(key, param))
            .or(old.params.get(&(key, param)))
            .copied()
            .or_else(|| session.with_plugin(d.id, |p| p.param_value(param)))
            .filter(|v| v.is_finite()),
    }
}

/// The node with `key` in a graph being built.
fn find(g: &GraphDesc, key: NodeKey) -> Option<NodeId> {
    (0..g.len() as u32)
        .map(NodeId)
        .find(|&id| g.key(id) == Some(key))
}

/// The parameter value every strip, send and device should have.
pub(crate) fn params(project: &Project) -> Vec<((NodeKey, u32), f64)> {
    let muted = effective_mutes(project);
    let mut out = Vec::new();
    for t in project.all_tracks() {
        let key = strip_key(project, t.id);
        out.push(((key, Strip::GAIN), db_to_gain(t.volume_db)));
        if t.kind != TrackKind::Master {
            out.push(((key, Strip::PAN), t.pan.clamp(-1.0, 1.0)));
            let m = if muted.contains(&t.id) { 1.0 } else { 0.0 };
            out.push(((key, Strip::MUTE), m));
        }
        for s in &t.sends {
            out.push(((send_key(s.id), Strip::GAIN), db_to_gain(s.level_db)));
        }
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
        let source = match session.media.get(&path) {
            Loaded::Memory(data, rate) => {
                file_rate = f64::from(rate);
                ClipSource::Memory(data)
            }
            Loaded::Stream(rate) if handle.has_stream(c.id, &path) => {
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
        set.clips.push(PlayClip {
            id: c.id,
            start,
            end,
            // The model keeps the offset in the file's own frames.
            file_start: (a.offset.0 as f64 * engine_rate / file_rate.max(1.0)).round() as i64,
            fade_in: true,
            fade_out: true,
            user_fade_in: (fade_in, a.fade_in.shape),
            user_fade_out: (fade_out, a.fade_out.shape),
            gain: 10f64.powf(a.gain_db / 20.0) as f32,
            path,
            source,
        });
    }
    join_contiguous(&mut set);
    set
}

/// Clips that continue each other (the two halves of a split: same file,
/// one ends where the next starts, the file running on) play without the
/// edge fades between them, so a cut is inaudible.
fn join_contiguous(set: &mut ClipSet) {
    for i in 1..set.clips.len() {
        let (before, after) = set.clips.split_at_mut(i);
        let next = &mut after[0];
        if let Some(prev) = before
            .iter_mut()
            .rev()
            .find(|p| p.end == next.start && p.path == next.path)
            && prev.file_start + (prev.end - prev.start) == next.file_start
            && prev.gain == next.gain
        {
            prev.fade_out = false;
            next.fade_in = false;
        }
    }
}

/// Restores a built-in device's saved state. A sampler's relative sample
/// paths are relative to `root`.
fn load_device_state(
    dev: &mut dyn daw_devices::Device,
    data: &[u8],
    root: Option<&Path>,
) -> Result<(), daw_devices::StateError> {
    let Some(sampler) = dev.as_any_mut().downcast_mut::<daw_devices::Sampler>() else {
        return dev.load_state(data);
    };
    sampler.load_state_with(data, |path| {
        let path = match root {
            Some(root) if path.is_relative() => root.join(path),
            _ => path.to_path_buf(),
        };
        daw_devices::SampleBuffer::load(&path).ok().map(Arc::new)
    })
}

/// The node of a plugin device: a fresh node of its loaded plugin, or a
/// bypass when it can't play.
fn plugin_node(
    session: &mut Session,
    g: &mut GraphDesc,
    built: &mut Built,
    old: &Built,
    d: &Device,
) -> NodeId {
    let key = device_key(d.id);
    let (sample_rate, max_frames) = (session.sample_rate(), session.config.engine.max_frames);
    let node: Box<dyn Node> =
        match session
            .plugins
            .ensure(d, sample_rate, max_frames, &mut session.warnings)
        {
            Some(live) => {
                if !std::mem::take(&mut live.fresh) {
                    carry_params(built, old, key);
                }
                // A fresh plugin has had none of the document's values: the session sends
                // them all once the graph is in.
                Box::new(live.handle.node(sample_rate, max_frames))
            }
            None => Box::new(Bypass),
        };
    let id = g.add_device(&d.name, node);
    g.set_key(id, key);
    id
}

/// The engine node for a model device, its gain reduction meter if it has
/// one, and a warning if it can't be made.
///
/// Samplers find relative sample paths in `root`, the project bundle.
fn device_node(
    d: &Device,
    root: Option<&Path>,
) -> (Box<dyn Node>, Option<Arc<PeakMeter>>, Option<Warning>) {
    match &d.source {
        DeviceSource::Builtin { kind } => match daw_devices::create(kind) {
            Some(mut dev) => {
                let mut warning = None;
                if let Some(state) = &d.state
                    && let Err(e) = load_device_state(dev.as_mut(), &state.0, root)
                {
                    warning = Some(Warning::DeviceStateRejected {
                        device: d.name.clone(),
                        detail: e.to_string(),
                    });
                }
                for (&p, &v) in &d.params {
                    dev.set_param(p, v);
                }
                let meter = dev.gain_reduction_meter();
                (dev, meter, warning)
            }
            None => (
                Box::new(Bypass),
                None,
                Some(Warning::UnknownDevice { kind: kind.clone() }),
            ),
        },
        DeviceSource::Plugin { .. } => unreachable!("plugins are made by plugin_node"),
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
