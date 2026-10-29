use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use daw_core::event::EventList;
use daw_core::queue::{Consumer, Producer, spsc};

use crate::cycle::{PortBuffers, push_or_count};
use crate::stats::{SharedStats, current_thread_is_realtime};
use crate::{
    AudioBackend, BackendConfig, BackendCycle, BackendError, BackendStats, PortDirection,
    PortFilter, PortInfo, PortKind, ProcessCallback, midi,
};

const DEFAULT_RATE: u32 = 48_000;
const DEFAULT_FRAMES: u32 = 256;
const RING_CAPACITY: usize = 4096;
const SYSTEM: &str = "system";

/// How the dummy backend paces its cycles.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Pacing {
    /// One cycle per period of wall-clock time, like a sound card. A cycle
    /// that finishes more than one period after its deadline is an xrun.
    RealTime,
    /// Back-to-back cycles as fast as the callback allows.
    Freewheel,
}

/// A raw MIDI message for the dummy backend's injector and tap.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct RawMessage {
    /// For tapped output: the absolute frame (`frame_time + event frame`).
    /// Ignored on injection.
    pub frame: u64,
    pub bytes: [u8; 3],
    pub len: u8,
}

impl RawMessage {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

/// Feeds MIDI into a [`DummyBackend`]'s input from any thread. Each message
/// arrives at frame 0 of the next cycle.
pub struct MidiInjector(Producer<RawMessage>);

impl MidiInjector {
    /// Queues one message of 1 to 3 bytes. Returns `false` if the message is
    /// too long or the queue is full.
    pub fn send(&mut self, bytes: &[u8]) -> bool {
        if bytes.is_empty() || bytes.len() > 3 {
            return false;
        }
        let mut raw = [0; 3];
        raw[..bytes.len()].copy_from_slice(bytes);
        self.0
            .push(RawMessage {
                frame: 0,
                bytes: raw,
                len: bytes.len() as u8,
            })
            .is_ok()
    }
}

/// Receives the MIDI a [`DummyBackend`]'s callback writes to `midi_out`.
pub struct MidiTap(Consumer<RawMessage>);

impl MidiTap {
    pub fn try_recv(&mut self) -> Option<RawMessage> {
        self.0.pop()
    }
}

/// A backend with no audio hardware: it runs the callback from an ordinary
/// thread, feeds silence to the inputs and throws the outputs away. It
/// pretends to be a system with two capture and two playback ports and one
/// MIDI port each way, so port listing and connection can be tested.
pub struct DummyBackend {
    pacing: Pacing,
    max_cycles: Option<u64>,
    cfg: Option<BackendConfig>,
    shared: Arc<SharedStats>,
    connections: BTreeSet<(String, String)>,
    midi_in_rx: Option<Consumer<RawMessage>>,
    midi_out_tx: Option<Producer<RawMessage>>,
    running: Option<Running>,
}

struct Running {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<Worker>,
}

impl DummyBackend {
    pub fn new(pacing: Pacing) -> Self {
        Self {
            pacing,
            max_cycles: None,
            cfg: None,
            shared: Arc::new(SharedStats::new()),
            connections: BTreeSet::new(),
            midi_in_rx: None,
            midi_out_tx: None,
            running: None,
        }
    }

    /// Stop by itself after `n` cycles (for tests). `is_running` stays true
    /// until [`stop`](AudioBackend::stop) is called.
    pub fn with_max_cycles(mut self, n: u64) -> Self {
        self.max_cycles = Some(n);
        self
    }

    /// Creates the MIDI input queue. Call before `start`; a later call
    /// replaces the previous injector.
    pub fn midi_injector(&mut self) -> MidiInjector {
        let (tx, rx) = spsc(RING_CAPACITY);
        self.midi_in_rx = Some(rx);
        MidiInjector(tx)
    }

    /// Creates the MIDI output tap. Call before `start`.
    pub fn midi_tap(&mut self) -> MidiTap {
        let (tx, rx) = spsc(RING_CAPACITY);
        self.midi_out_tx = Some(tx);
        MidiTap(rx)
    }

    /// Blocks until the worker has run at least `n` cycles or `timeout`
    /// passes. Returns whether it got there.
    pub fn wait_for_cycles(&self, n: u64, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        while self.shared.cycles.load(Relaxed) < n {
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        true
    }

    fn cfg(&self) -> Result<&BackendConfig, BackendError> {
        self.cfg.as_ref().ok_or(BackendError::NotOpen)
    }

    fn all_ports(&self) -> Vec<PortInfo> {
        let mut v = Vec::new();
        let mut add = |client: &str, name: String, kind, direction, ours: bool| {
            v.push(PortInfo {
                name: format!("{client}:{name}"),
                kind,
                direction,
                is_physical: !ours,
                is_ours: ours,
            })
        };
        use PortDirection::{Input, Output};
        use PortKind::{Audio, Midi};
        for i in 1..=2 {
            add(SYSTEM, format!("capture_{i}"), Audio, Output, false);
            add(SYSTEM, format!("playback_{i}"), Audio, Input, false);
        }
        add(SYSTEM, "midi_capture_1".into(), Midi, Output, false);
        add(SYSTEM, "midi_playback_1".into(), Midi, Input, false);
        if let Some(cfg) = &self.cfg {
            let me = cfg.client_name.as_str();
            for i in 1..=cfg.audio_inputs {
                add(me, format!("audio_in_{i}"), Audio, Input, true);
            }
            for i in 1..=cfg.audio_outputs {
                add(me, format!("audio_out_{i}"), Audio, Output, true);
            }
            if cfg.midi_input {
                add(me, "midi_in".into(), Midi, Input, true);
            }
            if cfg.midi_output {
                add(me, "midi_out".into(), Midi, Output, true);
            }
        }
        v
    }

    fn port(&self, name: &str) -> Result<PortInfo, String> {
        self.all_ports()
            .into_iter()
            .find(|p| p.name == name)
            .ok_or_else(|| format!("no port named {name}"))
    }

    fn auto_connect(&mut self) {
        let ports = self.all_ports();
        let pick = |ours: bool, kind, dir| {
            ports
                .iter()
                .filter(move |p| p.is_ours == ours && p.kind == kind && p.direction == dir)
                .map(|p| p.name.clone())
        };
        use PortDirection::{Input, Output};
        use PortKind::{Audio, Midi};
        let pairs: Vec<_> = pick(true, Audio, Output)
            .zip(pick(false, Audio, Input))
            .chain(pick(false, Audio, Output).zip(pick(true, Audio, Input)))
            .chain(pick(false, Midi, Output).zip(pick(true, Midi, Input)))
            .collect();
        self.connections.extend(pairs);
    }
}

impl Default for DummyBackend {
    fn default() -> Self {
        Self::new(Pacing::RealTime)
    }
}

impl AudioBackend for DummyBackend {
    fn name(&self) -> &'static str {
        "Dummy"
    }

    fn open(&mut self, cfg: BackendConfig) -> Result<(), BackendError> {
        if self.cfg.is_some() {
            return Err(BackendError::AlreadyOpen);
        }
        let rate = cfg.sample_rate.unwrap_or(DEFAULT_RATE);
        let frames = cfg.buffer_size.unwrap_or(DEFAULT_FRAMES);
        if rate == 0 || frames == 0 {
            return Err(BackendError::Config(
                "sample rate and buffer size must be non-zero".into(),
            ));
        }
        self.shared = Arc::new(SharedStats::new());
        self.shared.sample_rate.store(rate, Relaxed);
        self.shared.buffer_size.store(frames, Relaxed);
        self.connections.clear();
        self.cfg = Some(cfg);
        Ok(())
    }

    fn start(&mut self, cb: ProcessCallback) -> Result<(), BackendError> {
        if self.running.is_some() {
            return Err(BackendError::AlreadyRunning);
        }
        let cfg = self.cfg()?.clone();
        let frames = self.shared.buffer_size.load(Relaxed);
        let mut worker = Worker {
            cb,
            shared: self.shared.clone(),
            pacing: self.pacing,
            max_cycles: self.max_cycles,
            frames,
            sample_rate: f64::from(self.shared.sample_rate.load(Relaxed)),
            inputs: vec![vec![0.0; frames as usize]; usize::from(cfg.audio_inputs)],
            outputs: vec![vec![0.0; frames as usize]; usize::from(cfg.audio_outputs)],
            bufs: PortBuffers::new(cfg.audio_inputs.into(), cfg.audio_outputs.into()),
            midi_in: EventList::with_capacity(cfg.midi_capacity),
            midi_out: EventList::with_capacity(cfg.midi_capacity),
            midi_in_rx: self.midi_in_rx.take().filter(|_| cfg.midi_input),
            midi_out_tx: self.midi_out_tx.take().filter(|_| cfg.midi_output),
            frame_time: 0,
        };
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let thread = std::thread::Builder::new()
            .name(format!("{}-dummy-audio", cfg.client_name))
            .spawn(move || {
                worker.run(&stop2);
                worker
            })
            .map_err(|e| BackendError::Other(e.to_string()))?;
        self.running = Some(Running { stop, thread });
        if cfg.auto_connect {
            self.auto_connect();
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<(), BackendError> {
        let Some(running) = self.running.take() else {
            return Ok(());
        };
        running.stop.store(true, Relaxed);
        let worker = running
            .thread
            .join()
            .map_err(|_| BackendError::Other("the process callback panicked".into()))?;
        // Keep the queues so the injector and tap survive a restart; the
        // callback drops here, on the caller's thread.
        let Worker {
            midi_in_rx,
            midi_out_tx,
            ..
        } = worker;
        self.midi_in_rx = midi_in_rx.or(self.midi_in_rx.take());
        self.midi_out_tx = midi_out_tx.or(self.midi_out_tx.take());
        Ok(())
    }

    fn close(&mut self) -> Result<(), BackendError> {
        self.stop()?;
        self.cfg = None;
        self.connections.clear();
        Ok(())
    }

    fn is_running(&self) -> bool {
        self.running.is_some()
    }

    fn sample_rate(&self) -> f64 {
        f64::from(self.shared.sample_rate.load(Relaxed))
    }

    fn buffer_size(&self) -> u32 {
        self.shared.buffer_size.load(Relaxed)
    }

    fn list_ports(&self, filter: &PortFilter) -> Result<Vec<PortInfo>, BackendError> {
        self.cfg()?;
        Ok(self
            .all_ports()
            .into_iter()
            .filter(|p| filter.kind.is_none_or(|k| k == p.kind))
            .filter(|p| filter.direction.is_none_or(|d| d == p.direction))
            .filter(|p| !filter.physical_only || p.is_physical)
            .collect())
    }

    fn connect(&mut self, from: &str, to: &str) -> Result<(), BackendError> {
        self.cfg()?;
        let err = |reason: String| BackendError::Connection {
            from: from.into(),
            to: to.into(),
            reason,
        };
        let a = self.port(from).map_err(err)?;
        let b = self.port(to).map_err(err)?;
        if a.direction != PortDirection::Output || b.direction != PortDirection::Input {
            return Err(err("must connect an output to an input".into()));
        }
        if a.kind != b.kind {
            return Err(err("port types differ".into()));
        }
        self.connections.insert((from.into(), to.into()));
        Ok(())
    }

    fn disconnect(&mut self, from: &str, to: &str) -> Result<(), BackendError> {
        self.cfg()?;
        if self.connections.remove(&(from.into(), to.into())) {
            Ok(())
        } else {
            Err(BackendError::Connection {
                from: from.into(),
                to: to.into(),
                reason: "not connected".into(),
            })
        }
    }

    fn stats(&self) -> BackendStats {
        self.shared.snapshot()
    }
}

impl DummyBackend {
    /// Current connections as `(output, input)` pairs.
    pub fn connections(&self) -> impl Iterator<Item = (&str, &str)> {
        self.connections
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
    }
}

impl Drop for DummyBackend {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

struct Worker {
    cb: ProcessCallback,
    shared: Arc<SharedStats>,
    pacing: Pacing,
    max_cycles: Option<u64>,
    frames: u32,
    sample_rate: f64,
    inputs: Vec<Vec<f32>>,
    outputs: Vec<Vec<f32>>,
    bufs: PortBuffers,
    midi_in: EventList,
    midi_out: EventList,
    midi_in_rx: Option<Consumer<RawMessage>>,
    midi_out_tx: Option<Producer<RawMessage>>,
    frame_time: u64,
}

impl Worker {
    fn run(&mut self, stop: &AtomicBool) {
        let shared = self.shared.clone();
        shared.set_realtime(current_thread_is_realtime());
        let period = Duration::from_secs_f64(f64::from(self.frames) / self.sample_rate);
        let mut deadline = Instant::now() + period;
        let mut cycles = 0u64;
        while !stop.load(Relaxed) && self.max_cycles.is_none_or(|m| cycles < m) {
            let started = Instant::now();
            crate::rt_guard(&shared, || self.cycle());
            cycles += 1;
            if self.pacing == Pacing::RealTime {
                let now = Instant::now();
                let load = (now - started).as_secs_f32() / period.as_secs_f32() * 100.0;
                self.shared.set_dsp_load(load);
                // Like a sound card with two periods of buffering: being up
                // to one period late is absorbed, more is an xrun.
                if now > deadline + period {
                    self.shared.xruns.fetch_add(1, Relaxed);
                    deadline = now;
                } else if now > deadline {
                    deadline = now;
                } else {
                    std::thread::sleep(deadline - now);
                }
                deadline += period;
            }
        }
    }

    fn cycle(&mut self) {
        let frames = self.frames;
        let mut dropped = 0;
        self.midi_in.clear();
        self.midi_out.clear();
        if let Some(rx) = &mut self.midi_in_rx {
            while let Some(m) = rx.pop() {
                if let Some(e) = midi::decode(0, m.as_bytes()) {
                    push_or_count(&mut self.midi_in, e, &mut dropped);
                }
            }
        }
        let xruns = self.shared.xruns.load(Relaxed);
        let (frame_time, sample_rate) = (self.frame_time, self.sample_rate);
        let (cb, midi_in, midi_out) = (&mut self.cb, &self.midi_in, &mut self.midi_out);
        self.bufs.run(
            self.inputs.iter().map(|b| &b[..frames as usize]),
            self.outputs.iter_mut().map(|b| &mut b[..frames as usize]),
            |audio_in, audio_out| {
                cb(&mut BackendCycle {
                    frames,
                    sample_rate,
                    frame_time,
                    audio_in,
                    audio_out,
                    midi_in,
                    midi_out,
                    xruns,
                })
            },
        );
        if let Some(tx) = &mut self.midi_out_tx {
            for e in self.midi_out.iter() {
                let mut bytes = [0; 3];
                if let Some(len) = midi::encode(&e.kind, &mut bytes) {
                    let msg = RawMessage {
                        frame: frame_time + u64::from(e.frame),
                        bytes,
                        len: len as u8,
                    };
                    if tx.push(msg).is_err() {
                        dropped += 1;
                    }
                }
            }
        }
        self.frame_time += u64::from(frames);
        self.shared.frames.fetch_add(u64::from(frames), Relaxed);
        self.shared.midi_dropped.fetch_add(dropped, Relaxed);
        self.shared.cycles.fetch_add(1, Relaxed);
    }
}
