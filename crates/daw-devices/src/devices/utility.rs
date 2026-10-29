//! Gain, pan (balance), stereo width and polarity.

use daw_core::event::ParamId;
use daw_core::node::{Node, PortLayout, ProcessContext};

use crate::dsp::{Smoother, db_to_gain};
use crate::events::{Segmented, input, output, process_segmented};
use crate::param::{ParamSpec, ParamStore, ParamUnit};
use crate::{DeviceCategory, DeviceInfo, impl_device};

pub static INFO: DeviceInfo = DeviceInfo {
    id: "daw.utility",
    name: "Utility",
    category: DeviceCategory::Utility,
    description: "Gain, pan, stereo width and polarity",
};

pub const GAIN: ParamId = 0;
pub const PAN: ParamId = 1;
pub const WIDTH: ParamId = 2;
pub const INVERT_L: ParamId = 3;
pub const INVERT_R: ParamId = 4;

static PARAMS: &[ParamSpec] = &[
    ParamSpec::new(GAIN, "gain", "Gain", -70.0, 24.0, 0.0, ParamUnit::Decibels),
    ParamSpec::new(PAN, "pan", "Pan", -1.0, 1.0, 0.0, ParamUnit::Pan),
    ParamSpec::new(WIDTH, "width", "Width", 0.0, 2.0, 1.0, ParamUnit::Percent),
    ParamSpec::toggle(INVERT_L, "invert_l", "Invert Left", false),
    ParamSpec::toggle(INVERT_R, "invert_r", "Invert Right", false),
];

pub struct Utility {
    params: ParamStore,
    gain_l: Smoother,
    gain_r: Smoother,
    width: Smoother,
}

impl Default for Utility {
    fn default() -> Self {
        Self::new()
    }
}

impl Utility {
    pub fn new() -> Self {
        let mut u = Self {
            params: ParamStore::new(PARAMS),
            gain_l: Smoother::new(1.0),
            gain_r: Smoother::new(1.0),
            width: Smoother::new(1.0),
        };
        u.apply_param(GAIN);
        u.snap();
        u
    }

    /// Per-channel gains: gain, then a sin/cos balance law that keeps the
    /// centre at unity, then polarity.
    fn channel_gains(&self) -> (f32, f32) {
        let g = db_to_gain(self.params.get(GAIN));
        let pan = self.params.get(PAN);
        let angle = pan.abs() * std::f64::consts::FRAC_PI_2;
        let (mut l, mut r) = if pan > 0.0 {
            (angle.cos(), 1.0)
        } else {
            (1.0, angle.cos())
        };
        if self.params.get_bool(INVERT_L) {
            l = -l;
        }
        if self.params.get_bool(INVERT_R) {
            r = -r;
        }
        ((g * l) as f32, (g * r) as f32)
    }

    fn apply_param(&mut self, _id: ParamId) {
        let (l, r) = self.channel_gains();
        self.gain_l.set_target(l);
        self.gain_r.set_target(r);
        self.width.set_target(self.params.get_f32(WIDTH));
    }

    fn snap(&mut self) {
        self.gain_l.reset(self.gain_l.target());
        self.gain_r.reset(self.gain_r.target());
        self.width.reset(self.width.target());
    }
}

impl_device!(Utility, INFO, PARAMS);

impl Segmented for Utility {
    fn param_event(&mut self, param: ParamId, value: f64) {
        crate::Device::set_param(self, param, value);
    }

    fn render(&mut self, ctx: &mut ProcessContext, start: usize, end: usize) {
        for i in start..end {
            let (l, r) = (input(ctx, 0, i), input(ctx, 1, i));
            let w = self.width.tick();
            let mid = 0.5 * (l + r);
            let side = 0.5 * (l - r) * w;
            output(
                ctx,
                i,
                (mid + side) * self.gain_l.tick(),
                (mid - side) * self.gain_r.tick(),
            );
        }
    }
}

impl Node for Utility {
    fn ports(&self) -> PortLayout {
        PortLayout {
            audio_in: 2,
            audio_out: 2,
            event_in: true,
            event_out: false,
        }
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: u32) {
        for s in [&mut self.gain_l, &mut self.gain_r, &mut self.width] {
            s.set_time(10.0, sample_rate);
        }
        self.snap();
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        process_segmented(self, ctx);
    }

    fn reset(&mut self) {
        self.snap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balance_law() {
        let mut u = Utility::new();
        assert_eq!(u.channel_gains(), (1.0, 1.0));
        crate::Device::set_param(&mut u, PAN, 1.0);
        let (l, r) = u.channel_gains();
        assert!(l.abs() < 1e-7 && r == 1.0);
        crate::Device::set_param(&mut u, PAN, -0.5);
        crate::Device::set_param(&mut u, INVERT_L, 1.0);
        let (l, r) = u.channel_gains();
        assert_eq!(l, -1.0);
        assert!((r - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
    }
}
