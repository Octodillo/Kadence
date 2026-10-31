//! Runs a [`Session`] on a real audio backend.
//!
//! [`AudioHost`] opens JACK (on Plasma that is PipeWire through
//! `pipewire-jack`) and moves the session's processor into the backend's
//! callback. If no audio server answers, it falls back to the dummy backend
//! pacing itself in real time, so the app still runs (silently) and the
//! playhead still moves.
//!
//! MIDI: every source the user picks ([`AudioHost::set_midi_inputs`]) gets a
//! port of its own, so a track can listen to one device. The host connects
//! each source to its port whenever the source is there, which also covers
//! a device plugged back in, as long as [`AudioHost::refresh_midi`] is
//! called now and then. Anything else can still be patched into the
//! general `midi_in` port, which every track listening to all inputs hears.

use std::collections::HashMap;

use daw_backend::{
    AudioBackend, BackendConfig, BackendError, DummyBackend, JackBackend, Latency, MAX_MIDI_INPUTS,
    MidiInputPort, Pacing, PortDirection, PortFilter, PortInfo, PortKind,
};
use daw_core::event::EventList;
use daw_core::time::TickTime;
use daw_engine::EngineConfig;

use crate::{Session, SessionConfig};

/// Largest block the engine processes at once; longer periods are split.
const MAX_FRAMES: u32 = 1024;

/// Which kind of backend is running.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HostKind {
    Jack,
    /// No audio server: a silent stand-in that keeps time.
    Silent,
}

/// What the user asked for. JACK servers decide the rate and period
/// themselves, so these are requests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostConfig {
    pub sample_rate: u32,
    pub buffer_size: u32,
    /// Try an audio server before falling back to silence.
    pub use_server: bool,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            buffer_size: 256,
            use_server: true,
        }
    }
}

/// A MIDI source another client offers, as [`AudioHost::midi_sources`]
/// lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MidiSource {
    /// Full port name, `client:port`; what settings and tracks store.
    pub name: String,
    /// A hardware device rather than another program.
    pub is_physical: bool,
}

/// A source the user picked, and the port of ours it plays into.
#[derive(Clone, Debug)]
struct MidiLink {
    source: String,
    port: MidiInputPort,
    /// Connected as far as we know; cleared when the source goes away, so
    /// it is connected again when it comes back.
    connected: bool,
}

/// A backend with a session running on it.
pub struct AudioHost {
    backend: Box<dyn AudioBackend>,
    kind: HostKind,
    session: Session,
    config: HostConfig,
    last_error: Option<String>,
    /// The MIDI sources the user picked, in order.
    midi_wanted: Vec<String>,
    midi: Vec<MidiLink>,
    /// Whether each MIDI source seen so far is hardware, so its port keeps
    /// the same name when it is made again while the source is unplugged
    /// (after a restart).
    midi_physical: HashMap<String, bool>,
}

/// Engine event source 0 is the general `midi_in` port; the port in backend
/// slot `i` is source `i + 1`.
fn engine_source(slot: usize) -> usize {
    slot + 1
}

impl AudioHost {
    /// Starts audio with `config`, falling back to a silent backend.
    pub fn start(config: HostConfig) -> Self {
        let mut last_error = None;
        if config.use_server {
            match Self::open(Box::new(JackBackend::new()), HostKind::Jack, &config) {
                Ok(host) => return host,
                Err(e) => last_error = Some(e.to_string()),
            }
        }
        let mut host = Self::open(
            Box::new(DummyBackend::new(Pacing::RealTime)),
            HostKind::Silent,
            &config,
        )
        .expect("the dummy backend always opens");
        host.last_error = last_error;
        host
    }

    /// Runs on `backend` (a [`DummyBackend`] in tests). It counts as
    /// [`HostKind::Silent`], and [`restart`](Self::restart) replaces it with
    /// the usual choice.
    pub fn with_backend(
        backend: Box<dyn AudioBackend>,
        config: &HostConfig,
    ) -> Result<Self, BackendError> {
        Self::open(backend, HostKind::Silent, config)
    }

    fn open(
        mut backend: Box<dyn AudioBackend>,
        kind: HostKind,
        config: &HostConfig,
    ) -> Result<Self, BackendError> {
        backend.open(BackendConfig {
            client_name: "Kadence".into(),
            audio_inputs: 2,
            audio_outputs: 2,
            midi_input: true,
            midi_output: false,
            sample_rate: Some(config.sample_rate),
            buffer_size: Some(config.buffer_size),
            // MIDI sources are connected to ports of their own below, not
            // all to `midi_in`, so connect audio here.
            auto_connect: false,
            midi_capacity: 1024,
        })?;
        let rate = backend.sample_rate();
        let (session, mut processor) = Session::new(SessionConfig {
            engine: EngineConfig {
                sample_rate: rate,
                max_frames: MAX_FRAMES,
                event_sources: 1 + MAX_MIDI_INPUTS,
                ..Default::default()
            },
            inputs: 2,
            outputs: 2,
        });
        // Backend MIDI arrives with frame offsets inside the period, which is
        // what the processor wants too.
        backend.start(Box::new(move |cycle| {
            // Source 0, then the port in each slot (see `engine_source`).
            let mut sources: [&EventList; 1 + MAX_MIDI_INPUTS] =
                [cycle.midi_in; 1 + MAX_MIDI_INPUTS];
            for (to, list) in sources[1..].iter_mut().zip(cycle.midi_inputs) {
                *to = list;
            }
            processor.process_sources(
                cycle.frames as usize,
                cycle.audio_in,
                cycle.audio_out,
                &sources,
            );
        }))?;
        connect_audio(backend.as_mut());
        Ok(Self {
            backend,
            kind,
            session,
            config: config.clone(),
            last_error: None,
            midi_wanted: Vec::new(),
            midi: Vec::new(),
            midi_physical: HashMap::new(),
        })
    }

    /// Gives each source in `sources` (full port names, see
    /// [`midi_sources`](Self::midi_sources)) a MIDI input port of its own
    /// and connects it when it is there; drops the ports of sources no
    /// longer listed. Kept across [`restart`](Self::restart). Returns the
    /// sources that could not get a port, with why.
    pub fn set_midi_inputs(&mut self, sources: &[String]) -> Vec<(String, BackendError)> {
        let mut wanted: Vec<String> = Vec::new();
        for s in sources {
            if !wanted.contains(s) {
                wanted.push(s.clone());
            }
        }
        self.midi_wanted = wanted;
        let mut failed = Vec::new();
        let mut i = 0;
        while i < self.midi.len() {
            if self.midi_wanted.contains(&self.midi[i].source) {
                i += 1;
                continue;
            }
            let link = self.midi.remove(i);
            if let Err(e) = self.backend.remove_midi_input(link.port.slot) {
                log::warn!("could not remove the MIDI input for {}: {e}", link.source);
            }
        }
        for source in self.midi_wanted.clone() {
            if self.midi.iter().any(|l| l.source == source) {
                continue;
            }
            match self.add_midi_link(&source) {
                Ok(link) => self.midi.push(link),
                Err(e) => failed.push((source, e)),
            }
        }
        self.refresh_midi();
        self.publish_midi_sources();
        failed
    }

    /// A port for `source`, named after it and unique among ours.
    fn add_midi_link(&mut self, source: &str) -> Result<MidiLink, BackendError> {
        for s in self.midi_sources() {
            self.midi_physical.insert(s.name, s.is_physical);
        }
        // A source never seen counts as a program.
        let physical = self.midi_physical.get(source).copied().unwrap_or(false);
        let base = midi_port_base(source, physical);
        // Every port of ours, not only the MIDI inputs: a hardware source
        // named like `x:audio_in_1` must not take an audio port's name.
        let all = PortFilter {
            kind: None,
            direction: None,
            physical_only: false,
        };
        let taken: Vec<String> = self
            .backend
            .list_ports(&all)
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.is_ours)
            .map(|p| {
                p.name
                    .split_once(':')
                    .map_or(p.name.clone(), |(_, n)| n.to_owned())
            })
            .collect();
        let mut name = base.clone();
        let mut n = 2;
        while taken.contains(&name) || name == "midi_in" || name == "midi_out" {
            name = format!("{base} {n}");
            n += 1;
        }
        let port = self.backend.add_midi_input(&name)?;
        Ok(MidiLink {
            source: source.to_owned(),
            port,
            connected: false,
        })
    }

    /// Connects every picked source that is there and not connected yet.
    /// Call now and then (a second is fine) so devices plugged back in are
    /// heard again. Returns whether a source came or went.
    pub fn refresh_midi(&mut self) -> bool {
        self.backend.collect_removed();
        let present: Vec<String> = self.midi_sources().into_iter().map(|s| s.name).collect();
        let mut changed = false;
        for link in &mut self.midi {
            let here = present.contains(&link.source);
            if !here {
                changed |= link.connected;
                link.connected = false;
                continue;
            }
            // Connect even when we think it is: the source may have gone
            // and come back between two polls (a USB reset), which drops
            // the link without us seeing it. "Already connected" is fine.
            let was = link.connected;
            match self.backend.connect(&link.source, &link.port.name) {
                Ok(()) => {}
                // Made already (by a session manager, or before a restart).
                Err(BackendError::Connection { reason, .. }) if reason.contains("already") => {}
                Err(e) => {
                    log::warn!(
                        "could not connect {} to {}: {e}",
                        link.source,
                        link.port.name
                    );
                    continue;
                }
            }
            link.connected = true;
            changed |= !was;
        }
        changed
    }

    /// The MIDI sources other clients offer now: hardware and programs.
    pub fn midi_sources(&self) -> Vec<MidiSource> {
        let filter = PortFilter {
            kind: Some(PortKind::Midi),
            direction: Some(PortDirection::Output),
            physical_only: false,
        };
        self.backend
            .list_ports(&filter)
            .unwrap_or_default()
            .into_iter()
            .filter(|p| !p.is_ours)
            .map(|p| MidiSource {
                name: p.name,
                is_physical: p.is_physical,
            })
            .collect()
    }

    /// The sources given ports by [`set_midi_inputs`](Self::set_midi_inputs),
    /// each with whether it is connected now.
    pub fn midi_inputs(&self) -> Vec<(String, bool)> {
        self.midi
            .iter()
            .map(|l| (l.source.clone(), l.connected))
            .collect()
    }

    fn publish_midi_sources(&mut self) {
        let map: HashMap<String, usize> = self
            .midi
            .iter()
            .map(|l| (l.source.clone(), engine_source(l.port.slot)))
            .collect();
        self.session.set_midi_sources(map);
    }

    /// Restarts audio with a new configuration. The session is recreated at
    /// the new rate, so sync it again afterwards; the play position is kept.
    pub fn restart(&mut self, config: HostConfig) {
        let position = self.session.position();
        let looped = self.session.loop_range();
        let playing = self.session.is_playing();
        let tempo = self.session.tempo().clone();
        let midi = std::mem::take(&mut self.midi_wanted);
        let physical = std::mem::take(&mut self.midi_physical);
        // Saved while the old engine still plays them; restored at the new rate.
        let plugins = self.session.take_plugins();
        let _ = self.backend.close();
        *self = Self::start(config);
        self.midi_physical = physical;
        self.session.set_plugins(plugins);
        self.set_midi_inputs(&midi);
        // Positions are in ticks: map them with the project's tempo, not the
        // fresh session's default, so the playhead stays on the same bar.
        // The next sync applies the tempo to the engine.
        self.session.tempo = tempo;
        let _ = self.session.set_loop(looped);
        let _ = self.session.locate(position);
        if playing {
            let _ = self.session.play();
        }
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    pub fn kind(&self) -> HostKind {
        self.kind
    }

    pub fn config(&self) -> &HostConfig {
        &self.config
    }

    /// Why the audio server could not be used, when running silent.
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn sample_rate(&self) -> u32 {
        self.backend.sample_rate().round() as u32
    }

    pub fn buffer_size(&self) -> u32 {
        self.backend.buffer_size()
    }

    /// The backend's input and output latency as it reports it now.
    pub fn latency(&self) -> Latency {
        self.backend.latency()
    }

    /// Cycles the backend missed (xruns) since it started.
    pub fn xruns(&self) -> u64 {
        self.backend.stats().xruns
    }

    pub fn backend(&self) -> &dyn AudioBackend {
        self.backend.as_ref()
    }

    pub fn position(&self) -> TickTime {
        self.session.position()
    }
}

/// Connects our audio ports to the system's physical ones, in order, like
/// the backends' own auto-connect.
fn connect_audio(backend: &mut dyn AudioBackend) {
    let list = |direction, ours: bool| -> Vec<PortInfo> {
        let filter = PortFilter {
            kind: Some(PortKind::Audio),
            direction: Some(direction),
            physical_only: !ours,
        };
        backend
            .list_ports(&filter)
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.is_ours == ours)
            .collect()
    };
    use PortDirection::{Input, Output};
    let pairs: Vec<(String, String)> = list(Output, true)
        .into_iter()
        .zip(list(Input, false))
        .chain(list(Output, false).into_iter().zip(list(Input, true)))
        .map(|(a, b)| (a.name, b.name))
        .collect();
    for (from, to) in pairs {
        // Already-connected pairs (WirePlumber may link us first) are fine.
        if let Err(e) = backend.connect(&from, &to) {
            log::debug!("auto-connect {from} -> {to}: {e}");
        }
    }
}

impl Drop for AudioHost {
    fn drop(&mut self) {
        // Stop the callback (and drop the processor) before the session.
        let _ = self.backend.close();
    }
}

/// The name for our port that listens to `source`. Hardware ports name
/// the device in the port part, and their client is a bridge shared by
/// every device (`system`, `Midi-Bridge`, `a2j`), so only the port part is
/// kept. A program's port part is often generic (`out`, `midi_out`), so its
/// client name is kept in front: `seq:out` becomes `seq out`.
fn midi_port_base(source: &str, physical: bool) -> String {
    let name = match source.split_once(':') {
        Some((_, port)) if physical => port.to_owned(),
        Some((client, port)) => format!("{client} {port}"),
        None => source.to_owned(),
    };
    daw_backend::port_short_name(&name)
}
