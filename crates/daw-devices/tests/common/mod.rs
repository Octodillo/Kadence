//! Test harness shared by the device test binaries. Each device has its own
//! test binary, and nextest runs every test in its own process.
#![allow(dead_code)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use daw_core::event::{Event, EventKind, EventList};
use daw_core::node::{ProcessContext, TransportState};
use daw_core::time::SampleTime;
use daw_devices::Device;

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

pub const GOLDEN_RATE: f64 = 22050.0;

#[derive(Clone, Copy, Debug)]
pub struct Setup {
    pub sample_rate: f64,
    pub block: usize,
    pub bpm: f64,
    pub playing: bool,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            sample_rate: 48000.0,
            block: 64,
            bpm: 120.0,
            playing: true,
        }
    }
}

impl Setup {
    pub fn golden() -> Self {
        // An odd block size makes events land mid-block and checks that
        // nothing depends on power-of-two buffers.
        Self {
            sample_rate: GOLDEN_RATE,
            block: 97,
            ..Self::default()
        }
    }
}

pub struct Rendered {
    pub channels: Vec<Vec<f32>>,
    /// How long each `process` call took.
    pub block_times: Vec<Duration>,
}

/// Run `dev` for `frames` frames. `input` is per channel and zero-padded;
/// events are `(absolute frame, kind)`. Every `process` call runs inside
/// `assert_no_alloc`, and the test fails if it allocates.
pub fn render(
    dev: &mut dyn Device,
    frames: usize,
    input: &[Vec<f32>],
    events: &[(usize, EventKind)],
    setup: Setup,
) -> Rendered {
    let ports = dev.ports();
    dev.prepare(setup.sample_rate, setup.block as u32);
    let mut out = vec![vec![0.0f32; frames]; ports.audio_out as usize];
    let mut in_blocks = vec![vec![0.0f32; setup.block]; ports.audio_in as usize];
    let mut out_blocks = vec![vec![0.0f32; setup.block]; ports.audio_out as usize];
    let mut events_in = EventList::with_capacity(256);
    let mut events_out = EventList::with_capacity(256);
    let mut block_times = Vec::with_capacity(frames / setup.block + 1);
    let mut pos = 0;
    while pos < frames {
        let n = setup.block.min(frames - pos);
        for (c, b) in in_blocks.iter_mut().enumerate() {
            for (i, s) in b[..n].iter_mut().enumerate() {
                *s = input
                    .get(c)
                    .or(input.first())
                    .and_then(|ch| ch.get(pos + i))
                    .copied()
                    .unwrap_or(0.0);
            }
        }
        events_in.clear();
        events_out.clear();
        for &(at, kind) in events.iter().filter(|(at, _)| (pos..pos + n).contains(at)) {
            events_in
                .push(Event {
                    frame: (at - pos) as u32,
                    kind,
                })
                .expect("event capacity");
        }
        let transport = TransportState {
            playing: setup.playing,
            position: SampleTime(pos as i64),
            bpm: setup.bpm,
            sample_rate: setup.sample_rate,
        };
        {
            let ins: Vec<&[f32]> = in_blocks.iter().map(|b| &b[..n]).collect();
            let mut outs: Vec<&mut [f32]> = out_blocks.iter_mut().map(|b| &mut b[..n]).collect();
            let mut ctx = ProcessContext {
                frames: n as u32,
                transport: &transport,
                audio_in: &ins,
                audio_out: &mut outs,
                events_in: &events_in,
                events_out: &mut events_out,
            };
            let start = Instant::now();
            #[cfg(debug_assertions)]
            let violations = assert_no_alloc::violation_count();
            assert_no_alloc(|| dev.process(&mut ctx));
            // The workspace enables `warn_debug`, so a violation is counted
            // rather than aborting; turn it into a test failure.
            #[cfg(debug_assertions)]
            assert_eq!(
                assert_no_alloc::violation_count(),
                violations,
                "process allocated at frame {pos}"
            );
            block_times.push(start.elapsed());
        }
        for (o, b) in out.iter_mut().zip(&out_blocks) {
            o[pos..pos + n].copy_from_slice(&b[..n]);
        }
        pos += n;
    }
    Rendered {
        channels: out,
        block_times,
    }
}

pub fn note_on(key: u8, velocity: f32) -> EventKind {
    EventKind::NoteOn {
        channel: 0,
        key,
        velocity,
        note_id: -1,
    }
}

pub fn note_off(key: u8) -> EventKind {
    EventKind::NoteOff {
        channel: 0,
        key,
        velocity: 0.0,
        note_id: -1,
    }
}

pub fn param(param: u32, value: f64) -> EventKind {
    EventKind::ParamValue { param, value }
}

pub fn sine(freq: f64, amplitude: f32, sample_rate: f64, frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|i| amplitude * (std::f64::consts::TAU * freq * i as f64 / sample_rate).sin() as f32)
        .collect()
}

/// Deterministic white noise in `-amplitude..amplitude`.
pub fn noise(seed: u32, amplitude: f32, frames: usize) -> Vec<f32> {
    let mut x = seed.max(1);
    (0..frames)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            amplitude * ((x as f32 / u32::MAX as f32) * 2.0 - 1.0)
        })
        .collect()
}

pub fn impulse(frames: usize) -> Vec<f32> {
    let mut v = vec![0.0; frames];
    v[0] = 1.0;
    v
}

pub fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

pub fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|s| s * s).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.f32"))
}

/// Compare against `tests/golden/<name>.f32`. Set `DAW_UPDATE_GOLDEN=1` to
/// (re)write the file. A small tolerance absorbs libm differences between
/// platforms; real DSP changes are far larger.
pub fn check_golden(name: &str, channels: &[Vec<f32>]) {
    for c in channels {
        assert!(c.iter().all(|s| s.is_finite()), "{name}: non-finite output");
    }
    let path = golden_path(name);
    if std::env::var_os("DAW_UPDATE_GOLDEN").is_some() {
        let mut bytes = Vec::new();
        bytes.extend((channels.len() as u32).to_le_bytes());
        bytes.extend((channels[0].len() as u32).to_le_bytes());
        for c in channels {
            for s in c {
                bytes.extend(s.to_le_bytes());
            }
        }
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        return;
    }
    let bytes = std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e}; run with DAW_UPDATE_GOLDEN=1 to create it",
            path.display()
        )
    });
    let word = |i: usize| u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap());
    let (nch, frames) = (word(0) as usize, word(1) as usize);
    assert_eq!(nch, channels.len(), "{name}: channel count");
    assert_eq!(frames, channels[0].len(), "{name}: length");
    let mut worst = (0.0f32, 0, 0);
    for (c, ch) in channels.iter().enumerate() {
        for (i, &s) in ch.iter().enumerate() {
            let g = f32::from_bits(word(2 + c * frames + i));
            let d = (g - s).abs();
            if d > worst.0 {
                worst = (d, c, i);
            }
        }
    }
    assert!(
        worst.0 <= 1e-4,
        "{name}: differs from golden by {} at channel {} frame {}",
        worst.0,
        worst.1,
        worst.2
    );
}

/// A device that is excited and then fed silence must decay to exact zeros,
/// never output subnormal numbers, and not get slower while it decays.
pub fn check_denormals(
    dev: &mut dyn Device,
    input: &[Vec<f32>],
    events: &[(usize, EventKind)],
    seconds: f64,
) {
    let setup = Setup {
        block: 256,
        ..Setup::default()
    };
    let frames = (seconds * setup.sample_rate) as usize;
    let r = render(dev, frames, input, events, setup);
    for (c, ch) in r.channels.iter().enumerate() {
        if let Some(i) = ch.iter().position(|s| s.is_subnormal()) {
            panic!("subnormal output on channel {c} at frame {i}");
        }
        let tail = &ch[frames - frames / 10..];
        assert!(
            tail.iter().all(|&s| s == 0.0),
            "channel {c} has not decayed to silence; peak {}",
            peak(tail)
        );
    }
    let median = |t: &[Duration]| {
        let mut t = t.to_vec();
        t.sort();
        t[t.len() / 2]
    };
    let active = median(&r.block_times[..r.block_times.len() / 10]);
    let tail = median(&r.block_times[r.block_times.len() * 9 / 10..]);
    assert!(
        tail <= active * 4 + Duration::from_micros(50),
        "decaying tail is slower: {tail:?} vs {active:?}"
    );
}

/// Zeros in (and no events) must give exact zeros out.
pub fn check_silence(dev: &mut dyn Device) {
    let setup = Setup::default();
    let r = render(dev, setup.sample_rate as usize, &[], &[], setup);
    for ch in &r.channels {
        assert!(
            ch.iter().all(|&s| s == 0.0),
            "silence in did not give silence out; peak {}",
            peak(ch)
        );
    }
}
