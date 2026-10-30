//! Plays a sine (or a MIDI-played sine) through the audio backend and reports
//! xruns, DSP load and whether the audio thread got real-time scheduling.

use std::f32::consts::TAU;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use daw_backend::{AudioBackend, BackendConfig, DummyBackend, JackBackend, Pacing};
use daw_core::event::EventKind;

#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: daw_backend::RtAllocCounter = daw_backend::RtAllocCounter;

const USAGE: &str = "\
Usage: daw-backend-demo [options]
  --frames N    ask for an N-frame period (64, 128, 256...)
  --rate HZ     ask for a sample rate (JACK and PipeWire may ignore it)
  --seconds S   run for S seconds, then print a summary (default: until Ctrl+C)
  --midi        play notes from the MIDI input instead of a constant tone
  --no-connect  do not connect to the sound card automatically
  --dummy       use the dummy backend instead of JACK/PipeWire
Exits 0 if no xruns happened, 1 otherwise.";

struct Args {
    frames: Option<u32>,
    rate: Option<u32>,
    seconds: Option<u64>,
    midi: bool,
    connect: bool,
    dummy: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        frames: None,
        rate: None,
        seconds: None,
        midi: false,
        connect: true,
        dummy: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut num = |name: &str| -> Result<u32, String> {
            it.next()
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| format!("{name} needs a number"))
        };
        match arg.as_str() {
            "--frames" => a.frames = Some(num("--frames")?),
            "--rate" => a.rate = Some(num("--rate")?),
            "--seconds" => a.seconds = Some(num("--seconds")?.into()),
            "--midi" => a.midi = true,
            "--no-connect" => a.connect = false,
            "--dummy" => a.dummy = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown option {other}\n{USAGE}")),
        }
    }
    Ok(a)
}

/// A sine voice: a constant tone, or monophonic last-note priority from MIDI.
struct Synth {
    phase: f32,
    freq: f32,
    gain: f32,
    target: f32,
    held: [bool; 128],
    last: Option<u8>,
}

impl Synth {
    const LEVEL: f32 = 0.125; // -18 dBFS

    fn new(midi: bool) -> Self {
        Self {
            phase: 0.0,
            freq: 440.0,
            gain: 0.0,
            target: if midi { 0.0 } else { Self::LEVEL },
            held: [false; 128],
            last: None,
        }
    }

    fn note(&mut self, key: u8, on: bool) {
        self.held[usize::from(key & 0x7F)] = on;
        if on {
            self.last = Some(key);
        } else if self.last == Some(key) {
            self.last = (0..128u8).rev().find(|&k| self.held[usize::from(k)]);
        }
        match self.last {
            Some(k) => {
                self.freq = 440.0 * 2f32.powf((f32::from(k) - 69.0) / 12.0);
                self.target = Self::LEVEL;
            }
            None => self.target = 0.0,
        }
    }

    fn sample(&mut self, sample_rate: f32) -> f32 {
        // About 5 ms to full level, so notes do not click.
        let step = Self::LEVEL / (0.005 * sample_rate);
        self.gain += (self.target - self.gain).clamp(-step, step);
        self.phase = (self.phase + self.freq / sample_rate).fract();
        (self.phase * TAU).sin() * self.gain
    }
}

fn yes_no(rt: Option<bool>) -> &'static str {
    match rt {
        Some(true) => "yes",
        Some(false) => "NO",
        None => "?",
    }
}

fn main() {
    let args = parse_args().unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(2);
    });

    // Ask PipeWire for the period size the polite way: as this client's
    // latency, which the graph honours unless another client wants less.
    if let Some(frames) = args.frames
        && std::env::var_os("PIPEWIRE_LATENCY").is_none()
    {
        let rate = args.rate.unwrap_or(48_000);
        // SAFETY: no other threads exist yet.
        unsafe { std::env::set_var("PIPEWIRE_LATENCY", format!("{frames}/{rate}")) };
    }

    let mut backend: Box<dyn AudioBackend> = if args.dummy {
        Box::new(DummyBackend::new(Pacing::RealTime))
    } else {
        Box::new(JackBackend::new())
    };
    let cfg = BackendConfig {
        client_name: "daw-backend-demo".into(),
        audio_inputs: 0,
        audio_outputs: 2,
        midi_input: args.midi,
        midi_output: false,
        sample_rate: args.rate,
        buffer_size: args.frames,
        auto_connect: args.connect,
        ..BackendConfig::default()
    };
    if let Err(e) = backend.open(cfg) {
        eprintln!("Could not open the {} backend: {e}", backend.name());
        std::process::exit(1);
    }

    let mut synth = Synth::new(args.midi);
    let started = backend.start(Box::new(move |cycle| {
        for e in cycle.midi_in.iter() {
            match e.kind {
                EventKind::NoteOn { key, .. } => synth.note(key, true),
                EventKind::NoteOff { key, .. } => synth.note(key, false),
                _ => {}
            }
        }
        let sr = cycle.sample_rate as f32;
        for i in 0..cycle.frames as usize {
            let s = synth.sample(sr);
            for out in cycle.audio_out.iter_mut() {
                out[i] = s;
            }
        }
    }));
    if let Err(e) = started {
        eprintln!("Could not start: {e}");
        std::process::exit(1);
    }

    let quit = Arc::new(AtomicBool::new(false));
    let q = quit.clone();
    let _ = ctrlc::set_handler(move || q.store(true, Ordering::Relaxed));

    println!(
        "{} backend: {} frames at {} Hz ({:.2} ms). {}",
        backend.name(),
        backend.buffer_size(),
        backend.sample_rate(),
        f64::from(backend.buffer_size()) / backend.sample_rate() * 1000.0,
        if args.midi {
            "Play notes on the MIDI input."
        } else {
            "Playing 440 Hz."
        }
    );

    let begin = Instant::now();
    let (mut warned_rt, mut warned_size, mut peak_load) = (false, false, 0f32);
    while !quit.load(Ordering::Relaxed)
        && args.seconds.is_none_or(|s| begin.elapsed().as_secs() < s)
    {
        std::thread::sleep(Duration::from_secs(1));
        let s = backend.stats();
        let load = s.dsp_load.unwrap_or(0.0);
        peak_load = peak_load.max(load);
        println!(
            "{:>5}s  {:>4} frames  xruns {:<4} DSP {:>5.1}%  RT {}",
            begin.elapsed().as_secs(),
            backend.buffer_size(),
            s.xruns,
            load,
            yes_no(s.realtime)
        );
        // Give PipeWire a few seconds to promote the thread before warning.
        if s.realtime == Some(false) && !warned_rt && begin.elapsed() > Duration::from_secs(3) {
            warned_rt = true;
            eprintln!(
                "warning: the audio thread did not get real-time scheduling, so xruns are likely.\n  \
                 PipeWire gets it through its RT module: directly (needs RLIMIT_RTPRIO, e.g. the Arch\n  \
                 'realtime' group from realtime-privileges), via rtkit on the system bus, or via the\n  \
                 xdg-desktop-portal Realtime interface (the route that works inside distrobox)."
            );
        }
        if let Some(want) = args.frames
            && backend.buffer_size() != want
            && !warned_size
        {
            warned_size = true;
            eprintln!(
                "warning: asked for {want} frames but the server runs {}. On PipeWire, force it with\n  \
                 pw-metadata -n settings 0 clock.force-quantum {want}   (0 to undo)",
                backend.buffer_size()
            );
        }
        if s.rt_alloc_violations > 0 {
            eprintln!(
                "bug: {} allocations on the audio thread",
                s.rt_alloc_violations
            );
        }
        if s.server_gone {
            eprintln!("The audio server went away.");
            break;
        }
    }

    let s = backend.stats();
    let _ = backend.close();
    println!(
        "\nSummary: {:.0} s, {} cycles, {} xruns, peak DSP {:.1}%, real-time {}. {}",
        begin.elapsed().as_secs_f64(),
        s.cycles,
        s.xruns,
        peak_load,
        yes_no(s.realtime),
        if s.xruns == 0 { "PASS" } else { "FAIL" }
    );
    std::process::exit(if s.xruns == 0 { 0 } else { 1 });
}
