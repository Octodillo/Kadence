//! Six-band parametric EQ: high-pass, low shelf, two peaks, high shelf,
//! low-pass. Each band is an RBJ biquad.

use daw_core::event::ParamId;
use daw_core::node::{Node, PortLayout, ProcessContext};

use crate::dsp::{Biquad, BiquadCoefs, Smoother, db_to_gain, gain_to_db};
use crate::events::{Segmented, input, output, process_segmented};
use crate::param::{ParamSpec, ParamStore, ParamUnit};
use crate::{DeviceCategory, DeviceInfo, impl_device};

pub static INFO: DeviceInfo = DeviceInfo {
    id: "daw.eq",
    name: "Parametric EQ",
    category: DeviceCategory::AudioEffect,
    description: "Six-band equalizer with shelves, peaks and cut filters",
};

pub const HP_ON: ParamId = 0;
pub const HP_FREQ: ParamId = 1;
pub const LOW_FREQ: ParamId = 2;
pub const LOW_GAIN: ParamId = 3;
pub const PEAK1_FREQ: ParamId = 4;
pub const PEAK1_GAIN: ParamId = 5;
pub const PEAK1_Q: ParamId = 6;
pub const PEAK2_FREQ: ParamId = 7;
pub const PEAK2_GAIN: ParamId = 8;
pub const PEAK2_Q: ParamId = 9;
pub const HIGH_FREQ: ParamId = 10;
pub const HIGH_GAIN: ParamId = 11;
pub const LP_ON: ParamId = 12;
pub const LP_FREQ: ParamId = 13;
pub const OUTPUT: ParamId = 14;

static PARAMS: &[ParamSpec] = &[
    ParamSpec::toggle(HP_ON, "hp_on", "High-pass", false),
    ParamSpec::new(
        HP_FREQ,
        "hp_freq",
        "High-pass Frequency",
        20.0,
        2000.0,
        30.0,
        ParamUnit::Hertz,
    )
    .log(),
    ParamSpec::new(
        LOW_FREQ,
        "low_freq",
        "Low Shelf Frequency",
        20.0,
        1000.0,
        100.0,
        ParamUnit::Hertz,
    )
    .log(),
    ParamSpec::new(
        LOW_GAIN,
        "low_gain",
        "Low Shelf Gain",
        -24.0,
        24.0,
        0.0,
        ParamUnit::Decibels,
    ),
    ParamSpec::new(
        PEAK1_FREQ,
        "peak1_freq",
        "Peak 1 Frequency",
        20.0,
        20000.0,
        500.0,
        ParamUnit::Hertz,
    )
    .log(),
    ParamSpec::new(
        PEAK1_GAIN,
        "peak1_gain",
        "Peak 1 Gain",
        -24.0,
        24.0,
        0.0,
        ParamUnit::Decibels,
    ),
    ParamSpec::new(
        PEAK1_Q,
        "peak1_q",
        "Peak 1 Q",
        0.1,
        18.0,
        1.0,
        ParamUnit::None,
    )
    .log(),
    ParamSpec::new(
        PEAK2_FREQ,
        "peak2_freq",
        "Peak 2 Frequency",
        20.0,
        20000.0,
        2000.0,
        ParamUnit::Hertz,
    )
    .log(),
    ParamSpec::new(
        PEAK2_GAIN,
        "peak2_gain",
        "Peak 2 Gain",
        -24.0,
        24.0,
        0.0,
        ParamUnit::Decibels,
    ),
    ParamSpec::new(
        PEAK2_Q,
        "peak2_q",
        "Peak 2 Q",
        0.1,
        18.0,
        1.0,
        ParamUnit::None,
    )
    .log(),
    ParamSpec::new(
        HIGH_FREQ,
        "high_freq",
        "High Shelf Frequency",
        1000.0,
        20000.0,
        8000.0,
        ParamUnit::Hertz,
    )
    .log(),
    ParamSpec::new(
        HIGH_GAIN,
        "high_gain",
        "High Shelf Gain",
        -24.0,
        24.0,
        0.0,
        ParamUnit::Decibels,
    ),
    ParamSpec::toggle(LP_ON, "lp_on", "Low-pass", false),
    ParamSpec::new(
        LP_FREQ,
        "lp_freq",
        "Low-pass Frequency",
        1000.0,
        20000.0,
        18000.0,
        ParamUnit::Hertz,
    )
    .log(),
    ParamSpec::new(
        OUTPUT,
        "output",
        "Output",
        -24.0,
        24.0,
        0.0,
        ParamUnit::Decibels,
    ),
];

const BANDS: usize = 6;
const SHELF_Q: f64 = std::f64::consts::FRAC_1_SQRT_2;

pub struct ParametricEq {
    params: ParamStore,
    sample_rate: f64,
    coefs: [BiquadCoefs; BANDS],
    state: [[Biquad; BANDS]; 2],
    output: Smoother,
}

impl Default for ParametricEq {
    fn default() -> Self {
        Self::new()
    }
}

impl ParametricEq {
    pub fn new() -> Self {
        let mut eq = Self {
            params: ParamStore::new(PARAMS),
            sample_rate: 48000.0,
            coefs: [BiquadCoefs::IDENTITY; BANDS],
            state: [[Biquad::default(); BANDS]; 2],
            output: Smoother::new(1.0),
        };
        eq.prepare(48000.0, 0);
        eq
    }

    fn design(&self) -> [BiquadCoefs; BANDS] {
        let p = &self.params;
        let sr = self.sample_rate;
        [
            if p.get_bool(HP_ON) {
                BiquadCoefs::high_pass(sr, p.get(HP_FREQ), SHELF_Q)
            } else {
                BiquadCoefs::IDENTITY
            },
            BiquadCoefs::low_shelf(sr, p.get(LOW_FREQ), SHELF_Q, p.get(LOW_GAIN)),
            BiquadCoefs::peaking(sr, p.get(PEAK1_FREQ), p.get(PEAK1_Q), p.get(PEAK1_GAIN)),
            BiquadCoefs::peaking(sr, p.get(PEAK2_FREQ), p.get(PEAK2_Q), p.get(PEAK2_GAIN)),
            BiquadCoefs::high_shelf(sr, p.get(HIGH_FREQ), SHELF_Q, p.get(HIGH_GAIN)),
            if p.get_bool(LP_ON) {
                BiquadCoefs::low_pass(sr, p.get(LP_FREQ), SHELF_Q)
            } else {
                BiquadCoefs::IDENTITY
            },
        ]
    }

    fn apply_param(&mut self, id: ParamId) {
        if id == OUTPUT {
            self.output
                .set_target(db_to_gain(self.params.get(OUTPUT)) as f32);
        } else {
            self.coefs = self.design();
        }
    }

    /// Combined response at `freq` in dB, for drawing the EQ curve.
    pub fn response_db(&self, freq: f64) -> f64 {
        let lin: f64 = self
            .coefs
            .iter()
            .map(|c| c.magnitude(self.sample_rate, freq))
            .product();
        gain_to_db(lin) + self.params.get(OUTPUT)
    }
}

impl_device!(ParametricEq, INFO, PARAMS);

impl Segmented for ParametricEq {
    fn param_event(&mut self, param: ParamId, value: f64) {
        crate::Device::set_param(self, param, value);
    }

    fn render(&mut self, ctx: &mut ProcessContext, start: usize, end: usize) {
        for i in start..end {
            let g = self.output.tick();
            let mut ch = [input(ctx, 0, i), input(ctx, 1, i)];
            for (x, state) in ch.iter_mut().zip(self.state.iter_mut()) {
                for (band, c) in state.iter_mut().zip(self.coefs.iter()) {
                    *x = band.process(c, *x);
                }
                *x *= g;
            }
            output(ctx, i, ch[0], ch[1]);
        }
    }
}

impl Node for ParametricEq {
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
        self.coefs = self.design();
        self.output.set_time(10.0, sample_rate);
        self.output
            .reset(db_to_gain(self.params.get(OUTPUT)) as f32);
        self.reset();
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        process_segmented(self, ctx);
    }

    fn reset(&mut self) {
        self.state = [[Biquad::default(); BANDS]; 2];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Device;

    #[test]
    fn flat_by_default_and_peaks_where_asked() {
        let mut eq = ParametricEq::new();
        for f in [20.0, 100.0, 1000.0, 10000.0, 20000.0] {
            assert!(eq.response_db(f).abs() < 1e-9, "{f}");
        }
        eq.set_param(PEAK1_FREQ, 1000.0);
        eq.set_param(PEAK1_GAIN, -9.0);
        assert!((eq.response_db(1000.0) + 9.0).abs() < 0.05);
        eq.set_param(HP_ON, 1.0);
        eq.set_param(HP_FREQ, 200.0);
        assert!(eq.response_db(20.0) < -30.0);
    }
}
