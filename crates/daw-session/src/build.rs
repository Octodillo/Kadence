//! Turning a project snapshot into an engine graph, parameter values and
//! clip sets.
//!
//! Every node that stands for a model object is keyed by that object's id,
//! so a rebuilt graph reuses the running node (keeping plugin and synth
//! state, meters and players) whenever the object is still there.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use daw_core::meter::PeakMeter;
use daw_core::node::{Node, PortLayout, ProcessContext};
use daw_core::time::TickTime;
use daw_engine::mixer::SoloState;
use daw_engine::nodes::Strip;
use daw_engine::{
    AutomationLane, AutomationPoint, GraphDesc, NodeId, NodeKey, NodeKind, NodeMeter,
};
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
}

impl Structure {
    /// The shape of `project`, leaving out the automation lanes in
    /// `suspended`.
    pub fn of(project: &Project, suspended: &HashSet<LaneId>) -> Self {
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
                    let (node, mut handle) = player(session.loop_points.clone());
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
            let id = match (same_source, g.can_reuse(key)) {
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
                    let (node, meter, warning) = device_node(d);
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
            // A switched-off device stays in the graph, so it keeps its
            // state, but audio goes around it.
            if d.enabled {
                chain.push(id);
            }
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
        // Live MIDI goes through every instrument track's player (which
        // passes it on) for now; record arming will narrow it down.
        if let Some(player) = midi {
            g.route_external_events(player);
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

    for track in snapshot.tracks.iter() {
        let target = match track.output {
            TrackOutput::Master => master,
            TrackOutput::Bus(b) => b,
        };
        if let Some(&head) = heads.get(&target) {
            g.connect(strips[&track.id], head);
        }
    }
    for track in snapshot.all_tracks() {
        for s in &track.sends {
            let Some(&head) = heads.get(&s.target) else {
                continue;
            };
            let tap = if s.pre_fader {
                // Before the fader: the end of the device chain. A track with
                // nothing before its strip has no pre-fader signal.
                match pre_fader.get(&track.id) {
                    Some(&n) => n,
                    None => continue,
                }
            } else {
                strips[&track.id]
            };
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

    automate(session, snapshot, &mut g, &strips);
    (g, built)
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
) {
    for t in project.all_tracks() {
        for lane in t
            .automation
            .iter()
            .filter(|l| l.enabled && !l.points.is_empty() && !session.suspended.contains(&l.id))
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
            let points = session
                .engine_lane(&lane.points)
                .points()
                .iter()
                .map(|p| AutomationPoint {
                    value: to_value(p.value),
                    ..*p
                })
                .collect();
            g.automate(node, param, AutomationLane::new(points));
        }
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

/// The engine node for a model device, its gain reduction meter if it has
/// one, and a warning if it can't be made.
fn device_node(d: &Device) -> (Box<dyn Node>, Option<Arc<PeakMeter>>, Option<Warning>) {
    match &d.source {
        DeviceSource::Builtin { kind } => match daw_devices::create(kind) {
            Some(mut dev) => {
                let mut warning = None;
                if let Some(state) = &d.state
                    && let Err(e) = dev.load_state(&state.0)
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
        DeviceSource::Plugin { id, .. } => (
            Box::new(Bypass),
            None,
            Some(Warning::PluginNotLoaded {
                device: d.name.clone(),
                plugin: id.to_string(),
            }),
        ),
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
