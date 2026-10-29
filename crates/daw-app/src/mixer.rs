//! What the Mix page needs, without naming Qt.
//!
//! [`MixerHandle`] is the mixer as the UI sees it: channel strips with
//! volume, pan, mute, solo, sends and an insert chain, plus their meters.
//! Like [`EngineHandle`](crate::engine::EngineHandle) it uses plain Rust
//! types so another front end can share it.
//!
//! Until the project model is wired to the engine (daw-session), the app
//! runs a [`PreviewMixer`]: a small demo mix on a real `daw-engine` graph
//! with built-in devices, processed in step with the UI clock. It exercises
//! the same engine paths the real mixer uses: keyed nodes that survive
//! graph edits, strip parameters by key, solo logic, peak and gain
//! reduction meters.

use std::sync::Arc;

use daw_core::event::EventList;
use daw_core::meter::PeakMeter;
use daw_core::node::{Node, PortLayout, ProcessContext};
use daw_engine::mixer::{SoloState, effective_mutes};
use daw_engine::nodes::Strip;
use daw_engine::{EngineConfig, EngineController, EngineProcessor, GraphDesc, NodeKey, NodeMeter};

pub type StripId = u64;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum StripKind {
    Track,
    Bus,
    Master,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InsertInfo {
    /// Built-in device id, such as `daw.compressor`.
    pub device_id: String,
    pub name: String,
    pub has_gain_reduction: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SendInfo {
    pub target: StripId,
    pub target_name: String,
    pub level_db: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StripInfo {
    pub id: StripId,
    pub name: String,
    pub kind: StripKind,
    /// Fader level; [`SILENT_DB`] or lower is silence.
    pub volume_db: f64,
    /// Balance, -1 (left) to 1 (right).
    pub pan: f64,
    pub muted: bool,
    pub soloed: bool,
    /// Silent because of mute or someone else's solo.
    pub silenced: bool,
    pub inserts: Vec<InsertInfo>,
    pub sends: Vec<SendInfo>,
}

/// Meter readings since the last call to [`MixerHandle::levels`].
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct StripLevels {
    /// Linear peak per channel (mono strips repeat channel 0).
    pub peak: [f32; 2],
    /// Highest gain reduction among the strip's dynamics inserts, in dB.
    pub gain_reduction_db: Option<f32>,
}

/// A device the user can insert on a strip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectChoice {
    pub id: String,
    pub name: String,
}

/// Fader positions at or below this are silence.
pub const SILENT_DB: f64 = -60.0;
/// Top of the fader.
pub const MAX_DB: f64 = 6.0;

/// Fader dB to linear gain.
pub fn db_to_gain(db: f64) -> f64 {
    if db <= SILENT_DB {
        0.0
    } else {
        daw_devices::dsp::db_to_gain(db.min(MAX_DB))
    }
}

/// What a meter shows: positions from 0 (-60 dB and below) to 1 (+6 dB).
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct MeterDisplay {
    pub level: [f64; 2],
    /// Recent highest level, held for a moment.
    pub hold: [f64; 2],
    /// Gain reduction in dB, falling back smoothly; `None` without a
    /// dynamics insert.
    pub gain_reduction_db: Option<f64>,
}

/// Meter ballistics: levels rise at once, fall at [`FALL_DB_PER_S`], and
/// peaks are held for [`HOLD_S`]. Runs on every UI tick, whether or not new
/// audio arrived, so meters fall back to rest when playback stops.
#[derive(Default)]
pub struct MeterBallistics {
    shown: Vec<MeterDisplay>,
    held_for: Vec<[f64; 2]>,
}

pub const FALL_DB_PER_S: f64 = 20.0;
pub const HOLD_S: f64 = 1.5;

/// A linear peak as a meter position.
pub fn meter_position(linear: f32) -> f64 {
    if linear <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * f64::from(linear).log10();
    ((db - SILENT_DB) / (MAX_DB - SILENT_DB)).clamp(0.0, 1.0)
}

impl MeterBallistics {
    pub fn update(&mut self, levels: &[StripLevels], seconds: f64) -> &[MeterDisplay] {
        self.shown.resize(levels.len(), MeterDisplay::default());
        self.held_for.resize(levels.len(), [0.0; 2]);
        let fall = FALL_DB_PER_S * seconds.max(0.0);
        let fall_pos = fall / (MAX_DB - SILENT_DB);
        for ((shown, held_for), l) in self.shown.iter_mut().zip(&mut self.held_for).zip(levels) {
            for (ch, held) in held_for.iter_mut().enumerate() {
                let level = meter_position(l.peak[ch])
                    .max(shown.level[ch] - fall_pos)
                    .max(0.0);
                shown.level[ch] = level;
                *held += seconds.max(0.0);
                if level >= shown.hold[ch] || *held > HOLD_S {
                    shown.hold[ch] = level;
                    *held = 0.0;
                }
            }
            shown.gain_reduction_db = l.gain_reduction_db.map(|gr| {
                let prev = shown.gain_reduction_db.unwrap_or(0.0);
                f64::from(gr).max(prev - fall).max(0.0)
            });
        }
        &self.shown
    }
}

/// The mixer as the UI sees it. Called on the UI thread only.
pub trait MixerHandle {
    /// Strips in display order: tracks, then buses, then master.
    fn strips(&self) -> Vec<StripInfo>;
    fn set_volume(&mut self, id: StripId, db: f64);
    fn set_pan(&mut self, id: StripId, pan: f64);
    fn set_muted(&mut self, id: StripId, muted: bool);
    fn set_soloed(&mut self, id: StripId, soloed: bool);
    fn set_send_level(&mut self, id: StripId, target: StripId, db: f64);
    /// Appends a built-in effect to the strip's insert chain.
    fn add_insert(&mut self, id: StripId, device_id: &str) -> bool;
    fn remove_insert(&mut self, id: StripId, index: usize);
    fn effects(&self) -> Vec<EffectChoice>;
    /// Meter readings, parallel to [`strips`](Self::strips). Resets them.
    fn levels(&mut self) -> Vec<StripLevels>;
    /// Advances the mixer by `seconds` of wall time. Only the preview needs
    /// it; a mixer on the live engine ignores it.
    fn tick(&mut self, _playing: bool, _seconds: f64) {}
}

/// The mixer the app starts with until the project model drives the engine.
pub fn default_mixer() -> Box<dyn MixerHandle> {
    Box::new(PreviewMixer::demo())
}

struct Insert {
    /// Unique per insert, so the node survives edits to the chain.
    serial: u64,
    device_id: &'static str,
    name: &'static str,
    gr: Option<Arc<PeakMeter>>,
    /// The device, until the next rebuild moves it into the engine.
    fresh: Option<Box<dyn daw_devices::Device>>,
}

struct Send {
    target: StripId,
    level_db: f64,
}

struct PreviewStrip {
    id: StripId,
    name: String,
    kind: StripKind,
    volume_db: f64,
    pan: f64,
    muted: bool,
    soloed: bool,
    inserts: Vec<Insert>,
    sends: Vec<Send>,
    /// Demo source of a track: tone frequency and pulse rate.
    tone: Option<(f64, f64)>,
    meter: Arc<NodeMeter>,
}

const SAMPLE_RATE: f64 = 48_000.0;
const BLOCK: usize = 256;

/// A demo mix running on a real engine graph. See the module docs.
pub struct PreviewMixer {
    strips: Vec<PreviewStrip>,
    ctl: EngineController,
    proc: EngineProcessor,
    next_serial: u64,
    /// Frames owed to the engine from earlier ticks.
    pending: f64,
    scratch: [Vec<f32>; 2],
    events: EventList,
    effects: Vec<EffectChoice>,
}

/// Node keys: strip id in the high bits, role and index below.
fn key(strip: StripId, role: u32, index: u64) -> NodeKey {
    NodeKey(((strip as u128) << 64) | ((role as u128) << 48) | index as u128)
}
const ROLE_STRIP: u32 = 0;
const ROLE_SOURCE: u32 = 1;
const ROLE_INSERT: u32 = 2;
const ROLE_SEND: u32 = 3;

impl PreviewMixer {
    /// Three tracks, a reverb bus and master.
    pub fn demo() -> Self {
        let (ctl, proc) = EngineController::new(EngineConfig {
            sample_rate: SAMPLE_RATE,
            max_frames: BLOCK as u32,
            ..Default::default()
        });
        let mut m = Self {
            strips: Vec::new(),
            ctl,
            proc,
            next_serial: 1,
            pending: 0.0,
            scratch: [vec![0.0; BLOCK], vec![0.0; BLOCK]],
            events: EventList::with_capacity(0),
            effects: effect_choices(),
        };
        let track = |id, name: &str, tone| PreviewStrip {
            id,
            name: name.to_owned(),
            kind: StripKind::Track,
            volume_db: 0.0,
            pan: 0.0,
            muted: false,
            soloed: false,
            inserts: Vec::new(),
            sends: Vec::new(),
            tone: Some(tone),
            meter: Arc::new(NodeMeter::new(2)),
        };
        m.strips.push(track(1, "Drums", (110.0, 4.0)));
        m.strips.push(track(2, "Bass", (55.0, 1.0)));
        m.strips.push(track(3, "Keys", (440.0, 0.5)));
        m.strips[1].pan = -0.3;
        m.strips[2].pan = 0.4;
        m.strips.push(PreviewStrip {
            kind: StripKind::Bus,
            tone: None,
            ..track(10, "Reverb", (0.0, 0.0))
        });
        m.strips.push(PreviewStrip {
            kind: StripKind::Master,
            tone: None,
            ..track(0, "Master", (0.0, 0.0))
        });
        for t in [1, 3] {
            m.strip_mut(t).sends.push(Send {
                target: 10,
                level_db: -12.0,
            });
        }
        m.insert(3, "daw.compressor");
        m.insert(10, "daw.reverb");
        m.rebuild();
        m
    }

    fn strip(&self, id: StripId) -> Option<&PreviewStrip> {
        self.strips.iter().find(|s| s.id == id)
    }

    fn strip_mut(&mut self, id: StripId) -> &mut PreviewStrip {
        self.strips
            .iter_mut()
            .find(|s| s.id == id)
            .expect("strip ids come from strips()")
    }

    /// Adds an insert to the model without rebuilding.
    fn insert(&mut self, id: StripId, device_id: &str) -> bool {
        if self.strip(id).is_none() || !self.effects.iter().any(|e| e.id == device_id) {
            return false;
        }
        let Some(device) = daw_devices::create(device_id) else {
            return false;
        };
        let serial = self.next_serial;
        self.next_serial += 1;
        let info = device.info();
        self.strip_mut(id).inserts.push(Insert {
            serial,
            device_id: info.id,
            name: info.name,
            gr: device.gain_reduction_meter(),
            fresh: Some(device),
        });
        true
    }

    /// Rebuilds the graph after a structural edit, reusing every node that
    /// already runs so sources and effects keep their state.
    fn rebuild(&mut self) {
        let mut g = self.ctl.graph(0, 2);
        let mut strip_nodes = Vec::with_capacity(self.strips.len());
        for s in &mut self.strips {
            let strip_key = key(s.id, ROLE_STRIP, 0);
            let node = if s.kind == StripKind::Master {
                g.master()
            } else {
                reuse_or(&mut g, strip_key, || {
                    let kind = if s.kind == StripKind::Bus {
                        daw_engine::NodeKind::Bus
                    } else {
                        daw_engine::NodeKind::Track
                    };
                    let strip = Strip::track(2)
                        .with_gain(db_to_gain(s.volume_db) as f32)
                        .with_pan(s.pan as f32);
                    (kind, s.name.clone(), Box::new(strip) as Box<dyn Node>)
                })
            };
            g.set_meter(node, s.meter.clone());
            // Chain: source -> inserts -> strip.
            let mut prev = s.tone.map(|(freq, rate)| {
                reuse_or(&mut g, key(s.id, ROLE_SOURCE, 0), || {
                    let tone = Tone::new(freq, rate);
                    (
                        daw_engine::NodeKind::Device,
                        "tone".into(),
                        Box::new(tone) as _,
                    )
                })
            });
            if s.kind == StripKind::Master {
                // Master's inputs are summed before its inserts would run;
                // the preview keeps master free of inserts.
                strip_nodes.push(node);
                continue;
            }
            for ins in &mut s.inserts {
                let k = key(s.id, ROLE_INSERT, ins.serial);
                let id = match g.reuse(k) {
                    Some(id) => id,
                    None => {
                        let dev = match ins.fresh.take() {
                            Some(dev) => dev,
                            // Lost with a graph the engine refused: start over.
                            None => daw_devices::create(ins.device_id)
                                .expect("inserts come from the catalog"),
                        };
                        ins.gr = dev.gain_reduction_meter();
                        let id = g.add_device(ins.name, dev);
                        g.set_key(id, k);
                        id
                    }
                };
                if let Some(p) = prev {
                    g.connect(p, id);
                }
                prev = Some(id);
            }
            if let Some(p) = prev {
                g.connect(p, node);
            }
            strip_nodes.push(node);
        }
        let master = g.master();
        for (s, &node) in self.strips.iter().zip(&strip_nodes) {
            if s.kind == StripKind::Master {
                continue;
            }
            g.connect(node, master);
            for send in &s.sends {
                let Some(t) = self.strips.iter().position(|x| x.id == send.target) else {
                    continue;
                };
                let k = key(s.id, ROLE_SEND, send.target);
                let id = match g.reuse(k) {
                    Some(id) => {
                        g.connect(node, id);
                        id
                    }
                    None => {
                        let id = g.add_send(node, strip_nodes[t], db_to_gain(send.level_db) as f32);
                        g.set_key(id, k);
                        continue;
                    }
                };
                g.connect(id, strip_nodes[t]);
            }
        }
        if let Err(e) = self.ctl.set_graph(g) {
            eprintln!("kadence: preview mixer graph rejected: {e}");
        }
        self.apply_mutes();
    }

    /// Sends every strip's effective mute (mute buttons plus solo).
    fn apply_mutes(&mut self) {
        let mutes = self.silenced();
        for (s, muted) in self.strips.iter().zip(mutes) {
            if s.kind != StripKind::Master {
                let k = key(s.id, ROLE_STRIP, 0);
                let _ = self
                    .ctl
                    .set_param_by_key(k, Strip::MUTE, if muted { 1.0 } else { 0.0 });
            }
        }
    }

    fn silenced(&self) -> Vec<bool> {
        let states: Vec<SoloState> = self
            .strips
            .iter()
            .map(|s| SoloState {
                muted: s.muted,
                soloed: s.soloed,
                solo_safe: s.kind == StripKind::Master,
            })
            .collect();
        let index = |id| self.strips.iter().position(|s| s.id == id);
        let mut feeds = Vec::new();
        for (i, s) in self.strips.iter().enumerate() {
            for send in &s.sends {
                if let Some(t) = index(send.target) {
                    feeds.push((i, t));
                }
            }
        }
        effective_mutes(&states, &feeds)
    }

    fn strip_key(&self, id: StripId) -> NodeKey {
        match self.strip(id).map(|s| s.kind) {
            Some(StripKind::Master) => NodeKey::MASTER,
            _ => key(id, ROLE_STRIP, 0),
        }
    }
}

/// Built-in devices that process their input: effects and utilities with
/// audio inputs, not instruments or generators like the metronome.
fn effect_choices() -> Vec<EffectChoice> {
    daw_devices::devices()
        .iter()
        .filter(|e| {
            e.info.category != daw_devices::DeviceCategory::Instrument
                && (e.create)().ports().audio_in > 0
        })
        .map(|e| EffectChoice {
            id: e.info.id.to_owned(),
            name: e.info.name.to_owned(),
        })
        .collect()
}

fn reuse_or(
    g: &mut GraphDesc,
    k: NodeKey,
    make: impl FnOnce() -> (daw_engine::NodeKind, String, Box<dyn Node>),
) -> daw_engine::NodeId {
    if let Some(id) = g.reuse(k) {
        return id;
    }
    let (kind, name, node) = make();
    let id = g.add_node(kind, name, node);
    g.set_key(id, k);
    id
}

impl MixerHandle for PreviewMixer {
    fn strips(&self) -> Vec<StripInfo> {
        let silenced = self.silenced();
        self.strips
            .iter()
            .zip(silenced)
            .map(|(s, silenced)| StripInfo {
                id: s.id,
                name: s.name.clone(),
                kind: s.kind,
                volume_db: s.volume_db,
                pan: s.pan,
                muted: s.muted,
                soloed: s.soloed,
                silenced,
                inserts: s
                    .inserts
                    .iter()
                    .map(|i| InsertInfo {
                        device_id: i.device_id.to_owned(),
                        name: i.name.to_owned(),
                        has_gain_reduction: i.gr.is_some(),
                    })
                    .collect(),
                sends: s
                    .sends
                    .iter()
                    .map(|x| SendInfo {
                        target: x.target,
                        target_name: self
                            .strip(x.target)
                            .map(|t| t.name.clone())
                            .unwrap_or_default(),
                        level_db: x.level_db,
                    })
                    .collect(),
            })
            .collect()
    }

    fn set_volume(&mut self, id: StripId, db: f64) {
        if self.strip(id).is_none() {
            return;
        }
        let db = db.clamp(SILENT_DB, MAX_DB);
        self.strip_mut(id).volume_db = db;
        let k = self.strip_key(id);
        let _ = self.ctl.set_param_by_key(k, Strip::GAIN, db_to_gain(db));
    }

    fn set_pan(&mut self, id: StripId, pan: f64) {
        if self.strip(id).is_none_or(|s| s.kind == StripKind::Master) {
            return;
        }
        let pan = pan.clamp(-1.0, 1.0);
        self.strip_mut(id).pan = pan;
        let _ = self
            .ctl
            .set_param_by_key(key(id, ROLE_STRIP, 0), Strip::PAN, pan);
    }

    fn set_muted(&mut self, id: StripId, muted: bool) {
        if self.strip(id).is_some_and(|s| s.kind != StripKind::Master) {
            self.strip_mut(id).muted = muted;
            self.apply_mutes();
        }
    }

    fn set_soloed(&mut self, id: StripId, soloed: bool) {
        if self.strip(id).is_some_and(|s| s.kind != StripKind::Master) {
            self.strip_mut(id).soloed = soloed;
            self.apply_mutes();
        }
    }

    fn set_send_level(&mut self, id: StripId, target: StripId, db: f64) {
        let db = db.clamp(SILENT_DB, MAX_DB);
        let Some(s) = self.strips.iter_mut().find(|s| s.id == id) else {
            return;
        };
        let Some(send) = s.sends.iter_mut().find(|x| x.target == target) else {
            return;
        };
        send.level_db = db;
        let _ = self
            .ctl
            .set_param_by_key(key(id, ROLE_SEND, target), Strip::GAIN, db_to_gain(db));
    }

    fn add_insert(&mut self, id: StripId, device_id: &str) -> bool {
        if self.strip(id).is_some_and(|s| s.kind == StripKind::Master) {
            return false;
        }
        let added = self.insert(id, device_id);
        if added {
            self.rebuild();
        }
        added
    }

    fn remove_insert(&mut self, id: StripId, index: usize) {
        let Some(s) = self.strips.iter_mut().find(|s| s.id == id) else {
            return;
        };
        if index < s.inserts.len() {
            s.inserts.remove(index);
            self.rebuild();
        }
    }

    fn effects(&self) -> Vec<EffectChoice> {
        self.effects.clone()
    }

    fn levels(&mut self) -> Vec<StripLevels> {
        self.strips
            .iter()
            .map(|s| {
                let peaks = s.meter.take();
                let l = peaks.first().copied().unwrap_or(0.0);
                let r = peaks.get(1).copied().unwrap_or(l);
                let gr = s
                    .inserts
                    .iter()
                    .filter_map(|i| i.gr.as_ref().map(|m| m.take()))
                    .reduce(f32::max);
                StripLevels {
                    peak: [l, r],
                    gain_reduction_db: gr,
                }
            })
            .collect()
    }

    fn tick(&mut self, playing: bool, seconds: f64) {
        if playing != self.ctl.is_playing() {
            let _ = if playing {
                self.ctl.play()
            } else {
                self.ctl.stop()
            };
        }
        // Never catch up more than a quarter second after a stall.
        self.pending = (self.pending + seconds.max(0.0) * SAMPLE_RATE).min(SAMPLE_RATE / 4.0);
        while self.pending >= BLOCK as f64 {
            let [l, r] = &mut self.scratch;
            self.proc
                .process(BLOCK, &[], &mut [&mut l[..], &mut r[..]], &self.events);
            self.pending -= BLOCK as f64;
        }
        self.ctl.collect_garbage();
    }
}

/// Demo source: a tone pulsing `rate` times a second while the transport
/// plays, a little louder on one side so pan and balance show on meters.
struct Tone {
    freq: f64,
    rate: f64,
    phase: f64,
    sample_rate: f64,
}

impl Tone {
    fn new(freq: f64, rate: f64) -> Self {
        Self {
            freq,
            rate,
            phase: 0.0,
            sample_rate: SAMPLE_RATE,
        }
    }
}

impl Node for Tone {
    fn ports(&self) -> PortLayout {
        PortLayout {
            audio_out: 2,
            ..Default::default()
        }
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: u32) {
        self.sample_rate = sample_rate;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        if !ctx.transport.playing {
            return;
        }
        let pos = ctx.transport.position.0;
        for i in 0..ctx.frames as usize {
            let t = (pos + i as i64) as f64 / self.sample_rate;
            // Decaying pulse envelope.
            let env = (-(t * self.rate).fract() * 6.0).exp() * 0.7;
            self.phase = (self.phase + self.freq / self.sample_rate).fract();
            let v = (env * (self.phase * std::f64::consts::TAU).sin()) as f32;
            for ch in ctx.audio_out.iter_mut() {
                ch[i] = v;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(m: &mut PreviewMixer) -> Vec<StripLevels> {
        m.levels();
        m.tick(true, 0.1);
        m.levels()
    }

    #[test]
    fn demo_mix_meters_move_while_playing() {
        let mut m = PreviewMixer::demo();
        m.tick(false, 0.1);
        assert!(m.levels().iter().all(|l| l.peak == [0.0, 0.0]));
        let levels = run(&mut m);
        let strips = m.strips();
        for (s, l) in strips.iter().zip(&levels) {
            assert!(l.peak[0] > 0.0, "{} is silent", s.name);
        }
        let keys = strips.iter().position(|s| s.name == "Keys").unwrap();
        let gr = levels[keys]
            .gain_reduction_db
            .expect("Keys has a compressor");
        assert!(gr > 0.0, "the demo drives the compressor: {gr}");
    }

    #[test]
    fn solo_silences_other_tracks_but_keeps_returns() {
        let mut m = PreviewMixer::demo();
        m.set_soloed(1, true);
        let strips = m.strips();
        let silenced: Vec<_> = strips
            .iter()
            .map(|s| (s.name.as_str(), s.silenced))
            .collect();
        assert_eq!(
            silenced,
            [
                ("Drums", false),
                ("Bass", true),
                ("Keys", true),
                ("Reverb", false),
                ("Master", false)
            ]
        );
        let levels = run(&mut m);
        assert_eq!(levels[1].peak, [0.0, 0.0]);
        assert!(levels[0].peak[0] > 0.0);
    }

    #[test]
    fn fader_and_pan_reach_the_engine() {
        let mut m = PreviewMixer::demo();
        let before = run(&mut m)[0].peak[0];
        m.set_volume(1, -20.0);
        m.set_pan(1, 1.0);
        let after = run(&mut m)[0];
        assert!(after.peak[0] == 0.0, "hard right silences the left channel");
        assert!(after.peak[1] < before * 0.2);
        m.set_volume(1, SILENT_DB);
        assert_eq!(run(&mut m)[0].peak, [0.0, 0.0]);
    }

    #[test]
    fn editing_inserts_keeps_running_nodes() {
        let mut m = PreviewMixer::demo();
        run(&mut m);
        let swaps = m.ctl.graphs_swapped();
        assert!(m.add_insert(2, "daw.eq"));
        assert!(!m.add_insert(2, "daw.synth"), "instruments are not inserts");
        assert!(
            !m.add_insert(0, "daw.eq"),
            "master has no inserts in the preview"
        );
        assert!(m.add_insert(2, "daw.compressor"));
        m.remove_insert(2, 0);
        run(&mut m);
        assert_eq!(m.ctl.graphs_swapped(), swaps + 3);
        let bass = &m.strips()[1];
        assert_eq!(bass.inserts.len(), 1);
        assert!(bass.inserts[0].has_gain_reduction);
        // The Keys compressor, untouched by these edits, still meters.
        assert!(run(&mut m)[2].gain_reduction_db.is_some_and(|gr| gr > 0.0));
        assert!(m.ctl.has_node(key(3, ROLE_INSERT, 1)));
    }

    #[test]
    fn effects_list_only_has_effects() {
        let effects = PreviewMixer::demo().effects();
        assert!(effects.iter().any(|e| e.id == "daw.compressor"));
        assert!(effects.iter().all(|e| e.id != "daw.synth"));
        assert!(effects.iter().all(|e| e.id != "daw.metronome"));
        assert!(effects.iter().any(|e| e.id == "daw.utility"));
    }

    #[test]
    fn meters_fall_back_and_hold_peaks() {
        let mut b = MeterBallistics::default();
        let loud = StripLevels {
            peak: [1.0, 0.5],
            gain_reduction_db: Some(6.0),
        };
        let quiet = StripLevels {
            peak: [0.0, 0.0],
            gain_reduction_db: Some(0.0),
        };
        let first = b.update(&[loud], 0.03)[0];
        assert!((first.level[0] - meter_position(1.0)).abs() < 1e-9);
        let after = b.update(&[quiet], 1.0)[0];
        // 20 dB in a second, out of a 66 dB scale.
        assert!((first.level[0] - after.level[0] - 20.0 / 66.0).abs() < 1e-9);
        assert_eq!(after.hold, first.level, "peak held");
        assert_eq!(after.gain_reduction_db, Some(0.0));
        // Silence keeps falling to rest, and the hold drops after it expires.
        for _ in 0..10 {
            b.update(&[quiet], 1.0);
        }
        let rest = b.update(&[quiet], 0.03)[0];
        assert_eq!(rest.level, [0.0, 0.0]);
        assert_eq!(rest.hold, [0.0, 0.0]);
    }
}
