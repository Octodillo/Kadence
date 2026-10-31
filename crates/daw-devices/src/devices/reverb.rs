//! Freeverb-style reverb: per channel, eight damped comb filters in
//! parallel into four series all-pass filters, after a short pre-delay.

use daw_core::event::ParamId;
use daw_core::node::{Node, PortLayout, ProcessContext};

use crate::dsp::{DelayLine, Smoother, flush};
use crate::events::{Segmented, input, output, process_segmented};
use crate::param::{ParamSpec, ParamStore, ParamUnit};
use crate::{DeviceCategory, DeviceInfo, impl_device};

pub static INFO: DeviceInfo = DeviceInfo {
    id: "daw.reverb",
    name: "Reverb",
    category: DeviceCategory::AudioEffect,
    description: "Algorithmic room reverb",
};

pub const SIZE: ParamId = 0;
pub const DAMPING: ParamId = 1;
pub const WIDTH: ParamId = 2;
pub const MIX: ParamId = 3;
pub const PRE_DELAY: ParamId = 4;

pub(crate) static PARAMS: &[ParamSpec] = &[
    ParamSpec::new(SIZE, "size", "Room Size", 0.0, 1.0, 0.6, ParamUnit::Percent),
    ParamSpec::new(
        DAMPING,
        "damping",
        "Damping",
        0.0,
        1.0,
        0.5,
        ParamUnit::Percent,
    ),
    ParamSpec::new(WIDTH, "width", "Width", 0.0, 1.0, 1.0, ParamUnit::Percent),
    ParamSpec::new(MIX, "mix", "Mix", 0.0, 1.0, 0.3, ParamUnit::Percent),
    ParamSpec::new(
        PRE_DELAY,
        "pre_delay",
        "Pre-delay",
        0.0,
        200.0,
        10.0,
        ParamUnit::Milliseconds,
    ),
];

// Freeverb's tunings, in samples at 44.1 kHz.
const COMB_TUNING: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALLPASS_TUNING: [usize; 4] = [556, 441, 341, 225];
const STEREO_SPREAD: usize = 23;
const INPUT_GAIN: f32 = 0.015;
const ROOM_SCALE: f32 = 0.28;
const ROOM_OFFSET: f32 = 0.7;
const DAMP_SCALE: f32 = 0.4;
const ALLPASS_FEEDBACK: f32 = 0.5;
const WET_SCALE: f32 = 3.0;
const MAX_PRE_DELAY_MS: f64 = 200.0;

#[derive(Clone, Debug, Default)]
struct Comb {
    buf: Vec<f32>,
    pos: usize,
    store: f32,
}

impl Comb {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![0.0; len.max(1)],
            pos: 0,
            store: 0.0,
        }
    }

    #[inline]
    fn process(&mut self, x: f32, feedback: f32, damp: f32) -> f32 {
        let y = self.buf[self.pos];
        self.store = flush(y * (1.0 - damp) + self.store * damp);
        self.buf[self.pos] = flush(x + self.store * feedback);
        self.pos = (self.pos + 1) % self.buf.len();
        y
    }

    fn clear(&mut self) {
        self.buf.fill(0.0);
        self.store = 0.0;
    }
}

#[derive(Clone, Debug, Default)]
struct Allpass {
    buf: Vec<f32>,
    pos: usize,
}

impl Allpass {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![0.0; len.max(1)],
            pos: 0,
        }
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let b = self.buf[self.pos];
        self.buf[self.pos] = flush(x + b * ALLPASS_FEEDBACK);
        self.pos = (self.pos + 1) % self.buf.len();
        b - x
    }

    fn clear(&mut self) {
        self.buf.fill(0.0);
    }
}

#[derive(Clone, Debug, Default)]
struct Channel {
    combs: Vec<Comb>,
    allpasses: Vec<Allpass>,
}

impl Channel {
    fn new(scale: f64, spread: usize) -> Self {
        let len = |n: usize| ((n + spread) as f64 * scale).round() as usize;
        Self {
            combs: COMB_TUNING.iter().map(|&n| Comb::new(len(n))).collect(),
            allpasses: ALLPASS_TUNING
                .iter()
                .map(|&n| Allpass::new(len(n)))
                .collect(),
        }
    }

    #[inline]
    fn process(&mut self, x: f32, feedback: f32, damp: f32) -> f32 {
        let mut acc = 0.0;
        for c in &mut self.combs {
            acc += c.process(x, feedback, damp);
        }
        for a in &mut self.allpasses {
            acc = a.process(acc);
        }
        acc
    }

    fn clear(&mut self) {
        self.combs.iter_mut().for_each(Comb::clear);
        self.allpasses.iter_mut().for_each(Allpass::clear);
    }
}

pub struct Reverb {
    params: ParamStore,
    sample_rate: f64,
    channels: [Channel; 2],
    pre_delay: [DelayLine; 2],
    mix: Smoother,
    width: Smoother,
}

impl Default for Reverb {
    fn default() -> Self {
        Self::new()
    }
}

impl Reverb {
    pub fn new() -> Self {
        let mut r = Self {
            params: ParamStore::new(PARAMS),
            sample_rate: 0.0,
            channels: [Channel::default(), Channel::default()],
            pre_delay: [DelayLine::default(), DelayLine::default()],
            mix: Smoother::new(0.3),
            width: Smoother::new(1.0),
        };
        r.prepare(48000.0, 0);
        r
    }

    fn apply_param(&mut self, id: ParamId) {
        match id {
            MIX => self.mix.set_target(self.params.get_f32(MIX)),
            WIDTH => self.width.set_target(self.params.get_f32(WIDTH)),
            _ => {}
        }
    }
}

impl_device!(Reverb, INFO, PARAMS, {
    fn snap_smoothing(&mut self) {
        self.mix.reset(self.mix.target());
        self.width.reset(self.width.target());
    }
});

impl Segmented for Reverb {
    fn param_event(&mut self, param: ParamId, value: f64) {
        crate::Device::set_param(self, param, value);
    }

    fn render(&mut self, ctx: &mut ProcessContext, start: usize, end: usize) {
        let feedback = self.params.get_f32(SIZE) * ROOM_SCALE + ROOM_OFFSET;
        let damp = self.params.get_f32(DAMPING) * DAMP_SCALE;
        let pre = (self.params.get(PRE_DELAY) / 1000.0 * self.sample_rate) as f32;
        for i in start..end {
            let (l, r) = (input(ctx, 0, i), input(ctx, 1, i));
            self.pre_delay[0].push(l);
            self.pre_delay[1].push(r);
            let x = (self.pre_delay[0].read(pre) + self.pre_delay[1].read(pre)) * INPUT_GAIN;
            let out_l = self.channels[0].process(x, feedback, damp);
            let out_r = self.channels[1].process(x, feedback, damp);
            let mix = self.mix.tick();
            let width = self.width.tick();
            let wet1 = mix * WET_SCALE * (width / 2.0 + 0.5);
            let wet2 = mix * WET_SCALE * ((1.0 - width) / 2.0);
            let dry = 1.0 - mix;
            output(
                ctx,
                i,
                out_l * wet1 + out_r * wet2 + l * dry,
                out_r * wet1 + out_l * wet2 + r * dry,
            );
        }
    }
}

impl Node for Reverb {
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
            let scale = sample_rate / 44100.0;
            self.channels = [Channel::new(scale, 0), Channel::new(scale, STEREO_SPREAD)];
            let pre = (MAX_PRE_DELAY_MS / 1000.0 * sample_rate).ceil() as usize;
            self.pre_delay = [DelayLine::new(pre), DelayLine::new(pre)];
        }
        self.sample_rate = sample_rate;
        self.mix.set_time(10.0, sample_rate);
        self.width.set_time(10.0, sample_rate);
        self.mix.reset(self.params.get_f32(MIX));
        self.width.reset(self.params.get_f32(WIDTH));
        self.reset();
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        process_segmented(self, ctx);
    }

    fn reset(&mut self) {
        for c in &mut self.channels {
            c.clear();
        }
        for d in &mut self.pre_delay {
            d.clear();
        }
    }
}
