//! Feed-forward stereo-linked compressor with a soft knee, computed in the
//! log domain with a smooth branching peak detector.

use daw_core::event::ParamId;
use daw_core::node::{Node, PortLayout, ProcessContext};

use crate::dsp::{db_to_gain, flush};
use crate::events::{Segmented, input, output, process_segmented};
use crate::param::{ParamSpec, ParamStore, ParamUnit};
use crate::{DeviceCategory, DeviceInfo, impl_device};

pub static INFO: DeviceInfo = DeviceInfo {
    id: "daw.compressor",
    name: "Compressor",
    category: DeviceCategory::AudioEffect,
    description: "Soft-knee dynamic range compressor",
};

pub const THRESHOLD: ParamId = 0;
pub const RATIO: ParamId = 1;
pub const ATTACK: ParamId = 2;
pub const RELEASE: ParamId = 3;
pub const KNEE: ParamId = 4;
pub const MAKEUP: ParamId = 5;
pub const MIX: ParamId = 6;

static PARAMS: &[ParamSpec] = &[
    ParamSpec::new(
        THRESHOLD,
        "threshold",
        "Threshold",
        -60.0,
        0.0,
        -18.0,
        ParamUnit::Decibels,
    ),
    ParamSpec::new(RATIO, "ratio", "Ratio", 1.0, 20.0, 4.0, ParamUnit::Ratio).log(),
    ParamSpec::new(
        ATTACK,
        "attack",
        "Attack",
        0.1,
        200.0,
        10.0,
        ParamUnit::Milliseconds,
    )
    .log(),
    ParamSpec::new(
        RELEASE,
        "release",
        "Release",
        5.0,
        2000.0,
        100.0,
        ParamUnit::Milliseconds,
    )
    .log(),
    ParamSpec::new(KNEE, "knee", "Knee", 0.0, 24.0, 6.0, ParamUnit::Decibels),
    ParamSpec::new(
        MAKEUP,
        "makeup",
        "Makeup Gain",
        0.0,
        24.0,
        0.0,
        ParamUnit::Decibels,
    ),
    ParamSpec::new(MIX, "mix", "Mix", 0.0, 1.0, 1.0, ParamUnit::Percent),
];

pub struct Compressor {
    params: ParamStore,
    sample_rate: f64,
    attack: f32,
    release: f32,
    /// Smoothed gain reduction in dB (positive).
    envelope: f32,
}

impl Default for Compressor {
    fn default() -> Self {
        Self::new()
    }
}

/// Static gain curve: output level in dB for an input level in dB.
pub fn gain_computer(x: f32, threshold: f32, ratio: f32, knee: f32) -> f32 {
    let over = x - threshold;
    if knee > 0.0 && 2.0 * over.abs() <= knee {
        let t = over + knee / 2.0;
        x + (1.0 / ratio - 1.0) * t * t / (2.0 * knee)
    } else if over > 0.0 {
        threshold + over / ratio
    } else {
        x
    }
}

impl Compressor {
    pub fn new() -> Self {
        let mut c = Self {
            params: ParamStore::new(PARAMS),
            sample_rate: 48000.0,
            attack: 0.0,
            release: 0.0,
            envelope: 0.0,
        };
        c.prepare(48000.0, 0);
        c
    }

    /// Current gain reduction in dB, for metering.
    pub fn gain_reduction_db(&self) -> f32 {
        self.envelope
    }

    fn apply_param(&mut self, _id: ParamId) {
        let sr = self.sample_rate;
        self.attack = (-1000.0 / (self.params.get(ATTACK) * sr)).exp() as f32;
        self.release = (-1000.0 / (self.params.get(RELEASE) * sr)).exp() as f32;
    }
}

impl_device!(Compressor, INFO, PARAMS);

impl Segmented for Compressor {
    fn param_event(&mut self, param: ParamId, value: f64) {
        crate::Device::set_param(self, param, value);
    }

    fn render(&mut self, ctx: &mut ProcessContext, start: usize, end: usize) {
        let p = &self.params;
        let (threshold, ratio, knee) = (p.get_f32(THRESHOLD), p.get_f32(RATIO), p.get_f32(KNEE));
        let makeup = p.get(MAKEUP);
        let mix = p.get_f32(MIX);
        for i in start..end {
            let (l, r) = (input(ctx, 0, i), input(ctx, 1, i));
            let peak = l.abs().max(r.abs());
            let level = 20.0 * peak.max(1e-9).log10();
            let reduction = level - gain_computer(level, threshold, ratio, knee);
            let coef = if reduction > self.envelope {
                self.attack
            } else {
                self.release
            };
            self.envelope = flush(reduction + (self.envelope - reduction) * coef);
            let gain = db_to_gain(makeup - self.envelope as f64) as f32;
            let g = 1.0 - mix + mix * gain;
            output(ctx, i, l * g, r * g);
        }
    }
}

impl Node for Compressor {
    fn ports(&self) -> PortLayout {
        PortLayout {
            audio_in: 2,
            audio_out: 2,
            event_in: true,
            event_out: false,
        }
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: u32) {
        self.sample_rate = sample_rate;
        self.apply_param(ATTACK);
        self.reset();
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        process_segmented(self, ctx);
    }

    fn reset(&mut self) {
        self.envelope = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_curve() {
        assert_eq!(gain_computer(-30.0, -20.0, 4.0, 0.0), -30.0);
        assert_eq!(gain_computer(-8.0, -20.0, 4.0, 0.0), -17.0);
        // The knee is continuous at its edges.
        let edge = gain_computer(-17.0, -20.0, 4.0, 6.0);
        assert!((edge - (-20.0 + 3.0 / 4.0)).abs() < 1e-5);
        assert!((gain_computer(-23.0, -20.0, 4.0, 6.0) + 23.0).abs() < 1e-5);
    }
}
