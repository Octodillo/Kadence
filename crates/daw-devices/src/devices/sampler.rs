//! Sampler: plays in-memory sample buffers, either one-shot (drum style,
//! unpitched, ignores note-off) or key-mapped (pitched from a root key, with
//! a release on note-off). Zones choose a sample by key and velocity. When
//! every voice is busy the quietest releasing voice (else the oldest) is
//! stolen; it fades out over a few milliseconds instead of cutting off.
//!
//! Saved state holds the zone map with each sample's source path; loading it
//! decodes the files again (see [`Sampler::load_state_with`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use daw_core::event::{NoteId, ParamId};
use daw_core::node::{Node, PortLayout, ProcessContext};

use crate::dsp::{Adsr, db_to_gain};
use crate::events::{Note, Segmented, output, process_segmented};
use crate::param::{ParamSpec, ParamStore, ParamUnit};
use crate::state::{self, StateError};
use crate::{DeviceCategory, DeviceInfo, impl_device};
use serde::{Deserialize, Serialize};

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
/// Extra voices so stolen voices can fade out while their replacement starts.
const SPARE_VOICES: usize = 8;
const POOL: usize = VOICES + SPARE_VOICES;
/// Fade-out time for a stolen voice.
const STEAL_FADE_MS: f64 = 3.0;

/// Decoded audio held in memory. Mono or stereo; extra channels are ignored.
#[derive(Clone, Debug, Default)]
pub struct SampleBuffer {
    pub channels: Vec<Vec<f32>>,
    pub sample_rate: f64,
    /// The file the audio came from. The sampler saves it with the zone map so
    /// the sample can be loaded again; a buffer without one is saved as a
    /// silent placeholder.
    pub source: Option<PathBuf>,
}

impl SampleBuffer {
    pub fn mono(data: Vec<f32>, sample_rate: f64) -> Self {
        Self {
            channels: vec![data],
            sample_rate,
            source: None,
        }
    }

    /// An empty buffer standing in for a sample whose file could not be
    /// loaded. It keeps the path so saving again doesn't lose the reference.
    pub fn missing(source: impl Into<PathBuf>) -> Self {
        Self {
            source: Some(source.into()),
            ..Self::default()
        }
    }

    /// Decode an audio file (WAV, AIFF, FLAC, OGG Vorbis, MP3) at its own rate. The
    /// sampler corrects for the rate difference when it plays. Not real-time safe.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, daw_media::MediaError> {
        let path = path.as_ref();
        daw_media::decode_file(path).map(|d| Self::from(d).with_source(path))
    }

    /// Decode an audio file and resample it to `sample_rate` up front, which avoids
    /// the sampler's interpolation for unpitched one-shots. Not real-time safe.
    pub fn load_resampled(
        path: impl AsRef<Path>,
        sample_rate: u32,
    ) -> Result<Self, daw_media::MediaError> {
        let path = path.as_ref();
        daw_media::load_file(path, sample_rate).map(|d| Self::from(d).with_source(path))
    }

    /// Record the file this audio came from.
    pub fn with_source(mut self, source: impl Into<PathBuf>) -> Self {
        self.source = Some(source.into());
        self
    }

    pub fn len(&self) -> usize {
        self.channels.iter().map(Vec::len).min().unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl From<daw_media::AudioData> for SampleBuffer {
    fn from(data: daw_media::AudioData) -> Self {
        Self {
            channels: data.channels,
            sample_rate: f64::from(data.sample_rate),
            source: None,
        }
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

    /// Clamp keys to MIDI range and velocities to `0..=1`, and replace
    /// non-finite numbers, so the zone saves and plays predictably.
    fn sanitized(mut self) -> Self {
        let unit = |v: f32, default: f32| {
            if v.is_finite() {
                v.clamp(0.0, 1.0)
            } else {
                default
            }
        };
        self.root_key = self.root_key.min(127);
        self.key_lo = self.key_lo.min(127);
        self.key_hi = self.key_hi.min(127);
        self.velocity_lo = unit(self.velocity_lo, 0.0);
        self.velocity_hi = unit(self.velocity_hi, 1.0);
        if !self.gain_db.is_finite() {
            self.gain_db = 0.0;
        }
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
    /// Being faded out after a steal; doesn't count towards the voice limit.
    stolen: bool,
    fade: f32,
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
    voices: [Voice; POOL],
    sample_rate: f64,
    fade_step: f32,
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
            voices: [Voice::default(); POOL],
            sample_rate: 48000.0,
            fade_step: (1000.0 / (STEAL_FADE_MS * 48000.0)) as f32,
            clock: 0,
        }
    }

    /// Replace the zone map. Call off the real-time thread; stops all voices.
    pub fn set_zones(&mut self, zones: Vec<Zone>) {
        self.voices = [Voice::default(); POOL];
        self.zones = zones.into_iter().map(Zone::sanitized).collect();
    }

    pub fn zones(&self) -> &[Zone] {
        &self.zones
    }

    /// Voices currently sounding, not counting stolen ones fading out.
    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active && !v.stolen).count()
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

        if self.active_voices() >= VOICES {
            self.steal();
        }
        let slot = match self.voices.iter().position(|v| !v.active) {
            Some(i) => i,
            // Every spare is still fading: cut the quietest fade short.
            None => self
                .voices
                .iter()
                .enumerate()
                .filter(|(_, v)| v.stolen)
                .min_by(|(_, a), (_, b)| a.fade.total_cmp(&b.fade))
                .map(|(i, _)| i)
                .expect("a stolen voice exists when the pool is full"),
        };
        self.clock += 1;
        let (a, r) = (self.params.get(ATTACK), self.params.get(RELEASE));
        let v = &mut self.voices[slot];
        *v = Voice {
            active: true,
            stolen: false,
            fade: 1.0,
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

    /// Start fading out the quietest releasing voice, else the oldest one.
    fn steal(&mut self) {
        let victim = self
            .voices
            .iter()
            .enumerate()
            .filter(|(_, v)| v.active && !v.stolen)
            .min_by(|(_, a), (_, b)| {
                b.env
                    .is_releasing()
                    .cmp(&a.env.is_releasing())
                    .then(if a.env.is_releasing() {
                        (a.env.level() * a.gain).total_cmp(&(b.env.level() * b.gain))
                    } else {
                        a.age.cmp(&b.age)
                    })
            })
            .map(|(i, _)| i);
        if let Some(i) = victim {
            let v = &mut self.voices[i];
            v.stolen = true;
            v.fade = 1.0;
        }
    }

    fn note_off(&mut self, key: u8, id: NoteId) {
        if self.one_shot() {
            return;
        }
        for v in self
            .voices
            .iter_mut()
            .filter(|v| v.active && !v.stolen && !v.env.is_releasing())
        {
            // Match by id when both sides have one, otherwise by key, so an
            // id-less (raw MIDI) note still ends on an off that has an id.
            if (id >= 0 && v.id == id) || ((id < 0 || v.id < 0) && v.key == key) {
                v.env.gate_off();
            }
        }
    }

    /// Restore state saved by [`Device::save_state`](crate::Device::save_state),
    /// getting each zone's sample from `resolve`. It is called once per distinct
    /// source path, so callers can resolve paths relative to a project and
    /// share buffers. A sample it returns `None` for becomes
    /// [`SampleBuffer::missing`]: the zone keeps its mapping but stays silent.
    ///
    /// State without a zone map (from before zones were saved) keeps the
    /// current zones. Nothing changes if `data` is invalid. Not real-time safe.
    pub fn load_state_with(
        &mut self,
        data: &[u8],
        mut resolve: impl FnMut(&Path) -> Option<Arc<SampleBuffer>>,
    ) -> Result<(), StateError> {
        let (values, extra) = state::load_with(&INFO, PARAMS, data)?;
        let saved: Option<SavedZones> = extra.map(serde_json::from_value).transpose()?;
        let zones = saved.map(|saved| {
            let mut cache: HashMap<PathBuf, Arc<SampleBuffer>> = HashMap::new();
            saved
                .zones
                .into_iter()
                .map(|z| {
                    let sample = match z.sample {
                        Some(path) => cache
                            .entry(path)
                            .or_insert_with_key(|path| {
                                resolve(path)
                                    .unwrap_or_else(|| Arc::new(SampleBuffer::missing(path)))
                            })
                            .clone(),
                        None => Arc::new(SampleBuffer::default()),
                    };
                    Zone {
                        sample,
                        root_key: z.root_key,
                        key_lo: z.key_lo,
                        key_hi: z.key_hi,
                        velocity_lo: z.velocity_lo,
                        velocity_hi: z.velocity_hi,
                        gain_db: z.gain_db,
                    }
                })
                .collect()
        });
        for (id, value) in values.into_iter().enumerate() {
            crate::Device::set_param(self, id as ParamId, value);
        }
        if let Some(zones) = zones {
            self.set_zones(zones);
        }
        Ok(())
    }

    fn saved_zones(&self) -> serde_json::Value {
        let zones = self
            .zones
            .iter()
            .map(|z| SavedZone {
                sample: z.sample.source.clone(),
                root_key: z.root_key,
                key_lo: z.key_lo,
                key_hi: z.key_hi,
                velocity_lo: z.velocity_lo,
                velocity_hi: z.velocity_hi,
                gain_db: z.gain_db,
            })
            .collect();
        serde_json::to_value(SavedZones { zones }).expect("serializing plain data cannot fail")
    }
}

/// The zone map in saved state. Sample audio is referenced by path.
#[derive(Serialize, Deserialize)]
struct SavedZones {
    zones: Vec<SavedZone>,
}

#[derive(Serialize, Deserialize)]
struct SavedZone {
    /// Absent for a zone whose buffer had no source file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sample: Option<PathBuf>,
    root_key: u8,
    key_lo: u8,
    key_hi: u8,
    velocity_lo: f32,
    velocity_hi: f32,
    gain_db: f32,
}

impl_device!(Sampler, INFO, PARAMS, {
    fn save_state(&self) -> Vec<u8> {
        state::save_with(
            &INFO,
            PARAMS,
            |id| self.params.get(id),
            Some(self.saved_zones()),
        )
    }

    /// Decodes each zone's sample from its saved path; see
    /// [`Sampler::load_state_with`] to resolve paths another way.
    fn load_state(&mut self, data: &[u8]) -> Result<(), StateError> {
        self.load_state_with(data, |path| SampleBuffer::load(path).ok().map(Arc::new))
    }
});

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
        let fade_step = self.fade_step;
        for i in start..end {
            let (mut l, mut r) = (0.0, 0.0);
            for v in self.voices.iter_mut().filter(|v| v.active) {
                let sample = &self.zones[v.zone].sample;
                if v.pos >= sample.len() as f64 {
                    v.active = false;
                    v.stolen = false;
                    continue;
                }
                let mut g = v.env.tick() * v.gain;
                if v.stolen {
                    v.fade -= fade_step;
                    if v.fade <= 0.0 {
                        v.active = false;
                        v.stolen = false;
                        v.env.kill();
                        continue;
                    }
                    g *= v.fade;
                }
                if !v.env.is_active() {
                    v.active = false;
                    v.stolen = false;
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
        self.fade_step = (1000.0 / (STEAL_FADE_MS * sample_rate)) as f32;
        self.reset();
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        process_segmented(self, ctx);
    }

    fn reset(&mut self) {
        for v in &mut self.voices {
            v.active = false;
            v.stolen = false;
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
    fn steals_releasing_voices_before_the_oldest_held_one() {
        let mut s = Sampler::new();
        s.set_zones(vec![Zone::full(Arc::new(SampleBuffer::mono(
            vec![0.5; 48000],
            48000.0,
        )))]);
        for k in 0..VOICES as u8 {
            s.note_on(k, 1.0, -1);
        }
        s.note_off(5, -1);
        s.note_on(100, 1.0, -1);
        let held = |s: &Sampler| -> Vec<u8> {
            s.voices
                .iter()
                .filter(|v| v.active && !v.stolen)
                .map(|v| v.key)
                .collect()
        };
        assert_eq!(s.active_voices(), VOICES);
        assert!(!held(&s).contains(&5));
        assert!(held(&s).contains(&0));
        // No releasing voices left: the oldest (0) goes next.
        s.note_on(101, 1.0, -1);
        assert!(!held(&s).contains(&0));
        assert!(held(&s).contains(&101));
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
