//! Metronome click that follows the transport.

use daw_core::event::ParamId;
use daw_core::node::{Node, PortLayout, ProcessContext, TransportState};
use daw_core::time::{MusicalPosition, Timeline};

use crate::dsp::db_to_gain;
use crate::events::{Segmented, output, process_segmented};
use crate::param::{ParamSpec, ParamStore, ParamUnit};
use crate::{DeviceCategory, DeviceInfo, impl_device};

pub static INFO: DeviceInfo = DeviceInfo {
    id: "daw.metronome",
    name: "Metronome",
    category: DeviceCategory::Utility,
    description: "Click on every beat of the time signature while the transport plays",
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
    // 0 follows the project's time signature; 1 to 16 accent every that
    // many beats from each downbeat instead.
    ParamSpec::new(
        BEATS_PER_BAR,
        "beats_per_bar",
        "Beats per Bar",
        0.0,
        16.0,
        0.0,
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
/// Quarter notes (a few nanoseconds at any sane tempo) by which a position
/// may fall short of a beat and still count as on it.
const BEAT_EPSILON: f64 = 1e-9;
/// Click decay time constant.
const DECAY_MS: f64 = 12.0;

pub struct Metronome {
    params: ParamStore,
    sample_rate: f64,
    decay: f32,
    phase: f32,
    step: f32,
    env: f32,
    /// The latest beat clicked, as (bar, beat in bar). A click sounds when
    /// the beat moves past it, so a block that ends just before a beat and
    /// a next block that starts just after it still click once.
    last_beat: Option<(i64, i64)>,
    /// Where the next sample is expected while playing; anything else is a
    /// locate or loop jump.
    expected: Option<i64>,
    /// 120 bpm in 4/4, for a transport that has no tempo.
    fallback: Timeline,
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
            last_beat: None,
            expected: None,
            fallback: Timeline::default(),
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
        let mut t = *ctx.transport;
        if t.sample_rate <= 0.0 {
            t.sample_rate = self.sample_rate;
        }
        if t.bpm <= 0.0 {
            t = TransportState::at(&self.fallback, t.playing, t.position, t.sample_rate);
        }
        let volume = db_to_gain(self.params.get(VOLUME)) as f32;
        let group = self.params.get(BEATS_PER_BAR).round() as i64;
        let accent = self.params.get_bool(ACCENT);
        let per_sample = t.beats_per_sample();
        for i in start..end {
            let pos = t.position.0 + i as i64;
            if !t.playing {
                self.last_beat = None;
                self.expected = None;
            } else {
                let beats = t.music.beats + i as f64 * per_sample;
                let now = beat_at(&t.music, beats);
                if self.expected != Some(pos) {
                    // After a jump only a beat that starts on this very
                    // sample clicks.
                    self.last_beat = Some(beat_at(&t.music, beats - per_sample));
                }
                if self.last_beat.is_none_or(|last| now > last) {
                    let downbeat = if group > 0 {
                        now.1 % group == 0
                    } else {
                        now.1 == 0
                    };
                    self.trigger(accent && downbeat);
                    self.last_beat = Some(now);
                }
                self.expected = Some(pos + 1);
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

/// The beat that `beats` (quarter notes) falls in, as (bar, beat in bar), with
/// `m` the position at the start of the block. Bars after the block's first
/// keep its time signature, which only matters for a block that crosses a
/// time-signature change.
fn beat_at(m: &MusicalPosition, beats: f64) -> (i64, i64) {
    let beat_len = m.beat_len();
    let full = f64::from(m.numerator.max(1)) * beat_len;
    // Rounding in the tempo map can leave a position a hair before the
    // beat it is on, which would click a sample late.
    let into = beats - m.bar_start + BEAT_EPSILON;
    let (bar, into) = if into >= m.bar_len && m.bar_len > 0.0 {
        let over = into - m.bar_len;
        let more = (over / full).floor();
        (m.bar + 1 + more as i64, over - more * full)
    } else if into < 0.0 {
        let back = (-into / full).ceil();
        (m.bar - back as i64, into + back * full)
    } else {
        (m.bar, into)
    };
    (bar, (into / beat_len).floor() as i64)
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
        self.reset();
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        process_segmented(self, ctx);
    }

    fn reset(&mut self) {
        self.env = 0.0;
        self.last_beat = None;
        self.expected = None;
    }
}
