//! Turning a project snapshot into an engine graph, parameter values and
//! clip sets.

use std::collections::{HashMap, HashSet, VecDeque};

use daw_core::node::{Node, PortLayout, ProcessContext};
use daw_core::time::TickTime;
use daw_engine::nodes::Strip;
use daw_engine::{GraphDesc, NodeId, NodeKind};
use daw_model::{
    ClipContent, DeviceId, DeviceSource, Project, ProjectSnapshot, SendId, Track, TrackId,
    TrackKind, TrackOutput,
};

use crate::media::Loaded;
use crate::player::{ClipSet, ClipSource, PLAYER_CHANNELS, PlayClip, PlayerHandle, player};
use crate::{Built, Session, db_to_gain};

/// Every channel strip in the graph is stereo.
const STRIP_CHANNELS: u16 = 2;

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
}

impl Structure {
    pub fn of(project: &Project) -> Self {
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
                })
                .collect(),
        }
    }
}

/// Builds the whole graph for `snapshot`, with fresh players already sent
/// their clips.
pub(crate) fn graph(
    session: &mut Session,
    snapshot: &ProjectSnapshot,
    structure: Structure,
) -> (GraphDesc, Built) {
    let cfg = &session.config;
    let mut g = GraphDesc::new(cfg.inputs, cfg.outputs);
    let mut built = Built {
        structure,
        ..Default::default()
    };

    let muted = effective_mutes(snapshot);
    // Each track's chain: [player] -> devices -> strip. `heads` is where
    // audio routed into a track (from outputs and sends) enters.
    let mut heads: HashMap<TrackId, NodeId> = HashMap::new();
    let mut pre_fader: HashMap<TrackId, NodeId> = HashMap::new();
    for track in snapshot.all_tracks() {
        let strip = if track.kind == TrackKind::Master {
            g.master()
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
            g.add_node(kind, &track.name, Box::new(strip))
        };
        built.strips.insert(track.id, strip);

        let mut chain: Vec<NodeId> = Vec::new();
        if track.kind == TrackKind::Audio {
            let (node, mut handle) = player(session.loop_points.clone());
            let set = clip_set(session, track, &handle);
            // A fresh queue has room for the first set.
            let _ = handle.send(set);
            built.clips.insert(track.id, track.clips.clone());
            built.players.insert(track.id, handle);
            chain.push(g.add_device(format!("{} clips", track.name), Box::new(node)));
        }
        for d in &track.devices {
            let (node, warning) = device_node(d);
            if let Some(w) = warning {
                session.warnings.push(w);
            }
            let id = g.add_device(&d.name, node);
            built.devices.insert(d.id, id);
            chain.push(id);
        }
        for pair in chain.windows(2) {
            g.connect(pair[0], pair[1]);
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
    }

    let master = snapshot.master.id;
    for track in snapshot.tracks.iter() {
        let strip = built.strips[&track.id];
        let target = match track.output {
            TrackOutput::Master => master,
            TrackOutput::Bus(b) => b,
        };
        if let Some(&head) = heads.get(&target) {
            g.connect(strip, head);
        }
    }
    for track in snapshot.all_tracks() {
        let strip = built.strips[&track.id];
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
                strip
            };
            let node = g.add_node(
                NodeKind::Send,
                "send",
                Box::new(Strip::send(STRIP_CHANNELS, db_to_gain(s.level_db) as f32)),
            );
            g.connect(tap, node);
            g.connect(node, head);
            built.sends.insert(s.id, node);
        }
    }

    // Nodes were created with the model's values (except the master gain,
    // set below); remember them so only later changes are sent.
    built.params = params(snapshot, &built).into_iter().collect();
    let master_gain = db_to_gain(snapshot.master.volume_db);
    if master_gain != 1.0 {
        // The engine makes the master strip at unity; correct it on the first
        // block.
        built.params.remove(&(g.master(), Strip::GAIN));
    }
    (g, built)
}

/// The parameter value every strip, send and device should have.
pub(crate) fn params(project: &Project, built: &Built) -> Vec<((NodeId, u32), f64)> {
    let muted = effective_mutes(project);
    let mut out = Vec::new();
    for t in project.all_tracks() {
        let Some(&strip) = built.strips.get(&t.id) else {
            continue;
        };
        out.push(((strip, Strip::GAIN), db_to_gain(t.volume_db)));
        if t.kind != TrackKind::Master {
            out.push(((strip, Strip::PAN), t.pan));
            let m = if muted.contains(&t.id) { 1.0 } else { 0.0 };
            out.push(((strip, Strip::MUTE), m));
        }
        for s in &t.sends {
            if let Some(&n) = built.sends.get(&s.id) {
                out.push(((n, Strip::GAIN), db_to_gain(s.level_db)));
            }
        }
        for d in &t.devices {
            let Some(&n) = built.devices.get(&d.id) else {
                continue;
            };
            for (&p, &v) in &d.params {
                out.push(((n, p), v));
            }
        }
    }
    out
}

/// Tracks that are silent: muted ones, and while anything is soloed, every
/// track that neither is soloed nor carries a soloed track's signal (the
/// buses a soloed track feeds, and the tracks feeding a soloed bus).
pub fn effective_mutes(project: &Project) -> HashSet<TrackId> {
    let soloed: Vec<TrackId> = project
        .all_tracks()
        .filter(|t| t.solo)
        .map(|t| t.id)
        .collect();
    let mut muted: HashSet<TrackId> = project
        .tracks
        .iter()
        .filter(|t| t.mute)
        .map(|t| t.id)
        .collect();
    if soloed.is_empty() {
        return muted;
    }
    let mut down: HashMap<TrackId, Vec<TrackId>> = HashMap::new();
    let mut up: HashMap<TrackId, Vec<TrackId>> = HashMap::new();
    for t in project.tracks.iter() {
        let out = match t.output {
            TrackOutput::Master => project.master.id,
            TrackOutput::Bus(b) => b,
        };
        for target in std::iter::once(out).chain(t.sends.iter().map(|s| s.target)) {
            down.entry(t.id).or_default().push(target);
            up.entry(target).or_default().push(t.id);
        }
    }
    let audible: HashSet<TrackId> = [&down, &up]
        .into_iter()
        .flat_map(|edges| reach(&soloed, edges))
        .collect();
    for t in project.tracks.iter() {
        if !audible.contains(&t.id) {
            muted.insert(t.id);
        }
    }
    muted
}

fn reach(from: &[TrackId], edges: &HashMap<TrackId, Vec<TrackId>>) -> HashSet<TrackId> {
    let mut seen: HashSet<TrackId> = from.iter().copied().collect();
    let mut queue: VecDeque<TrackId> = from.iter().copied().collect();
    while let Some(t) = queue.pop_front() {
        for &n in edges.get(&t).into_iter().flatten() {
            if seen.insert(n) {
                queue.push_back(n);
            }
        }
    }
    seen
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
        let source = match session.media.get(&path) {
            Loaded::Memory(data) => ClipSource::Memory(data),
            Loaded::Stream if handle.has_stream(c.id, &path) => ClipSource::Stream(None),
            Loaded::Stream => match session.media.open_streams(&path) {
                Ok(s) => ClipSource::Stream(Some(Box::new(s))),
                Err(e) => {
                    session
                        .warnings
                        .push(format!("can't stream {}: {e}", path.display()));
                    ClipSource::Missing
                }
            },
            Loaded::Missing => {
                session
                    .warnings
                    .push(format!("can't read {}", path.display()));
                ClipSource::Missing
            }
        };
        set.clips.push(PlayClip {
            id: c.id,
            start: session.ticks_to_samples(c.start).0,
            end: session
                .ticks_to_samples(TickTime(c.start.0 + c.length.0))
                .0,
            file_start: a.offset.0,
            gain: 10f64.powf(a.gain_db / 20.0) as f32,
            path,
            source,
        });
    }
    set
}

/// The engine node for a model device, and a warning if it can't be made.
fn device_node(d: &daw_model::Device) -> (Box<dyn Node>, Option<String>) {
    if !d.enabled {
        return (Box::new(Bypass), None);
    }
    match &d.source {
        DeviceSource::Builtin { kind } => match daw_devices::create(kind) {
            Some(mut dev) => {
                let mut warning = None;
                if let Some(state) = &d.state
                    && let Err(e) = dev.load_state(&state.0)
                {
                    warning = Some(format!("{}: {e}", d.name));
                }
                for (&p, &v) in &d.params {
                    dev.set_param(p, v);
                }
                (dev, warning)
            }
            None => (
                Box::new(Bypass),
                Some(format!("unknown built-in device {kind}")),
            ),
        },
        DeviceSource::Plugin { id, .. } => (
            Box::new(Bypass),
            Some(format!("plugin {id} is not loaded yet; it is bypassed")),
        ),
    }
}

/// Stands in for a device that is off or can't be loaded: stereo through.
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
