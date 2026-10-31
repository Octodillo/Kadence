use std::collections::BTreeSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use daw_core::event::EventList;
use daw_core::queue::{Consumer, Producer, spsc};

use crate::cycle::{PortBuffers, push_or_count};
use crate::stats::{SharedStats, current_thread_is_realtime};
use crate::{
    AudioBackend, BackendConfig, BackendCycle, BackendError, BackendStats, Latency, PortDirection,
    PortFilter, PortInfo, PortKind, ProcessCallback, midi,
};
use crate::{MAX_MIDI_INPUTS, MidiInputPort, MidiInputs, MidiInputsControl, midi_inputs};

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

#[derive(Debug)]
struct World {
    /// Other clients' ports, as `list_ports` shows them.
    others: Vec<PortInfo>,
    connections: BTreeSet<(String, String)>,
}

/// The pretend system around a [`DummyBackend`]: other clients' ports, to
/// add and remove the way devices are plugged in and out.
#[derive(Clone, Debug)]
pub struct DummySystem(Arc<Mutex<World>>);

impl DummySystem {
    fn world(&self) -> std::sync::MutexGuard<'_, World> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Another client's port appears. `name` is the full `client:port`.
    pub fn add_port(
        &self,
        name: &str,
        kind: PortKind,
        direction: PortDirection,
        is_physical: bool,
    ) {
        let mut w = self.world();
        w.others.retain(|p| p.name != name);
        w.others.push(PortInfo {
            name: name.into(),
            kind,
            direction,
            is_physical,
            is_ours: false,
        });
    }

    /// Another client's port goes away, and its connections with it.
    pub fn remove_port(&self, name: &str) {
        let mut w = self.world();
        w.others.retain(|p| p.name != name);
        w.connections.retain(|(a, b)| a != name && b != name);
    }

    /// Current connections as `(output, input)` pairs.
    pub fn connections(&self) -> Vec<(String, String)> {
        self.world().connections.iter().cloned().collect()
    }
}

/// A backend with no audio hardware: it runs the callback from an ordinary
/// thread, feeds silence to the inputs and throws the outputs away. It
/// pretends to be a system with two capture and two playback ports and one
/// MIDI port each way, so port listing and connection can be tested.
pub struct DummyBackend {
    pacing: Pacing,
    max_cycles: Option<u64>,
    /// Feed the outputs back into the inputs this late; see
    /// [`with_loopback`](Self::with_loopback).
    loopback: Option<Latency>,
    cfg: Option<BackendConfig>,
    shared: Arc<SharedStats>,
    /// Other clients' ports and every connection, shared with
    /// [`DummySystem`] handles.
    world: Arc<Mutex<World>>,
    midi_in_rx: Option<Consumer<RawMessage>>,
    midi_out_tx: Option<Producer<RawMessage>>,
    running: Option<Running>,
    /// The extra MIDI inputs while stopped; the worker has them while
    /// running.
    extra: Option<MidiInputs<Consumer<RawMessage>>>,
    extra_control: Option<MidiInputsControl<Consumer<RawMessage>>>,
    /// The sending ends of the extra inputs, by slot, until taken by
    /// [`DummyBackend::midi_input_injector`].
    extra_tx: Vec<Option<Producer<RawMessage>>>,
}

struct Running {
    stop: Arc<AtomicBool>,
    /// The worker, and whether the callback panicked.
    thread: JoinHandle<(Worker, bool)>,
}

impl DummyBackend {
    pub fn new(pacing: Pacing) -> Self {
        Self {
            pacing,
            max_cycles: None,
            loopback: None,
            cfg: None,
            shared: Arc::new(SharedStats::new()),
            world: Arc::new(Mutex::new(World {
                others: Self::system_ports(),
                connections: BTreeSet::new(),
            })),
            midi_in_rx: None,
            midi_out_tx: None,
            running: None,
            extra: None,
            extra_control: None,
            extra_tx: (0..MAX_MIDI_INPUTS).map(|_| None).collect(),
        }
    }

    /// The pretend system: two capture and two playback ports, and one MIDI
    /// port each way.
    fn system_ports() -> Vec<PortInfo> {
        use PortDirection::{Input, Output};
        use PortKind::{Audio, Midi};
        let port = |name: String, kind, direction| PortInfo {
            name: format!("{SYSTEM}:{name}"),
            kind,
            direction,
            is_physical: true,
            is_ours: false,
        };
        let mut v = Vec::new();
        for i in 1..=2 {
            v.push(port(format!("capture_{i}"), Audio, Output));
            v.push(port(format!("playback_{i}"), Audio, Input));
        }
        v.push(port("midi_capture_1".into(), Midi, Output));
        v.push(port("midi_playback_1".into(), Midi, Input));
        v
    }

    /// A handle on the pretend system's other ports, for tests to plug
    /// devices in and out while the backend is in use elsewhere.
    pub fn system(&self) -> DummySystem {
        DummySystem(self.world.clone())
    }

    fn world(&self) -> std::sync::MutexGuard<'_, World> {
        self.world
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The sending end of the port in `slot` made by
    /// [`add_midi_input`](AudioBackend::add_midi_input). Each port has one;
    /// later calls return `None`.
    pub fn midi_input_injector(&mut self, slot: usize) -> Option<MidiInjector> {
        self.extra_tx.get_mut(slot)?.take().map(MidiInjector)
    }

    /// While stopped, the extra MIDI inputs take changes at once.
    fn apply_extra_now(&mut self) {
        if let Some(extra) = &mut self.extra {
            extra.begin_cycle();
        }
        if let Some(control) = &mut self.extra_control {
            control.take_removed().for_each(drop);
        }
    }

    /// Stop by itself after `n` cycles (for tests). `is_running` stays true
    /// until [`stop`](AudioBackend::stop) is called.
    pub fn with_max_cycles(mut self, n: u64) -> Self {
        self.max_cycles = Some(n);
        self
    }

    /// Wires each output back into the input of the same number, as a cable
    /// from the sound card's output to its input would, and reports
    /// `latency`: what comes out of the outputs arrives in the inputs
    /// `latency.total()` frames later. For testing recording and latency
    /// compensation. A total shorter than the period is raised to one
    /// period, since a cycle's inputs are read before its outputs exist.
    pub fn with_loopback(mut self, latency: Latency) -> Self {
        self.loopback = Some(latency);
        self
    }

    /// Creates the MIDI input queue, replacing any previous injector. Made
    /// while running, it delivers from the next `start` on; the running
    /// cycle keeps reading the previous one until `stop`.
    pub fn midi_injector(&mut self) -> MidiInjector {
        let (tx, rx) = spsc(RING_CAPACITY);
        self.midi_in_rx = Some(rx);
        MidiInjector(tx)
    }

    /// Creates the MIDI output tap, replacing any previous tap. Made while
    /// running, it receives from the next `start` on.
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
        let mut v = self.world().others.clone();
        let mut add = |client: &str, name: String, kind, direction| {
            v.push(PortInfo {
                name: format!("{client}:{name}"),
                kind,
                direction,
                is_physical: false,
                is_ours: true,
            })
        };
        use PortDirection::{Input, Output};
        use PortKind::{Audio, Midi};
        if let Some(cfg) = &self.cfg {
            let me = cfg.client_name.as_str();
            for i in 1..=cfg.audio_inputs {
                add(me, format!("audio_in_{i}"), Audio, Input);
            }
            for i in 1..=cfg.audio_outputs {
                add(me, format!("audio_out_{i}"), Audio, Output);
            }
            if cfg.midi_input {
                add(me, "midi_in".into(), Midi, Input);
            }
            if cfg.midi_output {
                add(me, "midi_out".into(), Midi, Output);
            }
        }
        if let Some(control) = &self.extra_control {
            for (_, name) in control.names() {
                v.push(PortInfo {
                    name: name.to_owned(),
                    kind: Midi,
                    direction: Input,
                    is_physical: false,
                    is_ours: true,
                });
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
        self.world().connections.extend(pairs);
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
        self.world().connections.clear();
        let (extra, control) = midi_inputs(cfg.midi_capacity);
        self.extra = Some(extra);
        self.extra_control = Some(control);
        self.extra_tx.iter_mut().for_each(|tx| *tx = None);
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
            loopback: self.loopback.map(|l| {
                let delay = l.total().max(frames) as usize;
                Loopback::new(usize::from(cfg.audio_inputs), delay, frames as usize)
            }),
            // Without the port, the queue stays here for a later `open`.
            midi_in_rx: if cfg.midi_input {
                self.midi_in_rx.take()
            } else {
                None
            },
            midi_out_tx: if cfg.midi_output {
                self.midi_out_tx.take()
            } else {
                None
            },
            extra: self.extra.take().ok_or(BackendError::NotOpen)?,
            frame_time: 0,
        };
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let thread = std::thread::Builder::new()
            .name(format!("{}-dummy-audio", cfg.client_name))
            .spawn(move || {
                // Catch a panic so `stop` gets the worker, and its queues, back.
                let panicked = catch_unwind(AssertUnwindSafe(|| worker.run(&stop2))).is_err();
                (worker, panicked)
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
        // Wake the worker from its wait for the next period.
        running.thread.thread().unpark();
        let (worker, panicked) = running
            .thread
            .join()
            .map_err(|_| BackendError::Other("the dummy audio thread panicked".into()))?;
        // Keep the queues so the injector and tap survive a restart. One made
        // while running is newer than the worker's and wins. The callback
        // drops here, on the caller's thread.
        let Worker {
            midi_in_rx,
            midi_out_tx,
            extra,
            ..
        } = worker;
        self.midi_in_rx = self.midi_in_rx.take().or(midi_in_rx);
        self.midi_out_tx = self.midi_out_tx.take().or(midi_out_tx);
        self.extra = Some(extra);
        self.apply_extra_now();
        if panicked {
            return Err(BackendError::Other("the process callback panicked".into()));
        }
        Ok(())
    }

    fn close(&mut self) -> Result<(), BackendError> {
        self.stop()?;
        self.cfg = None;
        self.world().connections.clear();
        self.extra = None;
        self.extra_control = None;
        self.extra_tx.iter_mut().for_each(|tx| *tx = None);
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
        self.world().connections.insert((from.into(), to.into()));
        Ok(())
    }

    fn disconnect(&mut self, from: &str, to: &str) -> Result<(), BackendError> {
        self.cfg()?;
        if self.world().connections.remove(&(from.into(), to.into())) {
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

    fn add_midi_input(&mut self, name: &str) -> Result<MidiInputPort, BackendError> {
        let me = self.cfg()?.client_name.clone();
        let full = format!("{me}:{name}");
        if name.is_empty() || name.contains(':') || self.port(&full).is_ok() {
            return Err(BackendError::Port(name.into()));
        }
        let control = self.extra_control.as_mut().ok_or(BackendError::NotOpen)?;
        let slot = control.free_slot().ok_or_else(|| {
            BackendError::Config(format!("at most {MAX_MIDI_INPUTS} MIDI inputs"))
        })?;
        let (tx, rx) = spsc(RING_CAPACITY);
        if control.add(slot, full.clone(), rx).is_err() {
            return Err(BackendError::Other(
                "the audio thread has not caught up with earlier port changes".into(),
            ));
        }
        self.extra_tx[slot] = Some(tx);
        self.apply_extra_now();
        Ok(MidiInputPort { slot, name: full })
    }

    fn remove_midi_input(&mut self, slot: usize) -> Result<(), BackendError> {
        let control = self.extra_control.as_mut().ok_or(BackendError::NotOpen)?;
        let name = control
            .names()
            .find(|&(s, _)| s == slot)
            .map(|(_, n)| n.to_owned())
            .ok_or_else(|| BackendError::Config(format!("no MIDI input in slot {slot}")))?;
        if !control.remove(slot) {
            return Err(BackendError::Other(
                "the audio thread has not caught up with earlier port changes".into(),
            ));
        }
        self.extra_tx[slot] = None;
        self.world()
            .connections
            .retain(|(a, b)| *a != name && *b != name);
        self.apply_extra_now();
        Ok(())
    }

    fn midi_inputs(&self) -> Vec<MidiInputPort> {
        self.extra_control.as_ref().map_or_else(Vec::new, |c| {
            c.names()
                .map(|(slot, name)| MidiInputPort {
                    slot,
                    name: name.to_owned(),
                })
                .collect()
        })
    }

    fn latency(&self) -> Latency {
        let Some(l) = self.loopback else {
            return Latency::default();
        };
        // Report what `start` will really use, raised to a period.
        let frames = self.shared.buffer_size.load(Relaxed);
        Latency {
            input: l.input,
            output: l.output.max(frames.saturating_sub(l.input)),
        }
    }
}

/// Delay lines from the outputs back to the inputs: one ring per input,
/// written after each cycle and read before the next.
struct Loopback {
    rings: Vec<Vec<f32>>,
    /// Ring index of the next sample to read.
    read: usize,
    delay: usize,
}

impl Loopback {
    fn new(channels: usize, delay: usize, frames: usize) -> Self {
        Self {
            rings: vec![vec![0.0; delay + frames]; channels],
            read: 0,
            delay,
        }
    }

    /// Fills `inputs` with what was played `delay` frames ago.
    fn read(&mut self, inputs: &mut [Vec<f32>], frames: usize) {
        for (ring, input) in self.rings.iter().zip(inputs.iter_mut()) {
            let len = ring.len();
            for (i, s) in input[..frames].iter_mut().enumerate() {
                *s = ring[(self.read + i) % len];
            }
        }
    }

    /// Stores this cycle's outputs to be read `delay` frames from now, and
    /// moves on a cycle.
    fn write(&mut self, outputs: &[Vec<f32>], frames: usize) {
        for (ch, ring) in self.rings.iter_mut().enumerate() {
            let len = ring.len();
            let start = self.read + self.delay;
            for i in 0..frames {
                ring[(start + i) % len] = outputs.get(ch).map_or(0.0, |o| o[i]);
            }
        }
        self.read = (self.read + frames) % self.rings.first().map_or(1, |r| r.len().max(1));
    }
}

impl DummyBackend {
    /// Current connections as `(output, input)` pairs.
    pub fn connections(&self) -> Vec<(String, String)> {
        self.world().connections.iter().cloned().collect()
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
    extra: MidiInputs<Consumer<RawMessage>>,
    frame_time: u64,
    loopback: Option<Loopback>,
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
                    wait_until(deadline, stop);
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
        if let Some(l) = &mut self.loopback {
            l.read(&mut self.inputs, frames as usize);
        }
        let extra = &mut self.extra;
        extra.begin_cycle();
        for (rx, list) in extra.ports.iter_mut().zip(&mut extra.lists) {
            let Some(rx) = rx else { continue };
            while let Some(m) = rx.pop() {
                if let Some(e) = midi::decode(0, m.as_bytes()) {
                    push_or_count(list, e, &mut dropped);
                }
            }
        }
        let xruns = self.shared.xruns.load(Relaxed);
        let (frame_time, sample_rate) = (self.frame_time, self.sample_rate);
        let (cb, midi_in, midi_out) = (&mut self.cb, &self.midi_in, &mut self.midi_out);
        let midi_inputs = &self.extra.lists[..];
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
                    midi_inputs,
                    midi_out,
                    xruns,
                })
            },
        );
        if let Some(tx) = &mut self.midi_out_tx {
            for e in self.midi_out.iter() {
                let mut bytes = [0; 3];
                let Some(len) = midi::encode(&e.kind, &mut bytes) else {
                    dropped += u64::from(midi::is_malformed(&e.kind));
                    continue;
                };
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
        if let Some(l) = &mut self.loopback {
            l.write(&self.outputs, frames as usize);
        }
        self.frame_time += u64::from(frames);
        self.shared.frames.fetch_add(u64::from(frames), Relaxed);
        self.shared.midi_dropped.fetch_add(dropped, Relaxed);
        self.shared.cycles.fetch_add(1, Relaxed);
    }
}

/// Parks until `deadline`, or until `stop` is set and the thread unparked, so
/// `stop()` never waits out a long period.
fn wait_until(deadline: Instant, stop: &AtomicBool) {
    loop {
        let now = Instant::now();
        if now >= deadline || stop.load(Relaxed) {
            return;
        }
        std::thread::park_timeout(deadline - now);
    }
}
