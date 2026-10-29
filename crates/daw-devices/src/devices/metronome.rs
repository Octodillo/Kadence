//! Metronome click that follows the transport.

use daw_core::event::ParamId;
use daw_core::node::{Node, PortLayout, ProcessContext};

use crate::dsp::db_to_gain;
use crate::events::{Segmented, output, process_segmented};
use crate::param::{ParamSpec, ParamStore, ParamUnit};
use crate::{DeviceCategory, DeviceInfo, impl_device};

pub static INFO: DeviceInfo = DeviceInfo {
    id: "daw.metronome",
    name: "Metronome",
    category: DeviceCategory::Utility,
    description: "Click on every beat while the transport plays",
};

pub const VOLUME: ParamId = 0;
pub const BEATS_PER_BAR: ParamId = 1;
pub const ACCENT: ParamId = 2;
pub const PITCH: ParamId = 3;

static PARAMS: &[ParamSpec] = &[
    ParamSpec::new(
        VOLUME,
        "volume",
        "Volume",
        -70.0,
        6.0,
        -6.0,
        ParamUnit::Decibels,
    ),
    ParamSpec::new(
        BEATS_PER_BAR,
        "beats_per_bar",
        "Beats per Bar",
        1.0,
        16.0,
        4.0,
        ParamUnit::None,
    )
    .integer(),
    ParamSpec::toggle(ACCENT, "accent", "Accent First Beat", true),
    ParamSpec::new(
        PITCH,
        "pitch",
        "Pitch",
        200.0,
        4000.0,
        1000.0,
        ParamUnit::Hertz,
    )
    .log(),
];

/// Accented clicks are this much higher and louder.
const ACCENT_PITCH: f32 = 1.5;
const ACCENT_GAIN: f32 = 1.0;
const BEAT_GAIN: f32 = 0.6;
/// Click decay time constant.
const DECAY_MS: f64 = 12.0;

pub struct Metronome {
    params: ParamStore,
    sample_rate: f64,
    decay: f32,
    phase: f32,
    step: f32,
    env: f32,
}

impl Default for Metronome {
    fn default() -> Self {
        Self::new()
    }
}

impl Metronome {
    pub fn new() -> Self {
        let mut m = Self {
            params: ParamStore::new(PARAMS),
            sample_rate: 48000.0,
            decay: 0.0,
            phase: 0.0,
            step: 0.0,
            env: 0.0,
        };
        m.prepare(48000.0, 0);
        m
    }

    fn apply_param(&mut self, _id: ParamId) {}

    fn trigger(&mut self, accent: bool) {
        let mut freq = self.params.get_f32(PITCH);
        let mut gain = BEAT_GAIN;
        if accent {
            freq *= ACCENT_PITCH;
            gain = ACCENT_GAIN;
        }
        self.phase = 0.0;
        self.step = freq / self.sample_rate as f32;
        self.env = gain;
    }
}

impl_device!(Metronome, INFO, PARAMS);

impl Segmented for Metronome {
    fn param_event(&mut self, param: ParamId, value: f64) {
        crate::Device::set_param(self, param, value);
    }

    fn render(&mut self, ctx: &mut ProcessContext, start: usize, end: usize) {
        let t = *ctx.transport;
        let volume = db_to_gain(self.params.get(VOLUME)) as f32;
        let bpb = self.params.get(BEATS_PER_BAR).max(1.0) as i64;
        let accent = self.params.get_bool(ACCENT);
        let bpm = if t.bpm > 0.0 { t.bpm } else { 120.0 };
        let sample_rate = if t.sample_rate > 0.0 {
            t.sample_rate
        } else {
            self.sample_rate
        };
        let samples_per_beat = 60.0 * sample_rate / bpm;
        for i in start..end {
            if t.playing {
                let pos = t.position.0 + i as i64;
                if pos >= 0 {
                    let beat = (pos as f64 / samples_per_beat).floor() as i64;
                    let prev = ((pos - 1) as f64 / samples_per_beat).floor() as i64;
                    if beat != prev {
                        self.trigger(accent && beat.rem_euclid(bpb) == 0);
                    }
                }
            }
            let mut y = 0.0;
            if self.env > 0.0 {
                y = (std::f32::consts::TAU * self.phase).sin() * self.env * volume;
                self.phase = (self.phase + self.step).fract();
                self.env *= self.decay;
                if self.env < 1e-4 {
                    self.env = 0.0;
                }
            }
            output(ctx, i, y, y);
        }
    }
}

impl Node for Metronome {
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
        self.decay = (-1000.0 / (DECAY_MS * sample_rate)).exp() as f32;
        self.env = 0.0;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        process_segmented(self, ctx);
    }

    fn reset(&mut self) {
        self.env = 0.0;
    }
}
