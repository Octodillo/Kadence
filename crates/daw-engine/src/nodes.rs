//! Built-in nodes the graph needs for routing: channel strips for tracks,
//! buses, sends and master.

use daw_core::event::{EventKind, ParamId};
use daw_core::node::{Node, PortLayout, ProcessContext};
use daw_core::plugin::ParamInfo;

/// A channel strip: per-channel linear gain, balance (stereo only) and mute.
///
/// Parameters change at the exact frame of their `ParamValue` event. There is
/// no smoothing yet, so automation that needs to be click-free should send
/// dense events.
pub struct Strip {
    channels: u16,
    gain: f32,
    pan: f32,
    muted: bool,
    has_pan: bool,
    has_mute: bool,
}

impl Strip {
    /// Linear gain, 0 to 4 (+12 dB).
    pub const GAIN: ParamId = 0;
    /// Balance, -1 (left) to 1 (right). Only on stereo track and bus strips.
    pub const PAN: ParamId = 1;
    /// 0 or 1. Only on track and bus strips.
    pub const MUTE: ParamId = 2;

    pub fn track(channels: u16) -> Self {
        Self::with(channels, 1.0, true, true)
    }

    pub fn bus(channels: u16) -> Self {
        Self::with(channels, 1.0, true, true)
    }

    pub fn send(channels: u16, gain: f32) -> Self {
        Self::with(channels, gain, false, false)
    }

    pub fn master(channels: u16) -> Self {
        Self::with(channels, 1.0, false, false)
    }

    fn with(channels: u16, gain: f32, pan: bool, mute: bool) -> Self {
        let mut strip = Self {
            channels,
            gain: 1.0,
            pan: 0.0,
            muted: false,
            has_pan: pan && channels == 2,
            has_mute: mute,
        };
        strip.set_gain(gain);
        strip
    }

    pub fn with_gain(mut self, gain: f32) -> Self {
        self.set_gain(gain);
        self
    }

    pub fn with_pan(mut self, pan: f32) -> Self {
        self.set_pan(pan);
        self
    }

    /// Clamps to the range of [`GAIN`](Self::GAIN), so an infinite gain
    /// cannot turn silence into NaN, and ignores NaN, which would poison
    /// every bus the strip feeds.
    pub fn set_gain(&mut self, gain: f32) {
        if !gain.is_nan() {
            self.gain = gain.clamp(0.0, 4.0);
        }
    }

    /// Ignores NaN, like [`set_gain`](Self::set_gain).
    pub fn set_pan(&mut self, pan: f32) {
        if !pan.is_nan() {
            self.pan = pan.clamp(-1.0, 1.0);
        }
    }

    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
    }

    pub fn params(&self) -> Vec<ParamInfo> {
        let mut v = vec![ParamInfo {
            id: Self::GAIN,
            name: "Gain".into(),
            min: 0.0,
            max: 4.0,
            default: 1.0,
            automatable: true,
        }];
        if self.has_pan {
            v.push(ParamInfo {
                id: Self::PAN,
                name: "Pan".into(),
                min: -1.0,
                max: 1.0,
                default: 0.0,
                automatable: true,
            });
        }
        if self.has_mute {
            v.push(ParamInfo {
                id: Self::MUTE,
                name: "Mute".into(),
                min: 0.0,
                max: 1.0,
                default: 0.0,
                automatable: true,
            });
        }
        v
    }

    fn apply(&mut self, param: ParamId, value: f64) {
        // `clamp` passes NaN through.
        if value.is_nan() {
            return;
        }
        match param {
            Self::GAIN => self.set_gain(value as f32),
            Self::PAN if self.has_pan => self.set_pan(value as f32),
            Self::MUTE if self.has_mute => self.muted = value >= 0.5,
            _ => {}
        }
    }

    fn channel_gain(&self, ch: usize) -> f32 {
        if self.muted {
            return 0.0;
        }
        if !self.has_pan || self.pan == 0.0 {
            return self.gain;
        }
        let balance = if ch == 0 {
            (1.0 - self.pan).min(1.0)
        } else {
            (1.0 + self.pan).min(1.0)
        };
        self.gain * balance
    }

    fn run(&self, ctx: &mut ProcessContext, start: usize, end: usize) {
        for (ch, (out, inp)) in ctx
            .audio_out
            .iter_mut()
            .zip(ctx.audio_in.iter())
            .enumerate()
        {
            let g = self.channel_gain(ch);
            for (o, i) in out[start..end].iter_mut().zip(&inp[start..end]) {
                *o = *i * g;
            }
        }
    }
}

impl Node for Strip {
    fn ports(&self) -> PortLayout {
        PortLayout {
            audio_in: self.channels,
            audio_out: self.channels,
            event_in: true,
            event_out: false,
        }
    }

    fn prepare(&mut self, _sample_rate: f64, _max_frames: u32) {}

    fn process(&mut self, ctx: &mut ProcessContext) {
        let frames = ctx.frames as usize;
        let mut start = 0;
        let events = ctx.events_in;
        for e in events.iter() {
            if let EventKind::ParamValue { param, value } = e.kind {
                let at = (e.frame as usize).min(frames);
                if at > start {
                    self.run(ctx, start, at);
                    start = at;
                }
                self.apply(param, value);
            }
        }
        self.run(ctx, start, frames);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_stays_finite_and_in_range() {
        assert_eq!(Strip::send(2, f32::NAN).gain, 1.0);
        assert_eq!(Strip::send(2, 0.25).gain, 0.25);
        assert_eq!(Strip::track(2).with_gain(f32::NAN).gain, 1.0);
        assert_eq!(Strip::track(2).with_gain(f32::INFINITY).gain, 4.0);
        assert_eq!(Strip::track(2).with_gain(-1.0).gain, 0.0);
        let mut s = Strip::bus(2);
        s.apply(Strip::GAIN, f64::INFINITY);
        assert_eq!(s.gain, 4.0);
        s.apply(Strip::GAIN, f64::NAN);
        assert_eq!(s.gain, 4.0);
    }
}
