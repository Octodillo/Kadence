//! Stereo delay, free-running or synced to the transport tempo, with
//! optional ping-pong and a high-cut filter in the feedback path.

use daw_core::event::ParamId;
use daw_core::node::{Node, PortLayout, ProcessContext};

use crate::dsp::{DelayLine, OnePole, Smoother, one_pole_lowpass_coef};
use crate::events::{Segmented, input, output, process_segmented};
use crate::param::{ParamSpec, ParamStore, ParamUnit};
use crate::{DeviceCategory, DeviceInfo, impl_device};

pub static INFO: DeviceInfo = DeviceInfo {
    id: "daw.delay",
    name: "Delay",
    category: DeviceCategory::AudioEffect,
    description: "Tempo-synced stereo delay with ping-pong",
};

pub const SYNC: ParamId = 0;
pub const DIVISION: ParamId = 1;
pub const TIME: ParamId = 2;
pub const FEEDBACK: ParamId = 3;
pub const MIX: ParamId = 4;
pub const PING_PONG: ParamId = 5;
pub const HIGH_CUT: ParamId = 6;

/// Note divisions and their length in beats (quarter notes).
pub const DIVISIONS: &[(&str, f64)] = &[
    ("1/1", 4.0),
    ("1/2", 2.0),
    ("1/4", 1.0),
    ("1/8", 0.5),
    ("1/16", 0.25),
    ("1/32", 0.125),
    ("1/2 dotted", 3.0),
    ("1/4 dotted", 1.5),
    ("1/8 dotted", 0.75),
    ("1/16 dotted", 0.375),
    ("1/2 triplet", 4.0 / 3.0),
    ("1/4 triplet", 2.0 / 3.0),
    ("1/8 triplet", 1.0 / 3.0),
    ("1/16 triplet", 1.0 / 6.0),
];

const DIVISION_LABELS: &[&str] = &[
    "1/1",
    "1/2",
    "1/4",
    "1/8",
    "1/16",
    "1/32",
    "1/2 dotted",
    "1/4 dotted",
    "1/8 dotted",
    "1/16 dotted",
    "1/2 triplet",
    "1/4 triplet",
    "1/8 triplet",
    "1/16 triplet",
];

static PARAMS: &[ParamSpec] = &[
    ParamSpec::toggle(SYNC, "sync", "Tempo Sync", true),
    ParamSpec::choice(DIVISION, "division", "Division", DIVISION_LABELS, 2),
    ParamSpec::new(
        TIME,
        "time",
        "Time",
        1.0,
        4000.0,
        375.0,
        ParamUnit::Milliseconds,
    )
    .log(),
    ParamSpec::new(
        FEEDBACK,
        "feedback",
        "Feedback",
        0.0,
        0.95,
        0.4,
        ParamUnit::Percent,
    ),
    ParamSpec::new(MIX, "mix", "Mix", 0.0, 1.0, 0.35, ParamUnit::Percent),
    ParamSpec::toggle(PING_PONG, "ping_pong", "Ping-pong", false),
    ParamSpec::new(
        HIGH_CUT,
        "high_cut",
        "High Cut",
        200.0,
        20000.0,
        12000.0,
        ParamUnit::Hertz,
    )
    .log(),
];

/// Longest delay we allocate for: a whole note at 30 BPM.
const MAX_SECONDS: f64 = 8.0;

pub struct Delay {
    params: ParamStore,
    sample_rate: f64,
    lines: [DelayLine; 2],
    damping: [OnePole; 2],
    damping_coef: f32,
    time: Smoother,
    /// Jump the delay time to the transport's tempo on the next render
    /// instead of gliding, after a reset or a state load.
    snap_time: bool,
    mix: Smoother,
    feedback: Smoother,
}

impl Default for Delay {
    fn default() -> Self {
        Self::new()
    }
}

impl Delay {
    pub fn new() -> Self {
        Self {
            params: ParamStore::new(PARAMS),
            sample_rate: 0.0,
            lines: [DelayLine::default(), DelayLine::default()],
            damping: [OnePole::default(); 2],
            damping_coef: 1.0,
            time: Smoother::new(0.0),
            snap_time: true,
            mix: Smoother::new(0.35),
            feedback: Smoother::new(0.4),
        }
    }

    /// Delay time in samples for the given tempo.
    pub fn delay_samples(&self, bpm: f64) -> f64 {
        let seconds = if self.params.get_bool(SYNC) {
            let beats = DIVISIONS[self.params.get_index(DIVISION)].1;
            let bpm = if bpm > 0.0 { bpm } else { 120.0 };
            beats * 60.0 / bpm
        } else {
            self.params.get(TIME) / 1000.0
        };
        (seconds * self.sample_rate).clamp(1.0, self.lines[0].max_delay().max(1) as f64)
    }

    fn apply_param(&mut self, id: ParamId) {
        match id {
            MIX => self.mix.set_target(self.params.get_f32(MIX)),
            FEEDBACK => self.feedback.set_target(self.params.get_f32(FEEDBACK)),
            HIGH_CUT => {
                self.damping_coef =
                    one_pole_lowpass_coef(self.params.get(HIGH_CUT), self.sample_rate)
            }
            _ => {}
        }
    }
}

impl_device!(Delay, INFO, PARAMS, {
    fn snap_smoothing(&mut self) {
        self.mix.reset(self.mix.target());
        self.feedback.reset(self.feedback.target());
        self.snap_time = true;
    }
});

impl Segmented for Delay {
    fn param_event(&mut self, param: ParamId, value: f64) {
        crate::Device::set_param(self, param, value);
    }

    fn render(&mut self, ctx: &mut ProcessContext, start: usize, end: usize) {
        let time = self.delay_samples(ctx.transport.bpm) as f32;
        if self.snap_time {
            self.time.reset(time);
            self.snap_time = false;
        } else {
            self.time.set_target(time);
        }
        let ping_pong = self.params.get_bool(PING_PONG);
        let coef = self.damping_coef;
        for i in start..end {
            let (l, r) = (input(ctx, 0, i), input(ctx, 1, i));
            // The line holds the newest sample at 0, so read d-1 before pushing.
            let d = self.time.tick() - 1.0;
            let wet_l = self.lines[0].read(d);
            let wet_r = self.lines[1].read(d);
            let fb = self.feedback.tick();
            let fb_l = self.damping[0].process(wet_l, coef) * fb;
            let fb_r = self.damping[1].process(wet_r, coef) * fb;
            if ping_pong {
                self.lines[0].push(0.5 * (l + r) + fb_r);
                self.lines[1].push(fb_l);
            } else {
                self.lines[0].push(l + fb_l);
                self.lines[1].push(r + fb_r);
            }
            let mix = self.mix.tick();
            output(
                ctx,
                i,
                l * (1.0 - mix) + wet_l * mix,
                r * (1.0 - mix) + wet_r * mix,
            );
        }
    }
}

impl Node for Delay {
    fn ports(&self) -> PortLayout {
        PortLayout {
            audio_in: 2,
            audio_out: 2,
            event_in: true,
            event_out: false,
        }
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: u32) {
        if sample_rate != self.sample_rate {
            let len = (MAX_SECONDS * sample_rate).ceil() as usize;
            self.lines = [DelayLine::new(len), DelayLine::new(len)];
        }
        self.sample_rate = sample_rate;
        self.damping_coef = one_pole_lowpass_coef(self.params.get(HIGH_CUT), sample_rate);
        self.time.set_time(50.0, sample_rate);
        self.mix.set_time(10.0, sample_rate);
        self.feedback.set_time(10.0, sample_rate);
        self.mix.reset(self.params.get_f32(MIX));
        self.feedback.reset(self.params.get_f32(FEEDBACK));
        self.reset();
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        process_segmented(self, ctx);
    }

    fn reset(&mut self) {
        for l in &mut self.lines {
            l.clear();
        }
        for d in &mut self.damping {
            d.reset();
        }
        // Start at the transport tempo's delay instead of gliding to it; the
        // tempo is only known once rendering starts.
        self.snap_time = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Device;

    #[test]
    fn division_lengths() {
        assert_eq!(DIVISIONS.len(), DIVISION_LABELS.len());
        for (d, l) in DIVISIONS.iter().zip(DIVISION_LABELS) {
            assert_eq!(d.0, *l);
        }
        let mut d = Delay::new();
        d.prepare(48000.0, 64);
        assert_eq!(d.delay_samples(120.0), 24000.0);
        d.set_param(DIVISION, 8.0); // 1/8 dotted
        assert!((d.delay_samples(100.0) - 21600.0).abs() < 1e-6);
        d.set_param(SYNC, 0.0);
        d.set_param(TIME, 10.0);
        assert_eq!(d.delay_samples(100.0), 480.0);
    }
}
