//! Audio and MIDI I/O for the DAW.
//!
//! An [`AudioBackend`] opens ports on some audio system and calls one process
//! callback per audio period with a [`BackendCycle`]. Two backends ship here:
//!
//! - [`JackBackend`] talks to a JACK server. On a Plasma desktop that server is
//!   PipeWire through its `libjack` replacement (`pipewire-jack`), so this is
//!   also our PipeWire backend. `libjack` is loaded at run time, so building
//!   needs no JACK headers.
//! - [`DummyBackend`] drives the callback from an ordinary thread, either at
//!   real-time pace or as fast as possible. Tests use it.
//!
//! The callback runs on the real-time thread. It must not allocate, lock, log
//! or do I/O. In debug builds, when the binary installs [`RtAllocCounter`] as
//! its global allocator, allocations in a cycle show up in
//! [`BackendStats::rt_alloc_violations`].

mod alloc;
mod cycle;
mod dummy;
mod jack_backend;
pub mod midi;
mod stats;

pub use alloc::RtAllocCounter;
pub use cycle::BackendCycle;
pub(crate) use cycle::{MidiInputs, MidiInputsControl, midi_inputs};
pub use dummy::{DummyBackend, DummySystem, MidiInjector, MidiTap, Pacing};
pub use jack_backend::JackBackend;
pub use stats::BackendStats;

/// Most MIDI input ports [`AudioBackend::add_midi_input`] adds at once.
pub const MAX_MIDI_INPUTS: usize = 32;

/// A MIDI input port made by [`AudioBackend::add_midi_input`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MidiInputPort {
    /// Its events are in [`BackendCycle::midi_inputs`]`[slot]`.
    pub slot: usize,
    /// Full name, `client:port`, as passed to [`AudioBackend::connect`].
    pub name: String,
}

/// The process callback a backend calls once per audio period.
pub type ProcessCallback = Box<dyn FnMut(&mut BackendCycle) + Send>;

/// What to open: how many ports, and what the audio system should run at.
#[derive(Clone, Debug)]
pub struct BackendConfig {
    /// Client name shown in patchbays (qpwgraph, Helvum, Carla).
    pub client_name: String,
    pub audio_inputs: u16,
    pub audio_outputs: u16,
    pub midi_input: bool,
    pub midi_output: bool,
    /// Requested sample rate. JACK and PipeWire pick the rate themselves; a
    /// backend that cannot honour this reports the real rate from
    /// [`AudioBackend::sample_rate`]. The dummy backend uses it as given.
    pub sample_rate: Option<u32>,
    /// Requested period size in frames. JACK asks the server for it; the
    /// server may refuse or change it later. Callbacks must read
    /// [`BackendCycle::frames`] every cycle, never assume a fixed size.
    pub buffer_size: Option<u32>,
    /// Connect our ports to the system's physical ports after starting.
    pub auto_connect: bool,
    /// Capacity of the MIDI in and MIDI out event lists, per cycle. Events
    /// beyond it are dropped and counted in [`BackendStats::midi_dropped`].
    pub midi_capacity: usize,
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            client_name: "daw".into(),
            audio_inputs: 2,
            audio_outputs: 2,
            midi_input: true,
            midi_output: false,
            sample_rate: None,
            buffer_size: None,
            auto_connect: true,
            midi_capacity: 1024,
        }
    }
}

/// The kind of data a port carries.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum PortKind {
    Audio,
    Midi,
}

/// Direction from the port's own point of view: an `Output` port produces
/// data (a capture device, or our own outputs), an `Input` port consumes it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum PortDirection {
    Input,
    Output,
}

/// A port on the audio system, as listed by [`AudioBackend::list_ports`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortInfo {
    /// Full name, `client:port`, as passed to [`AudioBackend::connect`].
    pub name: String,
    pub kind: PortKind,
    pub direction: PortDirection,
    /// A hardware port (sound card, MIDI device) rather than another app.
    pub is_physical: bool,
    /// One of the ports this backend registered.
    pub is_ours: bool,
}

/// Which ports [`AudioBackend::list_ports`] returns. `None` matches all.
#[derive(Clone, Debug, Default)]
pub struct PortFilter {
    pub kind: Option<PortKind>,
    pub direction: Option<PortDirection>,
    pub physical_only: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("the backend is not open")]
    NotOpen,
    #[error("the backend is already open")]
    AlreadyOpen,
    #[error("the backend is already running")]
    AlreadyRunning,
    #[error("could not reach the audio server: {0}")]
    Connect(String),
    #[error("could not register port {0}")]
    Port(String),
    #[error("could not connect {from} to {to}: {reason}")]
    Connection {
        from: String,
        to: String,
        reason: String,
    },
    #[error("invalid configuration: {0}")]
    Config(String),
    #[error("{0}")]
    Other(String),
}

/// An audio and MIDI I/O system.
///
/// Lifecycle: [`open`](Self::open) registers ports, [`start`](Self::start)
/// begins calling the process callback, [`stop`](Self::stop) ends it and
/// drops the callback on the calling thread. `stop` then `start` again is
/// allowed, and keeps the connections made while open, including ones made
/// before the first `start`. Dropping a backend stops and closes it.
pub trait AudioBackend: Send {
    /// A short human-readable name, for settings pages ("JACK", "Dummy").
    fn name(&self) -> &'static str;

    fn open(&mut self, cfg: BackendConfig) -> Result<(), BackendError>;

    /// Begins calling `cb` once per period.
    ///
    /// If the audio system refuses to start (JACK: activation fails), the
    /// backend is left closed and must be [`open`](Self::open)ed again. In
    /// that case `cb` may never be dropped, because the JACK library can
    /// still hold it, so don't rely on its drop to signal anything.
    fn start(&mut self, cb: ProcessCallback) -> Result<(), BackendError>;

    fn stop(&mut self) -> Result<(), BackendError>;

    /// Stop if running, then unregister ports and disconnect from the server.
    fn close(&mut self) -> Result<(), BackendError>;

    fn is_running(&self) -> bool;

    /// Current sample rate in Hz. `0.0` before [`open`](Self::open).
    fn sample_rate(&self) -> f64;

    /// Current maximum period size in frames. It can change while running
    /// (PipeWire changes its quantum when other apps ask for lower latency).
    /// `0` before [`open`](Self::open).
    fn buffer_size(&self) -> u32;

    fn list_ports(&self, filter: &PortFilter) -> Result<Vec<PortInfo>, BackendError>;

    /// Connect an output port to an input port, both by full name.
    fn connect(&mut self, from: &str, to: &str) -> Result<(), BackendError>;

    fn disconnect(&mut self, from: &str, to: &str) -> Result<(), BackendError>;

    /// Counters shared with the real-time thread. Cheap; safe to poll from a
    /// UI timer.
    fn stats(&self) -> BackendStats;

    /// Registers another MIDI input port, `name` on this client, while open
    /// or running, so MIDI from different sources stays apart. Its events
    /// arrive in [`BackendCycle::midi_inputs`] from a cycle soon after. The
    /// port lasts until [`remove_midi_input`](Self::remove_midi_input) or
    /// [`close`](Self::close), across `stop` and `start`.
    fn add_midi_input(&mut self, name: &str) -> Result<MidiInputPort, BackendError>;

    /// Unregisters a port made by [`add_midi_input`](Self::add_midi_input).
    /// Its slot may be given to a later port.
    fn remove_midi_input(&mut self, slot: usize) -> Result<(), BackendError>;

    /// The ports made by [`add_midi_input`](Self::add_midi_input) that are
    /// still there.
    fn midi_inputs(&self) -> Vec<MidiInputPort>;
}

/// The short name for a port made from `name`: JACK separates client and
/// port with `:`, so that becomes `-`, and the length is capped.
pub fn port_short_name(name: &str) -> String {
    const MAX_CHARS: usize = 64;
    let cleaned: String = name
        .chars()
        .map(|c| if c == ':' || c.is_control() { '-' } else { c })
        .take(MAX_CHARS)
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "midi_in".into()
    } else {
        trimmed.to_owned()
    }
}

/// Runs one cycle body under the debug-build allocation guard, and counts any
/// allocation it catches in [`BackendStats::rt_alloc_violations`]. It only
/// counts: printing from the real-time thread would cost more than the
/// allocation it reports.
#[inline]
pub(crate) fn rt_guard<R>(shared: &stats::SharedStats, f: impl FnOnce() -> R) -> R {
    #[cfg(debug_assertions)]
    {
        let before = alloc::violations();
        let r = alloc::forbid(f);
        let caught = alloc::violations().wrapping_sub(before);
        if caught > 0 {
            shared
                .rt_alloc_violations
                .fetch_add(caught, std::sync::atomic::Ordering::Relaxed);
        }
        r
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = shared;
        f()
    }
}
