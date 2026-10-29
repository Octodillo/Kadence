//! Polyphonic subtractive synth: two band-limited oscillators, a
//! state-variable filter, and amp and filter ADSR envelopes. When the voice
//! limit is reached the quietest releasing voice (else the oldest) is
//! stolen; it fades out over a few milliseconds instead of cutting off.

use daw_core::event::{NoteId, ParamId};
use daw_core::node::{Node, PortLayout, ProcessContext};

use crate::dsp::{Adsr, Smoother, Svf, SvfMode, Waveform, db_to_gain, key_to_hz};
use crate::events::{Note, Segmented, output, process_segmented};
use crate::param::{ParamSpec, ParamStore, ParamUnit};
use crate::{DeviceCategory, DeviceInfo, impl_device};

pub static INFO: DeviceInfo = DeviceInfo {
    id: "daw.synth",
    name: "Subtractive Synth",
    category: DeviceCategory::Instrument,
    description: "Two oscillators, a resonant filter and two envelopes",
};

pub const OSC1_WAVE: ParamId = 0;
pub const OSC2_WAVE: ParamId = 1;
pub const OSC2_SEMITONES: ParamId = 2;
pub const OSC2_DETUNE: ParamId = 3;
pub const OSC_MIX: ParamId = 4;
pub const FILTER_TYPE: ParamId = 5;
pub const CUTOFF: ParamId = 6;
pub const RESONANCE: ParamId = 7;
pub const FILTER_ENV_AMOUNT: ParamId = 8;
pub const KEY_TRACKING: ParamId = 9;
pub const AMP_ATTACK: ParamId = 10;
pub const AMP_DECAY: ParamId = 11;
pub const AMP_SUSTAIN: ParamId = 12;
pub const AMP_RELEASE: ParamId = 13;
pub const FILTER_ATTACK: ParamId = 14;
pub const FILTER_DECAY: ParamId = 15;
pub const FILTER_SUSTAIN: ParamId = 16;
pub const FILTER_RELEASE: ParamId = 17;
pub const VELOCITY: ParamId = 18;
pub const GAIN: ParamId = 19;
pub const POLYPHONY: ParamId = 20;
pub const BEND_RANGE: ParamId = 21;

const FILTER_TYPES: &[&str] = &["Low-pass", "High-pass", "Band-pass"];

static PARAMS: &[ParamSpec] = &[
    ParamSpec::choice(OSC1_WAVE, "osc1_wave", "Osc 1 Wave", Waveform::LABELS, 0),
    ParamSpec::choice(OSC2_WAVE, "osc2_wave", "Osc 2 Wave", Waveform::LABELS, 0),
    ParamSpec::new(
        OSC2_SEMITONES,
        "osc2_semitones",
        "Osc 2 Pitch",
        -24.0,
        24.0,
        0.0,
        ParamUnit::Semitones,
    )
    .integer(),
    ParamSpec::new(
        OSC2_DETUNE,
        "osc2_detune",
        "Osc 2 Detune",
        -100.0,
        100.0,
        7.0,
        ParamUnit::Cents,
    ),
    ParamSpec::new(
        OSC_MIX,
        "osc_mix",
        "Osc Mix",
        0.0,
        1.0,
        0.5,
        ParamUnit::Percent,
    ),
    ParamSpec::choice(FILTER_TYPE, "filter_type", "Filter Type", FILTER_TYPES, 0),
    ParamSpec::new(
        CUTOFF,
        "cutoff",
        "Cutoff",
        20.0,
        20000.0,
        2000.0,
        ParamUnit::Hertz,
    )
    .log(),
    ParamSpec::new(
        RESONANCE,
        "resonance",
        "Resonance",
        0.0,
        1.0,
        0.2,
        ParamUnit::Percent,
    ),
    ParamSpec::new(
        FILTER_ENV_AMOUNT,
        "filter_env_amount",
        "Filter Env Amount",
        -8.0,
        8.0,
        2.0,
        ParamUnit::Octaves,
    ),
    ParamSpec::new(
        KEY_TRACKING,
        "key_tracking",
        "Key Tracking",
        0.0,
        1.0,
        0.5,
        ParamUnit::Percent,
    ),
    ParamSpec::new(
        AMP_ATTACK,
        "amp_attack",
        "Amp Attack",
        0.0,
        10000.0,
        5.0,
        ParamUnit::Milliseconds,
    ),
    ParamSpec::new(
        AMP_DECAY,
        "amp_decay",
        "Amp Decay",
        1.0,
        10000.0,
        300.0,
        ParamUnit::Milliseconds,
    )
    .log(),
    ParamSpec::new(
        AMP_SUSTAIN,
        "amp_sustain",
        "Amp Sustain",
        0.0,
        1.0,
        0.8,
        ParamUnit::Percent,
    ),
    ParamSpec::new(
        AMP_RELEASE,
        "amp_release",
        "Amp Release",
        1.0,
        20000.0,
        300.0,
        ParamUnit::Milliseconds,
    )
    .log(),
    ParamSpec::new(
        FILTER_ATTACK,
        "filter_attack",
        "Filter Attack",
        0.0,
        10000.0,
        1.0,
        ParamUnit::Milliseconds,
    ),
    ParamSpec::new(
        FILTER_DECAY,
        "filter_decay",
        "Filter Decay",
        1.0,
        10000.0,
        400.0,
        ParamUnit::Milliseconds,
    )
    .log(),
    ParamSpec::new(
        FILTER_SUSTAIN,
        "filter_sustain",
        "Filter Sustain",
        0.0,
        1.0,
        0.3,
        ParamUnit::Percent,
    ),
    ParamSpec::new(
        FILTER_RELEASE,
        "filter_release",
        "Filter Release",
        1.0,
        20000.0,
        400.0,
        ParamUnit::Milliseconds,
    )
    .log(),
    ParamSpec::new(
        VELOCITY,
        "velocity",
        "Velocity Sensitivity",
        0.0,
        1.0,
        0.7,
        ParamUnit::Percent,
    ),
    ParamSpec::new(GAIN, "gain", "Gain", -70.0, 12.0, -6.0, ParamUnit::Decibels),
    ParamSpec::new(
        POLYPHONY,
        "polyphony",
        "Voices",
        1.0,
        MAX_POLYPHONY as f64,
        16.0,
        ParamUnit::None,
    )
    .integer(),
    ParamSpec::new(
        BEND_RANGE,
        "bend_range",
        "Pitch Bend Range",
        0.0,
        24.0,
        2.0,
        ParamUnit::Semitones,
    )
    .integer(),
];

/// Highest value of the polyphony parameter.
pub const MAX_POLYPHONY: usize = 32;
/// Extra voices so stolen voices can fade out while their replacement starts.
const SPARE_VOICES: usize = 8;
const POOL: usize = MAX_POLYPHONY + SPARE_VOICES;
/// Fade-out time for a stolen voice.
const STEAL_FADE_MS: f64 = 3.0;
/// Headroom so a handful of full-scale voices don't clip.
const VOICE_GAIN: f32 = 0.25;

#[derive(Clone, Copy, Default)]
struct Voice {
    active: bool,
    /// Being faded out after a steal; doesn't count towards polyphony.
    stolen: bool,
    fade: f32,
    key: u8,
    id: NoteId,
    velocity_gain: f32,
    age: u64,
    phase1: f32,
    phase2: f32,
    amp_env: Adsr,
    filter_env: Adsr,
    filter: Svf,
}

pub struct Synth {
    params: ParamStore,
    voices: [Voice; POOL],
    sample_rate: f64,
    clock: u64,
    bend: f32,
    fade_step: f32,
    gain: Smoother,
}

impl Default for Synth {
    fn default() -> Self {
        Self::new()
    }
}

impl Synth {
    pub fn new() -> Self {
        let mut s = Self {
            params: ParamStore::new(PARAMS),
            voices: [Voice::default(); POOL],
            sample_rate: 48000.0,
            clock: 0,
            bend: 0.0,
            fade_step: 0.0,
            gain: Smoother::new(0.0),
        };
        s.prepare(48000.0, 0);
        s
    }

    /// Voices currently sounding, not counting stolen ones fading out.
    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active && !v.stolen).count()
    }

    fn polyphony(&self) -> usize {
        self.params.get_index(POLYPHONY).clamp(1, MAX_POLYPHONY)
    }

    fn apply_param(&mut self, id: ParamId) {
        match id {
            AMP_ATTACK..=FILTER_RELEASE => {
                for v in &mut self.voices {
                    set_envelopes(&self.params, self.sample_rate, v);
                }
            }
            GAIN => self
                .gain
                .set_target(db_to_gain(self.params.get(GAIN)) as f32),
            _ => {}
        }
    }

    fn steal(&mut self) {
        let victim = self
            .voices
            .iter()
            .enumerate()
            .filter(|(_, v)| v.active && !v.stolen)
            .min_by(|(_, a), (_, b)| {
                // Releasing voices first, quietest first; otherwise oldest.
                b.amp_env
                    .is_releasing()
                    .cmp(&a.amp_env.is_releasing())
                    .then(if a.amp_env.is_releasing() {
                        a.amp_env.level().total_cmp(&b.amp_env.level())
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

    fn note_on(&mut self, key: u8, velocity: f32, id: NoteId) {
        // Retriggering a held key reuses its voice so envelopes continue smoothly.
        let existing = self
            .voices
            .iter()
            .position(|v| v.active && !v.stolen && v.key == key);
        let slot = match existing {
            Some(i) => i,
            None => {
                while self.active_voices() >= self.polyphony() {
                    self.steal();
                }
                match self.voices.iter().position(|v| !v.active) {
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
                }
            }
        };
        self.clock += 1;
        let sens = self.params.get_f32(VELOCITY);
        let mut v = if existing.is_some() {
            self.voices[slot]
        } else {
            Voice {
                phase2: 0.25,
                ..Voice::default()
            }
        };
        v.active = true;
        v.stolen = false;
        v.key = key;
        v.id = id;
        v.age = self.clock;
        v.velocity_gain = 1.0 - sens + sens * velocity;
        set_envelopes(&self.params, self.sample_rate, &mut v);
        v.amp_env.gate_on();
        v.filter_env.gate_on();
        self.voices[slot] = v;
    }

    fn note_off(&mut self, key: u8, id: NoteId) {
        for v in self.voices.iter_mut().filter(|v| v.active && !v.stolen) {
            // Match by id when both sides have one, otherwise by key, so an
            // id-less (raw MIDI) note still ends on an off that has an id.
            if (id >= 0 && v.id == id) || ((id < 0 || v.id < 0) && v.key == key) {
                v.amp_env.gate_off();
                v.filter_env.gate_off();
            }
        }
    }
}

fn set_envelopes(p: &ParamStore, sr: f64, v: &mut Voice) {
    v.amp_env.set(
        p.get(AMP_ATTACK),
        p.get(AMP_DECAY),
        p.get(AMP_SUSTAIN),
        p.get(AMP_RELEASE),
        sr,
    );
    v.filter_env.set(
        p.get(FILTER_ATTACK),
        p.get(FILTER_DECAY),
        p.get(FILTER_SUSTAIN),
        p.get(FILTER_RELEASE),
        sr,
    );
}

impl_device!(Synth, INFO, PARAMS);

impl Segmented for Synth {
    fn param_event(&mut self, param: ParamId, value: f64) {
        crate::Device::set_param(self, param, value);
    }

    fn note_event(&mut self, note: Note) {
        match note {
            Note::On { key, velocity, id } => self.note_on(key, velocity, id),
            Note::Off { key, id } => self.note_off(key, id),
            Note::AllOff => {
                for v in &mut self.voices {
                    v.amp_env.gate_off();
                    v.filter_env.gate_off();
                }
            }
            Note::Bend(b) => self.bend = b,
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext, start: usize, end: usize) {
        let p = &self.params;
        let sr = self.sample_rate as f32;
        let wave1 = Waveform::from_index(p.get_index(OSC1_WAVE));
        let wave2 = Waveform::from_index(p.get_index(OSC2_WAVE));
        let osc2_ratio =
            ((p.get(OSC2_SEMITONES) + p.get(OSC2_DETUNE) / 100.0) / 12.0).exp2() as f32;
        let mix = p.get_f32(OSC_MIX);
        let mode = match p.get_index(FILTER_TYPE) {
            0 => SvfMode::LowPass,
            1 => SvfMode::HighPass,
            _ => SvfMode::BandPass,
        };
        let cutoff = p.get_f32(CUTOFF);
        let resonance = p.get_f32(RESONANCE);
        let env_amount = p.get_f32(FILTER_ENV_AMOUNT);
        let tracking = p.get_f32(KEY_TRACKING);
        let bend = self.bend * p.get_f32(BEND_RANGE);
        let fade_step = self.fade_step;

        for i in start..end {
            let mut sum = 0.0;
            for v in self.voices.iter_mut().filter(|v| v.active) {
                let pitch = v.key as f32 + bend;
                let dt1 = (key_to_hz(pitch) / sr).min(0.5);
                let dt2 = (dt1 * osc2_ratio).min(0.5);
                let osc =
                    wave1.sample(v.phase1, dt1) * (1.0 - mix) + wave2.sample(v.phase2, dt2) * mix;
                v.phase1 = (v.phase1 + dt1).fract();
                v.phase2 = (v.phase2 + dt2).fract();

                let fenv = v.filter_env.tick();
                let octaves = env_amount * fenv + tracking * (v.key as f32 - 60.0) / 12.0;
                let fc = cutoff * octaves.exp2();
                let y = v.filter.process(osc, fc, resonance, sr, mode);

                let mut amp = v.amp_env.tick() * v.velocity_gain;
                if v.stolen {
                    v.fade -= fade_step;
                    if v.fade <= 0.0 {
                        v.active = false;
                        v.stolen = false;
                        v.amp_env.kill();
                        v.filter.reset();
                        continue;
                    }
                    amp *= v.fade;
                }
                if !v.amp_env.is_active() {
                    v.active = false;
                    v.filter.reset();
                    continue;
                }
                sum += y * amp;
            }
            let out = sum * VOICE_GAIN * self.gain.tick();
            output(ctx, i, out, out);
        }
    }
}

impl Node for Synth {
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
        self.gain.set_time(10.0, sample_rate);
        self.gain.reset(db_to_gain(self.params.get(GAIN)) as f32);
        self.reset();
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        process_segmented(self, ctx);
    }

    fn reset(&mut self) {
        self.voices = [Voice::default(); POOL];
        self.bend = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Device;

    #[test]
    fn steals_oldest_held_voice_and_releasing_voices_first() {
        let mut s = Synth::new();
        s.set_param(POLYPHONY, 4.0);
        for k in 60..64 {
            s.note_on(k, 1.0, -1);
        }
        assert_eq!(s.active_voices(), 4);
        // Release key 62; it should be stolen before older held keys.
        s.note_off(62, -1);
        s.note_on(70, 1.0, -1);
        let held: Vec<u8> = s
            .voices
            .iter()
            .filter(|v| v.active && !v.stolen)
            .map(|v| v.key)
            .collect();
        assert_eq!(held.len(), 4);
        assert!(!held.contains(&62));
        // No releasing voices left: the oldest (60) goes next.
        s.note_on(71, 1.0, -1);
        let held: Vec<u8> = s
            .voices
            .iter()
            .filter(|v| v.active && !v.stolen)
            .map(|v| v.key)
            .collect();
        assert!(!held.contains(&60));
        assert!(held.contains(&71));
    }

    #[test]
    fn many_notes_never_exceed_the_pool() {
        let mut s = Synth::new();
        s.set_param(POLYPHONY, MAX_POLYPHONY as f64);
        for k in 0..128u8 {
            s.note_on(k, 0.8, k as NoteId);
        }
        assert_eq!(s.active_voices(), MAX_POLYPHONY);
    }

    #[test]
    fn retrigger_reuses_voice() {
        let mut s = Synth::new();
        s.note_on(60, 1.0, -1);
        s.note_on(60, 1.0, -1);
        assert_eq!(s.active_voices(), 1);
    }
}
