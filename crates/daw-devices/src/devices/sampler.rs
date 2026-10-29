//! Sampler: plays in-memory sample buffers, either one-shot (drum style,
//! unpitched, ignores note-off) or key-mapped (pitched from a root key, with
//! a release on note-off). Zones choose a sample by key and velocity.

use std::sync::Arc;

use daw_core::event::{NoteId, ParamId};
use daw_core::node::{Node, PortLayout, ProcessContext};

use crate::dsp::{Adsr, db_to_gain};
use crate::events::{Note, Segmented, output, process_segmented};
use crate::param::{ParamSpec, ParamStore, ParamUnit};
use crate::{DeviceCategory, DeviceInfo, impl_device};

pub static INFO: DeviceInfo = DeviceInfo {
    id: "daw.sampler",
    name: "Sampler",
    category: DeviceCategory::Instrument,
    description: "One-shot and key-mapped sample playback",
};

pub const MODE: ParamId = 0;
pub const GAIN: ParamId = 1;
pub const ATTACK: ParamId = 2;
pub const RELEASE: ParamId = 3;
pub const TUNE: ParamId = 4;
pub const VELOCITY: ParamId = 5;

pub const MODE_ONE_SHOT: f64 = 0.0;
pub const MODE_KEY_MAPPED: f64 = 1.0;

static PARAMS: &[ParamSpec] = &[
    ParamSpec::choice(MODE, "mode", "Mode", &["One-shot", "Key-mapped"], 1),
    ParamSpec::new(GAIN, "gain", "Gain", -70.0, 12.0, 0.0, ParamUnit::Decibels),
    ParamSpec::new(
        ATTACK,
        "attack",
        "Attack",
        0.0,
        5000.0,
        0.0,
        ParamUnit::Milliseconds,
    ),
    ParamSpec::new(
        RELEASE,
        "release",
        "Release",
        1.0,
        10000.0,
        100.0,
        ParamUnit::Milliseconds,
    )
    .log(),
    ParamSpec::new(TUNE, "tune", "Tune", -24.0, 24.0, 0.0, ParamUnit::Semitones),
    ParamSpec::new(
        VELOCITY,
        "velocity",
        "Velocity Sensitivity",
        0.0,
        1.0,
        1.0,
        ParamUnit::Percent,
    ),
];

const VOICES: usize = 32;

/// Decoded audio held in memory. Mono or stereo; extra channels are ignored.
#[derive(Clone, Debug)]
pub struct SampleBuffer {
    pub channels: Vec<Vec<f32>>,
    pub sample_rate: f64,
}

impl SampleBuffer {
    pub fn mono(data: Vec<f32>, sample_rate: f64) -> Self {
        Self {
            channels: vec![data],
            sample_rate,
        }
    }

    pub fn len(&self) -> usize {
        self.channels.iter().map(Vec::len).min().unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Maps a key and velocity range to a sample.
#[derive(Clone, Debug)]
pub struct Zone {
    pub sample: Arc<SampleBuffer>,
    /// Key at which the sample plays at its recorded pitch (key-mapped mode).
    pub root_key: u8,
    pub key_lo: u8,
    pub key_hi: u8,
    /// Velocity range in `0..=1`.
    pub velocity_lo: f32,
    pub velocity_hi: f32,
    pub gain_db: f32,
}

impl Zone {
    /// A zone that covers every key and velocity, rooted at middle C.
    pub fn full(sample: Arc<SampleBuffer>) -> Self {
        Self {
            sample,
            root_key: 60,
            key_lo: 0,
            key_hi: 127,
            velocity_lo: 0.0,
            velocity_hi: 1.0,
            gain_db: 0.0,
        }
    }

    pub fn keys(mut self, lo: u8, hi: u8, root: u8) -> Self {
        self.key_lo = lo;
        self.key_hi = hi;
        self.root_key = root;
        self
    }

    fn matches(&self, key: u8, velocity: f32) -> bool {
        (self.key_lo..=self.key_hi).contains(&key)
            && velocity >= self.velocity_lo
            && velocity <= self.velocity_hi
    }
}

#[derive(Clone, Copy, Default)]
struct Voice {
    active: bool,
    zone: usize,
    key: u8,
    id: NoteId,
    pos: f64,
    rate: f64,
    gain: f32,
    age: u64,
    env: Adsr,
}

pub struct Sampler {
    params: ParamStore,
    zones: Vec<Zone>,
    voices: [Voice; VOICES],
    sample_rate: f64,
    clock: u64,
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

/// 4-point, 3rd-order Hermite interpolation.
#[inline]
fn hermite(data: &[f32], pos: f64) -> f32 {
    let i = pos as usize;
    let t = (pos - i as f64) as f32;
    let at = |j: isize| -> f32 {
        let k = i as isize + j;
        if k < 0 {
            0.0
        } else {
            data.get(k as usize).copied().unwrap_or(0.0)
        }
    };
    let (xm1, x0, x1, x2) = (at(-1), at(0), at(1), at(2));
    let c1 = 0.5 * (x1 - xm1);
    let c2 = xm1 - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
    let c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);
    ((c3 * t + c2) * t + c1) * t + x0
}

impl Sampler {
    pub fn new() -> Self {
        Self {
            params: ParamStore::new(PARAMS),
            zones: Vec::new(),
            voices: [Voice::default(); VOICES],
            sample_rate: 48000.0,
            clock: 0,
        }
    }

    /// Replace the zone map. Call off the real-time thread; stops all voices.
    pub fn set_zones(&mut self, zones: Vec<Zone>) {
        self.voices = [Voice::default(); VOICES];
        self.zones = zones;
    }

    pub fn zones(&self) -> &[Zone] {
        &self.zones
    }

    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }

    fn one_shot(&self) -> bool {
        self.params.get(MODE) == MODE_ONE_SHOT
    }

    fn apply_param(&mut self, id: ParamId) {
        if id == ATTACK || id == RELEASE {
            let sr = self.sample_rate;
            let (a, r) = (self.params.get(ATTACK), self.params.get(RELEASE));
            for v in &mut self.voices {
                v.env.set(a, 1.0, 1.0, r, sr);
            }
        }
    }

    fn note_on(&mut self, key: u8, velocity: f32, id: NoteId) {
        let Some(zone_index) = self.zones.iter().position(|z| z.matches(key, velocity)) else {
            return;
        };
        let zone = &self.zones[zone_index];
        if zone.sample.is_empty() {
            return;
        }
        let mut semis = self.params.get(TUNE);
        if !self.one_shot() {
            semis += key as f64 - zone.root_key as f64;
        }
        let rate = zone.sample.sample_rate / self.sample_rate * (semis / 12.0).exp2();
        let sens = self.params.get_f32(VELOCITY);
        let gain = db_to_gain(self.params.get(GAIN) + zone.gain_db as f64) as f32
            * (1.0 - sens + sens * velocity);

        // Prefer a free voice, then the oldest one.
        let slot = self
            .voices
            .iter()
            .position(|v| !v.active)
            .unwrap_or_else(|| {
                let (i, _) = self
                    .voices
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, v)| v.age)
                    .expect("voices");
                i
            });
        self.clock += 1;
        let (a, r) = (self.params.get(ATTACK), self.params.get(RELEASE));
        let v = &mut self.voices[slot];
        *v = Voice {
            active: true,
            zone: zone_index,
            key,
            id,
            pos: 0.0,
            rate,
            gain,
            age: self.clock,
            env: Adsr::default(),
        };
        v.env.set(a, 1.0, 1.0, r, self.sample_rate);
        v.env.gate_on();
    }

    fn note_off(&mut self, key: u8, id: NoteId) {
        if self.one_shot() {
            return;
        }
        for v in self
            .voices
            .iter_mut()
            .filter(|v| v.active && !v.env.is_releasing())
        {
            if (id >= 0 && v.id == id) || (id < 0 && v.key == key) {
                v.env.gate_off();
            }
        }
    }
}

impl_device!(Sampler, INFO, PARAMS);

impl Segmented for Sampler {
    fn param_event(&mut self, param: ParamId, value: f64) {
        crate::Device::set_param(self, param, value);
    }

    fn note_event(&mut self, note: Note) {
        match note {
            Note::On { key, velocity, id } => self.note_on(key, velocity, id),
            Note::Off { key, id } => self.note_off(key, id),
            Note::AllOff => {
                for v in &mut self.voices {
                    v.env.gate_off();
                }
            }
            Note::Bend(_) => {}
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext, start: usize, end: usize) {
        for i in start..end {
            let (mut l, mut r) = (0.0, 0.0);
            for v in self.voices.iter_mut().filter(|v| v.active) {
                let sample = &self.zones[v.zone].sample;
                if v.pos >= sample.len() as f64 {
                    v.active = false;
                    continue;
                }
                let g = v.env.tick() * v.gain;
                if !v.env.is_active() {
                    v.active = false;
                    continue;
                }
                let left = &sample.channels[0];
                let right = sample.channels.get(1).unwrap_or(left);
                l += hermite(left, v.pos) * g;
                r += hermite(right, v.pos) * g;
                v.pos += v.rate;
            }
            output(ctx, i, l, r);
        }
    }
}

impl Node for Sampler {
    fn ports(&self) -> PortLayout {
        PortLayout {
            audio_in: 0,
            audio_out: 2,
            event_in: true,
            event_out: false,
        }
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: u32) {
        self.sample_rate = sample_rate;
        self.reset();
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        process_segmented(self, ctx);
    }

    fn reset(&mut self) {
        for v in &mut self.voices {
            v.active = false;
            v.env.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hermite_passes_through_samples() {
        let d = [0.0, 1.0, 4.0, 9.0, 16.0];
        assert_eq!(hermite(&d, 2.0), 4.0);
        assert!((hermite(&d, 2.5) - 6.25).abs() < 0.01);
    }

    #[test]
    fn zone_selection_by_key_and_velocity() {
        let s = Arc::new(SampleBuffer::mono(vec![1.0; 10], 48000.0));
        let mut z = Zone::full(s).keys(36, 47, 36);
        z.velocity_lo = 0.5;
        assert!(z.matches(40, 0.7));
        assert!(!z.matches(48, 0.7));
        assert!(!z.matches(40, 0.2));
    }
}
