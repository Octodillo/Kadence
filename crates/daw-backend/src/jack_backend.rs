use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;

use daw_core::event::EventList;
use jack::{
    AudioIn, AudioOut, Client, ClientOptions, ClientStatus, Control, Frames, MidiIn, MidiOut,
    NotificationHandler, Port, PortFlags, ProcessHandler, ProcessScope, RawMidi,
};

use crate::cycle::{PortBuffers, push_or_count};
use crate::stats::{SharedStats, current_thread_is_realtime};
use crate::{
    AudioBackend, BackendConfig, BackendCycle, BackendError, BackendStats, PortDirection,
    PortFilter, PortInfo, PortKind, ProcessCallback, midi,
};

const AUDIO_TYPE: &str = jack::jack_sys::FLOAT_MONO_AUDIO;
const MIDI_TYPE: &str = jack::jack_sys::RAW_MIDI_TYPE;
/// How often to re-check scheduling until real-time is granted.
const RT_RECHECK_CYCLES: u32 = 64;

/// JACK backend. On PipeWire systems this goes through `pipewire-jack`, the
/// PipeWire implementation of `libjack`; on others, through a real JACK
/// server. It never starts a JACK server itself.
#[derive(Default)]
pub struct JackBackend {
    state: State,
    cfg: Option<BackendConfig>,
    shared: Arc<SharedStats>,
    /// Connections to make on the next `start`. Deactivating a JACK client
    /// drops all of its connections, and JACK refuses to connect an inactive
    /// client's ports, so while stopped they are kept here: `stop` saves the
    /// live ones and `connect`/`disconnect` edit the list.
    links: Vec<(String, String)>,
    /// Auto-connect has run since `open`. Later starts restore `links`
    /// instead, so ports the user unplugged stay unplugged.
    auto_connected: bool,
    /// Make the next `start` fail the way `jack_activate` failing does.
    #[cfg(test)]
    fail_next_activation: bool,
}

#[derive(Default)]
enum State {
    #[default]
    Closed,
    Open(Client, Ports),
    Running(jack::AsyncClient<Notifications, Processor>),
}

struct Ports {
    audio_in: Vec<Port<AudioIn>>,
    audio_out: Vec<Port<AudioOut>>,
    midi_in: Option<Port<MidiIn>>,
    midi_out: Option<Port<MidiOut>>,
}

impl JackBackend {
    pub fn new() -> Self {
        Self::default()
    }

    fn client(&self) -> Result<&Client, BackendError> {
        match &self.state {
            State::Closed => Err(BackendError::NotOpen),
            State::Open(c, _) => Ok(c),
            State::Running(a) => Ok(a.as_client()),
        }
    }

    /// Full names of our ports, each with whether it is an output.
    fn our_ports(client: &Client, cfg: &BackendConfig) -> Vec<(String, bool)> {
        let me = client.name();
        let mut v = Vec::new();
        v.extend((1..=cfg.audio_inputs).map(|i| (format!("{me}:audio_in_{i}"), false)));
        v.extend((1..=cfg.audio_outputs).map(|i| (format!("{me}:audio_out_{i}"), true)));
        if cfg.midi_input {
            v.push((format!("{me}:midi_in"), false));
        }
        if cfg.midi_output {
            v.push((format!("{me}:midi_out"), true));
        }
        v
    }

    /// Every live connection of our ports, as (output, input) pairs.
    fn live_links(client: &Client, cfg: &BackendConfig) -> Vec<(String, String)> {
        let mut links = Vec::new();
        for (name, is_output) in Self::our_ports(client, cfg) {
            let Some(port) = client.port_by_name(&name) else {
                continue;
            };
            for other in port.get_connections() {
                let link = if is_output {
                    (name.clone(), other)
                } else {
                    (other, name.clone())
                };
                // A link between two of our own ports shows up on both.
                if !links.contains(&link) {
                    links.push(link);
                }
            }
        }
        links
    }

    /// The full name of a port given by any of its names (an alias, say), so
    /// saved links compare equal to what `get_connections` reports.
    fn full_name(client: &Client, name: &str) -> String {
        client
            .port_by_name(name)
            .and_then(|p| p.name().ok())
            .unwrap_or_else(|| name.to_owned())
    }

    /// Checks what JACK would check on connect, for a client that cannot
    /// connect yet because it is inactive. Returns whether either port is
    /// ours: a link between two other clients' ports does not depend on us
    /// being active, so it goes to JACK at once.
    fn check_link(client: &Client, from: &str, to: &str) -> Result<bool, String> {
        let port = |name| {
            client
                .port_by_name(name)
                .ok_or_else(|| format!("no port named {name}"))
        };
        let (a, b) = (port(from)?, port(to)?);
        if !a.flags().contains(PortFlags::IS_OUTPUT) || !b.flags().contains(PortFlags::IS_INPUT) {
            return Err("must connect an output to an input".into());
        }
        if a.port_type().ok() != b.port_type().ok() {
            return Err("port types differ".into());
        }
        Ok(client.is_mine(&a) || client.is_mine(&b))
    }

    fn auto_connect(&self, client: &Client, cfg: &BackendConfig) {
        let me = client.name();
        let physical = |ty, dir| client.ports(None, Some(ty), PortFlags::IS_PHYSICAL | dir);
        let ours = |prefix: &str, n: u16| -> Vec<String> {
            (1..=n).map(|i| format!("{me}:{prefix}_{i}")).collect()
        };
        let mut pairs: Vec<(String, String)> = Vec::new();
        pairs.extend(
            ours("audio_out", cfg.audio_outputs)
                .into_iter()
                .zip(physical(AUDIO_TYPE, PortFlags::IS_INPUT)),
        );
        pairs.extend(
            physical(AUDIO_TYPE, PortFlags::IS_OUTPUT)
                .into_iter()
                .zip(ours("audio_in", cfg.audio_inputs)),
        );
        if cfg.midi_input {
            let midi_in = format!("{me}:midi_in");
            pairs.extend(
                physical(MIDI_TYPE, PortFlags::IS_OUTPUT)
                    .into_iter()
                    .map(|p| (p, midi_in.clone())),
            );
        }
        for (from, to) in pairs {
            // Already-connected pairs (WirePlumber may link us first) are fine.
            if let Err(e) = client.connect_ports_by_name(&from, &to) {
                log::debug!("auto-connect {from} -> {to}: {e}");
            }
        }
    }
}

fn connect_error(e: jack::Error) -> BackendError {
    BackendError::Connect(match e {
        jack::Error::ClientError(status) if status.contains(ClientStatus::SERVER_FAILED) => {
            "no JACK server is running. On PipeWire, install pipewire-jack (or run under pw-jack)"
                .into()
        }
        jack::Error::LibraryError(msg) => {
            format!("libjack not found ({msg}). Install pipewire-jack or JACK")
        }
        e => e.to_string(),
    })
}

impl AudioBackend for JackBackend {
    fn name(&self) -> &'static str {
        "JACK"
    }

    fn open(&mut self, cfg: BackendConfig) -> Result<(), BackendError> {
        if !matches!(self.state, State::Closed) {
            return Err(BackendError::AlreadyOpen);
        }
        let (client, _status) =
            Client::new(&cfg.client_name, ClientOptions::NO_START_SERVER).map_err(connect_error)?;

        if let Some(n) = cfg.buffer_size
            && client.buffer_size() != n
            && let Err(e) = client.set_buffer_size(n)
        {
            log::warn!("JACK refused buffer size {n}: {e}");
        }
        if let Some(rate) = cfg.sample_rate
            && client.sample_rate() != rate
        {
            log::warn!(
                "JACK runs at {} Hz, not the requested {rate} Hz; the server decides the rate",
                client.sample_rate()
            );
        }

        let port_err = |name: String| move |_| BackendError::Port(name);
        let mut ports = Ports {
            audio_in: Vec::new(),
            audio_out: Vec::new(),
            midi_in: None,
            midi_out: None,
        };
        for i in 1..=cfg.audio_inputs {
            let name = format!("audio_in_{i}");
            ports.audio_in.push(
                client
                    .register_port(&name, AudioIn::default())
                    .map_err(port_err(name))?,
            );
        }
        for i in 1..=cfg.audio_outputs {
            let name = format!("audio_out_{i}");
            ports.audio_out.push(
                client
                    .register_port(&name, AudioOut::default())
                    .map_err(port_err(name))?,
            );
        }
        if cfg.midi_input {
            ports.midi_in = Some(
                client
                    .register_port("midi_in", MidiIn::default())
                    .map_err(port_err("midi_in".into()))?,
            );
        }
        if cfg.midi_output {
            ports.midi_out = Some(
                client
                    .register_port("midi_out", MidiOut::default())
                    .map_err(port_err("midi_out".into()))?,
            );
        }

        self.shared = Arc::new(SharedStats::new());
        self.shared.sample_rate.store(client.sample_rate(), Relaxed);
        self.shared.buffer_size.store(client.buffer_size(), Relaxed);
        self.state = State::Open(client, ports);
        self.cfg = Some(cfg);
        self.links.clear();
        self.auto_connected = false;
        Ok(())
    }

    fn start(&mut self, cb: ProcessCallback) -> Result<(), BackendError> {
        let (client, ports) = match std::mem::take(&mut self.state) {
            State::Open(c, p) => (c, p),
            other @ State::Running(_) => {
                self.state = other;
                return Err(BackendError::AlreadyRunning);
            }
            State::Closed => return Err(BackendError::NotOpen),
        };
        let cfg = self.cfg.clone().ok_or(BackendError::NotOpen)?;
        let processor = Processor {
            bufs: PortBuffers::new(ports.audio_in.len(), ports.audio_out.len()),
            ports,
            midi_in: EventList::with_capacity(cfg.midi_capacity),
            midi_out: EventList::with_capacity(cfg.midi_capacity),
            cb,
            shared: self.shared.clone(),
            frame_time: 0,
            rt_check_in: 0,
            panicked: false,
        };
        self.shared.callback_panicked.store(false, Relaxed);
        let notifications = Notifications {
            shared: self.shared.clone(),
        };
        let raw = client.raw();
        #[cfg(test)]
        let result = if std::mem::take(&mut self.fail_next_activation) {
            // What the jack crate does when jack_activate fails.
            std::mem::forget((client, notifications, processor));
            Err(jack::Error::ClientActivationError)
        } else {
            client.activate_async(notifications, processor)
        };
        #[cfg(not(test))]
        let result = client.activate_async(notifications, processor);
        let active = match result {
            Ok(active) => active,
            Err(e) => {
                if matches!(e, jack::Error::ClientActivationError) {
                    // When jack_activate fails, the jack crate leaks the
                    // client rather than close it (its callbacks may already
                    // point at the context). Left open, it stays on the
                    // server under our name, and the next open is renamed
                    // ("daw-01"), so saved "daw:*" links no longer match.
                    // SAFETY: the crate forgot its `Client`, so nothing else
                    // uses or closes `raw`. Closing it ends all callbacks
                    // into the leaked context, which is never freed.
                    drop(unsafe { Client::from_raw(raw) });
                }
                // On any other error the crate dropped, and so closed, the
                // client. Either way the ports are gone.
                self.cfg = None;
                self.links.clear();
                return Err(BackendError::Other(format!(
                    "could not activate the JACK client ({e}); the backend is closed and must be opened again"
                )));
            }
        };
        if cfg.auto_connect && !self.auto_connected {
            self.auto_connect(active.as_client(), &cfg);
            self.auto_connected = true;
        }
        for (from, to) in std::mem::take(&mut self.links) {
            let client = active.as_client();
            // Auto-connect or the session manager may have made it already.
            let made = client
                .port_by_name(&from)
                .is_some_and(|p| p.is_connected_to(&to).unwrap_or(false));
            if made {
                continue;
            }
            if let Err(e) = client.connect_ports_by_name(&from, &to) {
                log::warn!("could not restore connection {from} -> {to}: {e}");
            }
        }
        self.state = State::Running(active);
        Ok(())
    }

    fn stop(&mut self) -> Result<(), BackendError> {
        match std::mem::take(&mut self.state) {
            State::Running(active) => {
                // After a server shutdown there is nothing left to ask.
                if let Some(cfg) = &self.cfg
                    && !self.shared.server_gone.load(Relaxed)
                {
                    self.links = Self::live_links(active.as_client(), cfg);
                }
                let (client, _, processor) = active
                    .deactivate()
                    .map_err(|e| BackendError::Other(format!("could not deactivate: {e}")))?;
                // The callback drops here, on the caller's thread.
                self.state = State::Open(client, processor.ports);
            }
            other => self.state = other,
        }
        Ok(())
    }

    fn close(&mut self) -> Result<(), BackendError> {
        self.stop()?;
        self.state = State::Closed;
        self.cfg = None;
        self.links.clear();
        Ok(())
    }

    fn is_running(&self) -> bool {
        matches!(self.state, State::Running(_))
    }

    fn sample_rate(&self) -> f64 {
        f64::from(self.shared.sample_rate.load(Relaxed))
    }

    fn buffer_size(&self) -> u32 {
        self.shared.buffer_size.load(Relaxed)
    }

    fn list_ports(&self, filter: &PortFilter) -> Result<Vec<PortInfo>, BackendError> {
        let client = self.client()?;
        let ty = filter.kind.map(|k| match k {
            PortKind::Audio => AUDIO_TYPE,
            PortKind::Midi => MIDI_TYPE,
        });
        let mut flags = PortFlags::empty();
        if filter.physical_only {
            flags |= PortFlags::IS_PHYSICAL;
        }
        match filter.direction {
            Some(PortDirection::Input) => flags |= PortFlags::IS_INPUT,
            Some(PortDirection::Output) => flags |= PortFlags::IS_OUTPUT,
            None => {}
        }
        Ok(client
            .ports(None, ty, flags)
            .into_iter()
            .filter_map(|name| {
                let port = client.port_by_name(&name)?;
                let kind = match port.port_type().ok()?.as_str() {
                    AUDIO_TYPE => PortKind::Audio,
                    MIDI_TYPE => PortKind::Midi,
                    _ => return None,
                };
                let flags = port.flags();
                Some(PortInfo {
                    kind,
                    direction: if flags.contains(PortFlags::IS_INPUT) {
                        PortDirection::Input
                    } else {
                        PortDirection::Output
                    },
                    is_physical: flags.contains(PortFlags::IS_PHYSICAL),
                    is_ours: client.is_mine(&port),
                    name,
                })
            })
            .collect())
    }

    fn connect(&mut self, from: &str, to: &str) -> Result<(), BackendError> {
        let err = |reason: String| BackendError::Connection {
            from: from.into(),
            to: to.into(),
            reason,
        };
        match &self.state {
            State::Closed => Err(BackendError::NotOpen),
            State::Open(client, _) => {
                if !Self::check_link(client, from, to).map_err(err)? {
                    return client
                        .connect_ports_by_name(from, to)
                        .map_err(|e| err(e.to_string()));
                }
                let link = (Self::full_name(client, from), Self::full_name(client, to));
                if self.links.contains(&link) {
                    return Err(err("already connected".into()));
                }
                self.links.push(link);
                Ok(())
            }
            State::Running(a) => a
                .as_client()
                .connect_ports_by_name(from, to)
                .map_err(|e| err(e.to_string())),
        }
    }

    fn disconnect(&mut self, from: &str, to: &str) -> Result<(), BackendError> {
        let err = |reason: String| BackendError::Connection {
            from: from.into(),
            to: to.into(),
            reason,
        };
        match &self.state {
            State::Closed => Err(BackendError::NotOpen),
            State::Open(client, _) => {
                let link = (Self::full_name(client, from), Self::full_name(client, to));
                let before = self.links.len();
                self.links.retain(|l| *l != link);
                if self.links.len() < before {
                    return Ok(());
                }
                // Not one of ours waiting for start; it may be a live link
                // between two other clients.
                client
                    .disconnect_ports_by_name(from, to)
                    .map_err(|e| err(e.to_string()))
            }
            State::Running(a) => a
                .as_client()
                .disconnect_ports_by_name(from, to)
                .map_err(|e| err(e.to_string())),
        }
    }

    fn stats(&self) -> BackendStats {
        let mut s = self.shared.snapshot();
        if let State::Running(a) = &self.state {
            s.dsp_load = Some(a.as_client().cpu_load());
        }
        s
    }
}

impl Drop for JackBackend {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

struct Notifications {
    shared: Arc<SharedStats>,
}

impl NotificationHandler for Notifications {
    unsafe fn shutdown(&mut self, _status: ClientStatus, _reason: &str) {
        self.shared.server_gone.store(true, Relaxed);
    }

    fn sample_rate(&mut self, _: &Client, srate: Frames) -> Control {
        self.shared.sample_rate.store(srate, Relaxed);
        Control::Continue
    }

    fn xrun(&mut self, _: &Client) -> Control {
        self.shared.xruns.fetch_add(1, Relaxed);
        Control::Continue
    }
}

struct Processor {
    ports: Ports,
    bufs: PortBuffers,
    midi_in: EventList,
    midi_out: EventList,
    cb: ProcessCallback,
    shared: Arc<SharedStats>,
    frame_time: u64,
    /// Cycles until the next real-time scheduling check; `u32::MAX` once
    /// the thread is known to be real-time.
    rt_check_in: u32,
    /// The callback panicked; it is not called again.
    panicked: bool,
}

impl ProcessHandler for Processor {
    fn process(&mut self, _: &Client, ps: &ProcessScope) -> Control {
        if !self.panicked {
            let shared = self.shared.clone();
            // Catch the panic here: the jack crate's own handler prints, leaks
            // the payload and quits, which leaves us looking alive but silent.
            let ok = crate::rt_guard(&shared, || {
                catch_unwind(AssertUnwindSafe(|| self.cycle(ps))).is_ok()
            });
            if ok {
                return Control::Continue;
            }
            self.panicked = true;
            self.shared.callback_panicked.store(true, Relaxed);
        }
        self.silence(ps);
        self.shared.cycles.fetch_add(1, Relaxed);
        self.shared
            .frames
            .fetch_add(u64::from(ps.n_frames()), Relaxed);
        Control::Continue
    }

    fn buffer_size(&mut self, _: &Client, size: Frames) -> Control {
        self.shared.buffer_size.store(size, Relaxed);
        Control::Continue
    }
}

impl Processor {
    /// Writes silence to every output, for cycles without a callback.
    fn silence(&mut self, ps: &ProcessScope) {
        for port in &mut self.ports.audio_out {
            port.as_mut_slice(ps).fill(0.0);
        }
        if let Some(port) = &mut self.ports.midi_out {
            // Creating the writer clears the buffer.
            let _ = port.writer(ps);
        }
    }

    fn cycle(&mut self, ps: &ProcessScope) {
        // PipeWire promotes its data thread to real-time asynchronously
        // (through rtkit or the xdg-desktop-portal), often after the first
        // cycles, so keep checking every so often until it reads true.
        if self.rt_check_in == 0 {
            let rt = current_thread_is_realtime();
            self.shared.set_realtime(rt);
            self.rt_check_in = if rt { u32::MAX } else { RT_RECHECK_CYCLES };
        } else if self.rt_check_in != u32::MAX {
            self.rt_check_in -= 1;
        }
        let frames = ps.n_frames();
        let mut dropped = 0;
        self.midi_in.clear();
        self.midi_out.clear();
        if let Some(port) = &self.ports.midi_in {
            for m in port.iter(ps) {
                if let Some(e) = midi::decode(m.time, m.bytes) {
                    push_or_count(&mut self.midi_in, e, &mut dropped);
                }
            }
        }

        let frame_time = self.frame_time;
        let sample_rate = f64::from(self.shared.sample_rate.load(Relaxed));
        let xruns = self.shared.xruns.load(Relaxed);
        let (cb, midi_in, midi_out) = (&mut self.cb, &self.midi_in, &mut self.midi_out);
        self.bufs.run(
            self.ports.audio_in.iter().map(|p| p.as_slice(ps)),
            self.ports.audio_out.iter_mut().map(|p| p.as_mut_slice(ps)),
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

        if let Some(port) = &mut self.ports.midi_out {
            let mut writer = port.writer(ps);
            for e in self.midi_out.iter() {
                let mut bytes = [0; 3];
                let Some(len) = midi::encode(&e.kind, &mut bytes) else {
                    dropped += u64::from(midi::is_malformed(&e.kind));
                    continue;
                };
                let time = e.frame.min(frames.saturating_sub(1));
                if writer
                    .write(&RawMidi {
                        time,
                        bytes: &bytes[..len],
                    })
                    .is_err()
                {
                    dropped += 1;
                }
            }
        }

        self.frame_time += u64::from(frames);
        self.shared.frames.fetch_add(u64::from(frames), Relaxed);
        self.shared.cycles.fetch_add(1, Relaxed);
        if dropped > 0 {
            self.shared.midi_dropped.fetch_add(dropped, Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs a running JACK server"]
    fn failed_activation_closes_the_client_and_frees_its_name() {
        let cfg = BackendConfig {
            client_name: "wu2-actfail".into(),
            auto_connect: false,
            ..BackendConfig::default()
        };
        let mut b = JackBackend::new();
        b.open(cfg.clone()).unwrap();
        b.fail_next_activation = true;
        let err = b.start(Box::new(|_| {})).unwrap_err();
        assert!(matches!(err, BackendError::Other(_)), "{err}");
        assert!(!b.is_running());
        assert!(matches!(
            b.start(Box::new(|_| {})),
            Err(BackendError::NotOpen)
        ));

        // The leaked client is gone, so reopening gets the same name back.
        b.open(cfg).unwrap();
        let ours: Vec<_> = b
            .list_ports(&PortFilter::default())
            .unwrap()
            .into_iter()
            .filter(|p| p.is_ours)
            .map(|p| p.name)
            .collect();
        assert!(!ours.is_empty());
        assert!(
            ours.iter().all(|n| n.starts_with("wu2-actfail:")),
            "renamed: {ours:?}"
        );
        b.start(Box::new(|_| {})).unwrap();
        b.close().unwrap();
    }
}
