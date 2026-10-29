//! Deterministic test nodes. Everything here uses only `+` and `*` on
//! fixed constants, so renders are bit-identical on every platform.
#![allow(dead_code)]

use daw_core::event::{Event, EventKind};
use daw_core::node::{Node, PortLayout, ProcessContext};

/// A sine from a rotating phasor, about 1 kHz at 48 kHz, on every channel.
pub struct Sine {
    channels: u16,
    x: f64,
    y: f64,
}

// cos and sin of 2π / 48.
const C: f64 = 0.991_444_861_373_810_4;
const S: f64 = 0.130_526_192_220_051_6;

impl Sine {
    pub fn new(channels: u16) -> Self {
        Self {
            channels,
            x: 1.0,
            y: 0.0,
        }
    }

    /// The first `n` samples this node outputs.
    pub fn reference(n: usize) -> Vec<f32> {
        let mut s = Self::new(1);
        (0..n).map(|_| s.tick()).collect()
    }

    fn tick(&mut self) -> f32 {
        let out = self.y as f32;
        let (x, y) = (C * self.x - S * self.y, S * self.x + C * self.y);
        self.x = x;
        self.y = y;
        out
    }
}

impl Node for Sine {
    fn ports(&self) -> PortLayout {
        PortLayout {
            audio_out: self.channels,
            ..Default::default()
        }
    }

    fn prepare(&mut self, _sample_rate: f64, _max_frames: u32) {}

    fn process(&mut self, ctx: &mut ProcessContext) {
        for i in 0..ctx.frames as usize {
            let v = self.tick();
            for ch in ctx.audio_out.iter_mut() {
                ch[i] = v;
            }
        }
    }

    fn reset(&mut self) {
        (self.x, self.y) = (1.0, 0.0);
    }
}

/// Delays a mono signal by `n` samples and reports it as latency, like a
/// look-ahead plugin.
pub struct Delay {
    buf: Vec<f32>,
    pos: usize,
}

impl Delay {
    pub fn new(n: usize) -> Self {
        Self {
            buf: vec![0.0; n],
            pos: 0,
        }
    }
}

impl Node for Delay {
    fn ports(&self) -> PortLayout {
        PortLayout {
            audio_in: 1,
            audio_out: 1,
            ..Default::default()
        }
    }

    fn prepare(&mut self, _sample_rate: f64, _max_frames: u32) {}

    fn process(&mut self, ctx: &mut ProcessContext) {
        for i in 0..ctx.frames as usize {
            let x = ctx.audio_in[0][i];
            ctx.audio_out[0][i] = self.buf[self.pos];
            self.buf[self.pos] = x;
            self.pos = (self.pos + 1) % self.buf.len();
        }
    }

    fn latency(&self) -> u32 {
        self.buf.len() as u32
    }
}

/// Outputs the transport position of each frame (or -1 when stopped).
pub struct Position;

impl Node for Position {
    fn ports(&self) -> PortLayout {
        PortLayout {
            audio_out: 1,
            ..Default::default()
        }
    }

    fn prepare(&mut self, _sample_rate: f64, _max_frames: u32) {}

    fn process(&mut self, ctx: &mut ProcessContext) {
        let t = ctx.transport;
        for (i, o) in ctx.audio_out[0].iter_mut().enumerate() {
            *o = if t.playing {
                (t.position.0 + i as i64) as f32
            } else {
                -1.0
            };
        }
    }
}

/// Emits a note-on at `frame` of every block.
pub struct NoteSource {
    pub frame: u32,
}

impl Node for NoteSource {
    fn ports(&self) -> PortLayout {
        PortLayout {
            event_out: true,
            ..Default::default()
        }
    }

    fn prepare(&mut self, _sample_rate: f64, _max_frames: u32) {}

    fn process(&mut self, ctx: &mut ProcessContext) {
        if self.frame < ctx.frames {
            let kind = EventKind::NoteOn {
                channel: 0,
                key: 60,
                velocity: 1.0,
                note_id: -1,
            };
            let _ = ctx.events_out.push(Event {
                frame: self.frame,
                kind,
            });
        }
    }
}

/// Outputs how many events arrived at each frame.
pub struct EventProbe;

impl Node for EventProbe {
    fn ports(&self) -> PortLayout {
        PortLayout {
            audio_out: 1,
            event_in: true,
            ..Default::default()
        }
    }

    fn prepare(&mut self, _sample_rate: f64, _max_frames: u32) {}

    fn process(&mut self, ctx: &mut ProcessContext) {
        for e in ctx.events_in.iter() {
            ctx.audio_out[0][e.frame as usize] += 1.0;
        }
    }
}
