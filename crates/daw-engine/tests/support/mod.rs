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

/// Outputs 1, 2, 3, ... one per frame, counting across blocks, so a test can
/// tell whether the same instance kept running.
pub struct Counter(pub f32);

impl Node for Counter {
    fn ports(&self) -> PortLayout {
        PortLayout {
            audio_out: 1,
            ..Default::default()
        }
    }

    fn prepare(&mut self, _sample_rate: f64, _max_frames: u32) {}

    fn process(&mut self, ctx: &mut ProcessContext) {
        for o in ctx.audio_out[0].iter_mut() {
            self.0 += 1.0;
            *o = self.0;
        }
    }
}

/// Outputs a constant on every frame.
pub struct Dc(pub f32);

impl Node for Dc {
    fn ports(&self) -> PortLayout {
        PortLayout {
            audio_out: 1,
            ..Default::default()
        }
    }

    fn prepare(&mut self, _sample_rate: f64, _max_frames: u32) {}

    fn process(&mut self, ctx: &mut ProcessContext) {
        ctx.audio_out[0].fill(self.0);
    }
}

/// What a test can ask of a [`FakePlugin`] from outside.
#[derive(Default)]
pub struct FakeControl {
    /// Latency and output count to take at the next prepare.
    pub next_latency: std::sync::atomic::AtomicU32,
    pub next_outputs: std::sync::atomic::AtomicU32,
    /// Set to make the next `on_main_thread` report a layout change.
    pub request: std::sync::atomic::AtomicBool,
    pub prepares: std::sync::atomic::AtomicU32,
    /// The thread the plugin was dropped on.
    pub dropped_on: std::sync::Mutex<Option<std::thread::ThreadId>>,
}

/// A plugin that outputs a sample counter on every channel. The counter is its state: it
/// survives prepares, and `load_state` sets it.
pub struct FakePlugin {
    pub control: std::sync::Arc<FakeControl>,
    descriptor: daw_core::plugin::PluginDescriptor,
    outputs: u16,
    latency: u32,
    count: f32,
    changed: bool,
}

impl FakePlugin {
    pub fn new(outputs: u16) -> Self {
        let control = FakeControl::default();
        control
            .next_outputs
            .store(outputs.into(), std::sync::atomic::Ordering::Relaxed);
        Self {
            control: std::sync::Arc::new(control),
            descriptor: daw_core::plugin::PluginDescriptor {
                format: daw_core::plugin::PluginFormatKind::Clap,
                id: "fake".into(),
                name: "fake".into(),
                vendor: String::new(),
                path: Default::default(),
                is_instrument: true,
                ports: (0, outputs),
            },
            outputs,
            latency: 0,
            count: 0.0,
            changed: false,
        }
    }
}

impl Node for FakePlugin {
    fn ports(&self) -> PortLayout {
        PortLayout {
            audio_out: self.outputs,
            event_in: true,
            ..Default::default()
        }
    }

    fn prepare(&mut self, _sample_rate: f64, _max_frames: u32) {
        use std::sync::atomic::Ordering::Relaxed;
        let c = &self.control;
        c.prepares.fetch_add(1, Relaxed);
        self.outputs = c.next_outputs.load(Relaxed) as u16;
        self.latency = c.next_latency.load(Relaxed);
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        assert_eq!(ctx.audio_out.len(), self.outputs as usize);
        for i in 0..ctx.frames as usize {
            self.count += 1.0;
            for ch in ctx.audio_out.iter_mut() {
                ch[i] = self.count;
            }
        }
    }

    fn latency(&self) -> u32 {
        self.latency
    }
}

impl Drop for FakePlugin {
    fn drop(&mut self) {
        *self.control.dropped_on.lock().unwrap() = Some(std::thread::current().id());
    }
}

impl daw_core::plugin::PluginInstance for FakePlugin {
    fn descriptor(&self) -> &daw_core::plugin::PluginDescriptor {
        &self.descriptor
    }
    fn params(&self) -> Vec<daw_core::plugin::ParamInfo> {
        Vec::new()
    }
    fn param_value(&self, _id: daw_core::event::ParamId) -> f64 {
        0.0
    }
    fn save_state(&mut self) -> Result<Vec<u8>, daw_core::plugin::PluginError> {
        Ok(self.count.to_le_bytes().to_vec())
    }
    /// Takes the counter and, like a plugin whose latency depends on its settings, asks for
    /// a new layout.
    fn load_state(&mut self, data: &[u8]) -> Result<(), daw_core::plugin::PluginError> {
        let bytes = data
            .try_into()
            .map_err(|_| daw_core::plugin::PluginError::State("4 bytes".into()))?;
        self.count = f32::from_le_bytes(bytes);
        self.changed = true;
        Ok(())
    }
    fn has_editor(&self) -> bool {
        false
    }
    fn open_editor(&mut self) -> Result<(), daw_core::plugin::PluginError> {
        Err(daw_core::plugin::PluginError::Editor("none".into()))
    }
    fn close_editor(&mut self) {}
    fn on_main_thread(&mut self) {
        if self
            .control
            .request
            .swap(false, std::sync::atomic::Ordering::Relaxed)
        {
            self.changed = true;
        }
    }
    fn take_layout_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }
}
