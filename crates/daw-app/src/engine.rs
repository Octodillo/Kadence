//! What the UI needs from the audio engine, without naming the engine.
//!
//! [`EngineHandle`] is the only door from the front end into audio. It uses
//! plain Rust types so a second front end (a GNOME one is planned) can share
//! it unchanged; nothing here may mention Qt.
//!
//! [`SessionEngine`] is the real one: JACK (PipeWire on Plasma) through
//! `daw-session`'s [`AudioHost`], falling back to a silent backend when no
//! audio server answers. [`StubEngine`] pretends to have two devices, for
//! tests of the settings UI; it still plays silently so the transport works.

use std::fmt;
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use daw_backend::{DummyBackend, DummySystem, Pacing, PortDirection, PortKind};
use daw_core::plugin::PluginDescriptor;
use daw_plugin_host::{HostLauncher, SandboxConfig};
pub use daw_session::host::MidiSource;
use daw_session::host::{AudioHost, AudioServer, HostConfig, HostHealth, HostKind, JackServer};
use daw_session::{PluginLoader, SandboxLoader, Session};

/// An audio device the user can pick in the settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioDevice {
    /// Stable identifier, stored in the config file.
    pub id: String,
    /// Human-readable name for the device list. Empty for the backend's
    /// own output (the only device of JACK or the silent backend): the
    /// front end names that after [`BackendKind`], in the user's language.
    pub name: String,
}

/// Which audio system runs, for the front end to name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendKind {
    /// A JACK server, usually PipeWire's.
    Jack,
    /// No audio server answered; the project plays silently.
    Silent,
    /// [`StubEngine`], for tests.
    Stub,
}

/// The audio configuration the user asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioConfig {
    /// Device id, or `None` for the backend's default device.
    pub device: Option<String>,
    pub sample_rate: u32,
    pub buffer_size: u32,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            device: None,
            sample_rate: 48_000,
            buffer_size: 256,
        }
    }
}

/// What applying an audio configuration does, worked out by
/// [`EngineHandle::plan_audio_config`] and carried out as it says by
/// [`EngineHandle::apply_audio_plan`]: deciding once means whoever applies
/// it can prepare for a restart (end a take) knowing one will come.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioPlan {
    config: AudioConfig,
    restarts: bool,
}

impl AudioPlan {
    /// Applying `config` restarts audio, or not. For engines to make.
    pub fn new(config: AudioConfig, restarts: bool) -> Self {
        Self { config, restarts }
    }

    /// The configuration asked for.
    pub fn config(&self) -> &AudioConfig {
        &self.config
    }

    /// Whether carrying it out restarts audio, which ends a take and
    /// replaces the session.
    pub fn restarts(&self) -> bool {
        self.restarts
    }
}

/// The buffer sizes, in frames, every engine offers.
const BUFFER_SIZES: [u32; 6] = [64, 128, 256, 512, 1024, 2048];
/// The sample rates, in Hz, an engine without an audio server offers.
const SILENT_RATES: [u32; 4] = [44_100, 48_000, 88_200, 96_000];

/// Whether audio is flowing, and if not, why.
#[derive(Clone, Debug, PartialEq)]
pub enum EngineStatus {
    /// Not started yet, or stopped on purpose.
    Stopped,
    /// Audio is running with this configuration.
    Running { sample_rate: u32, buffer_size: u32 },
    /// Audio stopped or never started, and this is why.
    Failed(EngineError),
}

/// Why an [`EngineHandle`] call was refused, or why audio stopped.
///
/// The variants carry data, not sentences: each front end words them in
/// the user's language. [`Display`](fmt::Display) is English for logs only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineError {
    /// No device with this id (most likely it was unplugged).
    UnknownDevice(String),
    /// The device cannot run at this sample rate, in Hz.
    UnsupportedSampleRate(u32),
    /// The device cannot run with this buffer size, in frames.
    UnsupportedBufferSize(u32),
    /// The audio server went away while audio was running (it exited, or
    /// PipeWire restarted) and has not come back yet.
    ServerLost,
    /// The engine failed inside the audio callback and stopped processing.
    /// Applying the audio settings starts it again.
    Crashed,
    /// Audio was restarted again and again and still fails, so the engine
    /// stopped trying. Applying the audio settings tries again.
    Unstable,
    /// The audio system failed for a reason of its own. The text comes from
    /// the backend or a library below it, so it is not translated; show it
    /// as a detail after a translated sentence.
    Backend(String),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownDevice(id) => write!(f, "unknown audio device “{id}”"),
            Self::UnsupportedSampleRate(rate) => write!(f, "unsupported sample rate {rate} Hz"),
            Self::UnsupportedBufferSize(size) => write!(f, "unsupported buffer size {size}"),
            Self::ServerLost => write!(f, "the audio server went away"),
            Self::Crashed => write!(f, "the engine stopped after an internal error"),
            Self::Unstable => write!(f, "audio keeps failing"),
            Self::Backend(message) => write!(f, "audio system error: {message}"),
        }
    }
}

impl std::error::Error for EngineError {}

/// The engine as the UI sees it. Called on the UI thread only: the session
/// keeps its plugins on the thread that created them, so it is not `Send`.
pub trait EngineHandle {
    fn backend_kind(&self) -> BackendKind;
    fn audio_devices(&self) -> Vec<AudioDevice>;
    fn supported_sample_rates(&self) -> Vec<u32>;
    fn supported_buffer_sizes(&self) -> Vec<u32>;
    /// Says what applying `config` does, or why it cannot be applied.
    fn plan_audio_config(&self, config: &AudioConfig) -> Result<AudioPlan, EngineError>;
    /// Carries out `plan`, restarting audio if and only if it says so.
    /// [`audio_config`](Self::audio_config) then says what runs, which is
    /// not always what was asked for: the audio system can come or go
    /// during a restart, or run at a rate of its own.
    fn apply_audio_plan(&mut self, plan: AudioPlan);
    /// Plans `config` and carries the plan out. On error nothing changes:
    /// audio keeps running (or stays failed).
    fn apply_audio_config(&mut self, config: &AudioConfig) -> Result<(), EngineError> {
        let plan = self.plan_audio_config(config)?;
        self.apply_audio_plan(plan);
        Ok(())
    }
    /// The configuration in effect.
    fn audio_config(&self) -> AudioConfig;
    /// Whether audio is flowing now. It can fail at any time (the audio
    /// server may go away), so ask again every second or so.
    fn status(&self) -> EngineStatus;

    /// Whether [`restart_audio`](Self::restart_audio) would mend or improve
    /// things now: the audio server went away, came back, or changed its
    /// sample rate. Ask together with [`status`](Self::status).
    fn wants_restart(&self) -> bool {
        false
    }

    /// Restarts audio on whatever the audio system runs at now, keeping the
    /// rest of the configuration. Like
    /// [`apply_audio_config`](Self::apply_audio_config) it may replace the
    /// session.
    fn restart_audio(&mut self) {}

    fn play(&mut self);
    fn stop(&mut self);
    fn is_playing(&self) -> bool;

    /// The audio system's round trip, input plus output latency, in
    /// samples. Recording moves takes back by this much (plus the graph's
    /// own latency) so they line up with what the player heard.
    fn round_trip_latency(&self) -> u32 {
        0
    }

    /// Cycles the audio system missed (xruns) so far.
    fn xruns(&self) -> u64 {
        0
    }

    /// How an export loads plugins, as the session does, out of `known`
    /// (the plugin catalog's list, which is also what the session was
    /// told): `None` when the session has no plugin loader either (the stub
    /// engine).
    fn export_plugin_loader(
        &self,
        _known: Vec<PluginDescriptor>,
    ) -> Option<daw_session::export::LoaderFactory> {
        None
    }

    /// The session playing the project. After
    /// [`apply_audio_config`](Self::apply_audio_config) it may be a new one
    /// that has to be synced with the document again.
    fn session(&self) -> Option<&Session>;
    fn session_mut(&mut self) -> Option<&mut Session>;

    /// MIDI sources other programs and devices offer now.
    fn midi_sources(&self) -> Vec<MidiSource> {
        Vec::new()
    }

    /// Gives each source in `sources` a MIDI port of its own (see
    /// `AudioHost::set_midi_inputs`). Returns the ones that failed.
    fn set_midi_inputs(&mut self, sources: &[String]) -> Vec<String> {
        sources.to_vec()
    }

    /// The sources with ports, each with whether it is connected now.
    fn midi_inputs(&self) -> Vec<(String, bool)> {
        Vec::new()
    }

    /// Connects sources plugged back in. Returns whether any came or went.
    fn refresh_midi(&mut self) -> bool {
        false
    }
}

/// The MIDI calls of [`EngineHandle`], for engines built on an [`AudioHost`].
macro_rules! host_midi {
    () => {
        fn midi_sources(&self) -> Vec<MidiSource> {
            self.host.midi_sources()
        }

        fn set_midi_inputs(&mut self, sources: &[String]) -> Vec<String> {
            self.host
                .set_midi_inputs(sources)
                .into_iter()
                .map(|(source, _)| source)
                .collect()
        }

        fn midi_inputs(&self) -> Vec<(String, bool)> {
            self.host.midi_inputs()
        }

        fn refresh_midi(&mut self) -> bool {
            self.host.refresh_midi()
        }
    };
}

/// Loads plugins in sandbox host processes: this executable again, which
/// answers `--plugin-host` before Qt starts (see
/// [`run_plugin_child`](crate::run_plugin_child)). The loader learns which
/// plugins are installed from the plugin catalog, through
/// [`Session::set_known_plugins`].
fn plugin_loader() -> Option<Box<dyn PluginLoader>> {
    Some(Box::new(sandbox_loader(|c| c)?))
}

/// [`plugin_loader`] for an export, which renders faster than real time and
/// starts out knowing the catalog's plugins.
fn export_plugin_loader(known: Vec<PluginDescriptor>) -> Option<Box<dyn PluginLoader>> {
    let loader = sandbox_loader(daw_session::export::offline_sandbox)?;
    Some(Box::new(loader.with_known(known)))
}

fn sandbox_loader(adjust: impl FnOnce(SandboxConfig) -> SandboxConfig) -> Option<SandboxLoader> {
    let launcher = HostLauncher::current_exe().ok()?;
    Some(SandboxLoader::new(adjust(SandboxConfig::new(launcher))))
}

/// The client name of a quiet run, set by [`use_quiet_audio`].
static QUIET: OnceLock<String> = OnceLock::new();

/// Makes this a quiet run: the engine still uses the audio server, under a
/// name of its own (`kadence-<purpose>-<pid>`), but connects nothing to the
/// speakers or the microphone. For `--smoke-test`, `--screenshot-dir` and
/// `--benchmark`; call it before the engine starts. Only the first call
/// counts.
pub fn use_quiet_audio(purpose: &str) {
    let _ = QUIET.set(format!("kadence-{purpose}-{}", std::process::id()));
}

/// The configuration the engine starts with: `saved`'s rate and buffer
/// size where the engine offers them (the settings file can hold any
/// number), and see [`use_quiet_audio`], and [`default_engine`] for
/// `KADENCE_AUDIO=server`.
fn startup_config(saved: Option<&AudioConfig>) -> HostConfig {
    let config = QUIET
        .get()
        .map_or_else(HostConfig::default, |name| HostConfig::quiet(name.as_str()));
    let offered = |value: u32, list: &[u32]| list.contains(&value).then_some(value);
    HostConfig {
        sample_rate: saved
            .and_then(|s| offered(s.sample_rate, &SILENT_RATES))
            .unwrap_or(config.sample_rate),
        buffer_size: saved
            .and_then(|s| offered(s.buffer_size, &BUFFER_SIZES))
            .unwrap_or(config.buffer_size),
        require_socket: audio_override().as_deref() != Some("server"),
        ..config
    }
}

/// `KADENCE_AUDIO`: `stub` for [`StubEngine`]; `server` to open the audio
/// server even when its socket is not where Kadence looks (see
/// `HostConfig::require_socket`).
fn audio_override() -> Option<String> {
    std::env::var("KADENCE_AUDIO").ok()
}

/// Starts audio on `server` with plugins loading through [`plugin_loader`].
fn start_host(server: Rc<dyn AudioServer>, config: HostConfig) -> AudioHost {
    let mut host = AudioHost::start_on(server, config);
    host.session_mut().set_plugin_loader(plugin_loader());
    host
}

/// The engine the app starts with, at the rate and buffer size of `saved`
/// (the settings) when there is one, so that applying them after is no
/// second restart: the real one, or the stub when the environment says
/// `KADENCE_AUDIO=stub` (the UI tests do). `KADENCE_AUDIO=server` keeps
/// the real one but tries the audio server even when no socket of it was
/// found.
pub fn default_engine(saved: Option<&AudioConfig>) -> Box<dyn EngineHandle> {
    if audio_override().as_deref() == Some("stub") {
        Box::new(StubEngine::default())
    } else {
        Box::new(SessionEngine::new(saved))
    }
}

/// The real engine. See the module docs.
pub struct SessionEngine {
    host: AudioHost,
    config: AudioConfig,
    recovery: Recovery,
}

/// What the restarts so far say about the next one: the part of
/// [`SessionEngine`] that keeps it from restarting audio every second for
/// good, and from promising a reconnect it will not try.
#[derive(Debug)]
struct Recovery {
    /// The audio server went away under us and is not back yet.
    lost: bool,
    /// Go back to the audio server as soon as one answers. Off after a try
    /// that a server answered and that still ended silent: such a server
    /// is not asked again every second.
    reconnect: bool,
    /// Automatic restarts since `since`.
    recent: u32,
    since: Instant,
    /// Too many automatic restarts in a short time: no more of them until
    /// the user applies the audio settings.
    gave_up: bool,
}

impl Recovery {
    /// More automatic restarts than this within [`Self::WINDOW`] and the
    /// engine stops trying. A server that goes and comes back once costs two.
    const MAX_RESTARTS: u32 = 6;
    const WINDOW: Duration = Duration::from_secs(60);

    fn new(now: Instant) -> Self {
        Self {
            lost: false,
            reconnect: true,
            recent: 0,
            since: now,
            gave_up: false,
        }
    }

    /// The user (re)started audio, which ended on a server or not: every
    /// automatic recovery is worth trying again.
    fn restarted_by_user(&mut self, on_server: bool, now: Instant) {
        *self = Self {
            lost: self.lost && !on_server,
            ..Self::new(now)
        };
    }

    /// Audio restarted by itself, from running on a server or not to
    /// running on one or not.
    fn restarted(&mut self, was_on_server: bool, on_server: bool, now: Instant) {
        if now.duration_since(self.since) > Self::WINDOW {
            self.since = now;
            self.recent = 0;
        }
        self.recent += 1;
        self.gave_up = self.recent > Self::MAX_RESTARTS;
        // Silent after a server: it went away, and coming back is worth a
        // try. Silent as before: a server answered and could not be used,
        // which the backend's error explains better than "went away".
        self.lost = was_on_server && !on_server;
        self.reconnect = was_on_server || on_server;
    }
}

impl SessionEngine {
    const JACK: &'static str = "jack";
    const SILENT: &'static str = "silent";

    /// The engine on the JACK server; see [`default_engine`] for `saved`.
    pub fn new(saved: Option<&AudioConfig>) -> Self {
        Self::start_on(Rc::new(JackServer), startup_config(saved))
    }

    /// The engine on `server` (a pretend one in tests), started with
    /// `config`.
    fn start_on(server: Rc<dyn AudioServer>, config: HostConfig) -> Self {
        let buffer_size = config.buffer_size;
        let host = start_host(server, config);
        let config = AudioConfig {
            device: None,
            sample_rate: host.sample_rate(),
            // A request, as after applying settings: a server that runs
            // another size does not make the saved one differ.
            buffer_size,
        };
        Self {
            host,
            config,
            recovery: Recovery::new(Instant::now()),
        }
    }

    /// Restarts the host with `wanted` and takes the sample rate from what
    /// then runs: an audio server keeps its own whatever was asked for.
    fn restart(&mut self, wanted: HostConfig) {
        self.host.restart(wanted);
        self.config.sample_rate = self.host.sample_rate();
    }

    fn on_server(&self) -> bool {
        self.host.kind() == HostKind::Jack
    }

    /// Whether audio is not what it should be: failed, or silent for want
    /// of a server.
    fn is_failed(&self) -> bool {
        matches!(self.status(), EngineStatus::Failed(_))
    }

    /// Refuses what this engine cannot run at all.
    fn check(&self, config: &AudioConfig) -> Result<(), EngineError> {
        if let Some(id) = &config.device
            && *id != self.device().id
        {
            return Err(EngineError::UnknownDevice(id.clone()));
        }
        if !self.supported_sample_rates().contains(&config.sample_rate) {
            return Err(EngineError::UnsupportedSampleRate(config.sample_rate));
        }
        if !self.supported_buffer_sizes().contains(&config.buffer_size) {
            return Err(EngineError::UnsupportedBufferSize(config.buffer_size));
        }
        Ok(())
    }

    /// Whether applying `config`, which [`check`](Self::check) allows,
    /// restarts audio. Compared with what runs, not with what was last
    /// asked for: a server may have settled on another rate since. Also
    /// when nothing changed but audio is down and a restart could mend
    /// it: applying the settings again is how the user asks for another
    /// try. Silent for want of a server that is still not there, it could
    /// not.
    fn restarts_for(&self, config: &AudioConfig) -> bool {
        let could_mend = self.host.kind() == HostKind::Jack || self.host.server_appeared();
        config.sample_rate != self.config.sample_rate
            || config.buffer_size != self.config.buffer_size
            || !self.host.config().use_server
            || (self.is_failed() && could_mend)
    }

    fn device(&self) -> AudioDevice {
        match self.host.kind() {
            HostKind::Jack => AudioDevice {
                id: Self::JACK.into(),
                name: String::new(),
            },
            HostKind::Silent => AudioDevice {
                id: Self::SILENT.into(),
                name: String::new(),
            },
        }
    }
}

impl EngineHandle for SessionEngine {
    fn export_plugin_loader(
        &self,
        known: Vec<PluginDescriptor>,
    ) -> Option<daw_session::export::LoaderFactory> {
        Some(Box::new(move || export_plugin_loader(known)))
    }

    fn backend_kind(&self) -> BackendKind {
        match self.host.kind() {
            HostKind::Jack => BackendKind::Jack,
            HostKind::Silent => BackendKind::Silent,
        }
    }

    fn audio_devices(&self) -> Vec<AudioDevice> {
        vec![self.device()]
    }

    fn supported_sample_rates(&self) -> Vec<u32> {
        match self.host.kind() {
            // The server decides; offer only what it runs at.
            HostKind::Jack => vec![self.host.sample_rate()],
            HostKind::Silent => SILENT_RATES.to_vec(),
        }
    }

    fn supported_buffer_sizes(&self) -> Vec<u32> {
        BUFFER_SIZES.to_vec()
    }

    fn plan_audio_config(&self, config: &AudioConfig) -> Result<AudioPlan, EngineError> {
        self.check(config)?;
        Ok(AudioPlan::new(config.clone(), self.restarts_for(config)))
    }

    fn apply_audio_plan(&mut self, plan: AudioPlan) {
        let AudioPlan { config, restarts } = plan;
        if !restarts && self.is_failed() {
            // Still a try by the user, though nothing restarts now: the
            // engine may reconnect by itself again when the server comes.
            self.recovery
                .restarted_by_user(self.on_server(), Instant::now());
        }
        if restarts {
            let wanted = HostConfig {
                sample_rate: config.sample_rate,
                buffer_size: config.buffer_size,
                use_server: true,
                ..self.host.config().clone()
            };
            let was_on_server = self.on_server();
            self.restart(wanted);
            self.recovery
                .restarted_by_user(self.on_server(), Instant::now());
            // The buffer size stays a request: a server applies it when it
            // can, and `status` reports the size in use.
            self.config.buffer_size = config.buffer_size;
            if self.on_server() != was_on_server {
                // A server came or went during the restart, so what was
                // asked for was chosen from the wrong lists. Audio runs,
                // and `audio_config` says with what.
                self.config.device = None;
                return;
            }
            // A server that switched its rate since the list was made runs
            // at its new one. That too is not refused: audio restarted
            // already, and `audio_config` says at what rate.
        }
        self.config.device = config.device;
    }

    fn wants_restart(&self) -> bool {
        if self.recovery.gave_up {
            return false;
        }
        match self.host.health() {
            HostHealth::ServerGone | HostHealth::RateChanged(_) => true,
            // Restarting on its own could crash again, every second.
            HostHealth::Crashed => false,
            HostHealth::Ok => self.recovery.reconnect && self.host.server_appeared(),
        }
    }

    fn restart_audio(&mut self) {
        let was_on_server = self.on_server();
        self.restart(self.host.config().clone());
        self.recovery
            .restarted(was_on_server, self.on_server(), Instant::now());
    }

    fn audio_config(&self) -> AudioConfig {
        self.config.clone()
    }

    fn status(&self) -> EngineStatus {
        let failed = match (self.host.health(), self.host.kind(), self.host.last_error()) {
            (HostHealth::Crashed, ..) => EngineError::Crashed,
            (HostHealth::ServerGone, ..) if self.recovery.gave_up => EngineError::Unstable,
            (_, HostKind::Silent, Some(_)) if self.recovery.gave_up => EngineError::Unstable,
            (HostHealth::ServerGone, ..) => EngineError::ServerLost,
            (_, HostKind::Silent, Some(_)) if self.recovery.lost => EngineError::ServerLost,
            (_, HostKind::Silent, Some(e)) => EngineError::Backend(e.to_owned()),
            _ => {
                return EngineStatus::Running {
                    sample_rate: self.host.sample_rate(),
                    buffer_size: self.host.buffer_size(),
                };
            }
        };
        EngineStatus::Failed(failed)
    }

    fn play(&mut self) {
        let _ = self.host.session_mut().play();
    }

    fn stop(&mut self) {
        let _ = self.host.session_mut().stop();
    }

    fn is_playing(&self) -> bool {
        self.host.session().is_playing()
    }

    fn round_trip_latency(&self) -> u32 {
        self.host.latency().total()
    }

    fn xruns(&self) -> u64 {
        self.host.xruns()
    }

    fn session(&self) -> Option<&Session> {
        Some(self.host.session())
    }

    fn session_mut(&mut self) -> Option<&mut Session> {
        Some(self.host.session_mut())
    }

    host_midi!();
}

/// A pretend engine with two devices, for testing the settings UI without
/// an audio server. It plays through a silent backend so the transport and
/// arrange view behave as with real audio.
pub struct StubEngine {
    config: AudioConfig,
    running: bool,
    host: AudioHost,
    /// The pretend audio server `host` plays on.
    system: DummySystem,
    /// The plugins each export was to load from, for tests of what the app
    /// hands over.
    #[cfg(test)]
    pub(crate) exports_knew: std::rc::Rc<std::cell::RefCell<Vec<Vec<PluginDescriptor>>>>,
}

impl Default for StubEngine {
    fn default() -> Self {
        let (host, system) = Self::start_host();
        Self {
            config: AudioConfig::default(),
            running: false,
            host,
            system,
            #[cfg(test)]
            exports_knew: Default::default(),
        }
    }
}

impl fmt::Debug for StubEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StubEngine")
            .field("config", &self.config)
            .field("running", &self.running)
            .finish_non_exhaustive()
    }
}

impl StubEngine {
    /// A host on a pretend audio server of its own, and that server.
    fn start_host() -> (AudioHost, DummySystem) {
        let dummy = DummyBackend::new(Pacing::RealTime);
        let system = dummy.system();
        for (name, physical) in Self::MIDI_SOURCES {
            system.add_port(name, PortKind::Midi, PortDirection::Output, physical);
        }
        let config = HostConfig {
            use_server: false,
            ..Default::default()
        };
        let mut host = AudioHost::with_backend(Box::new(dummy), &config)
            .expect("the dummy backend always opens");
        host.session_mut().set_plugin_loader(plugin_loader());
        (host, system)
    }

    /// The pretend audio server, to make it fail under the engine.
    pub fn system(&self) -> &DummySystem {
        &self.system
    }

    const DEVICES: [(&'static str, &'static str); 2] = [
        ("stub:default", "Default output (stub)"),
        ("stub:usb", "USB audio interface (stub)"),
    ];

    /// Pretend MIDI sources besides the dummy system's own: two devices
    /// and a program.
    const MIDI_SOURCES: [(&'static str, bool); 3] = [
        ("Midi-Bridge:USB Keyboard (capture)", true),
        ("Midi-Bridge:Drum Pads (capture)", true),
        ("Sequencer:out", false),
    ];
}

impl EngineHandle for StubEngine {
    /// Exports load no plugins, as the trait has it.
    #[cfg(test)]
    fn export_plugin_loader(
        &self,
        known: Vec<PluginDescriptor>,
    ) -> Option<daw_session::export::LoaderFactory> {
        self.exports_knew.borrow_mut().push(known);
        None
    }

    fn backend_kind(&self) -> BackendKind {
        BackendKind::Stub
    }

    fn audio_devices(&self) -> Vec<AudioDevice> {
        Self::DEVICES
            .iter()
            .map(|(id, name)| AudioDevice {
                id: (*id).to_owned(),
                name: (*name).to_owned(),
            })
            .collect()
    }

    fn supported_sample_rates(&self) -> Vec<u32> {
        SILENT_RATES.to_vec()
    }

    fn supported_buffer_sizes(&self) -> Vec<u32> {
        BUFFER_SIZES.to_vec()
    }

    /// The stub only pretends: nothing restarts.
    fn plan_audio_config(&self, config: &AudioConfig) -> Result<AudioPlan, EngineError> {
        if let Some(id) = &config.device
            && !Self::DEVICES.iter().any(|(known, _)| known == id)
        {
            return Err(EngineError::UnknownDevice(id.clone()));
        }
        if !self.supported_sample_rates().contains(&config.sample_rate) {
            return Err(EngineError::UnsupportedSampleRate(config.sample_rate));
        }
        if !self.supported_buffer_sizes().contains(&config.buffer_size) {
            return Err(EngineError::UnsupportedBufferSize(config.buffer_size));
        }
        Ok(AudioPlan::new(config.clone(), false))
    }

    fn apply_audio_plan(&mut self, plan: AudioPlan) {
        self.config = plan.config;
        self.running = true;
    }

    fn audio_config(&self) -> AudioConfig {
        self.config.clone()
    }

    fn status(&self) -> EngineStatus {
        match self.host.health() {
            HostHealth::ServerGone => EngineStatus::Failed(EngineError::ServerLost),
            HostHealth::Crashed => EngineStatus::Failed(EngineError::Crashed),
            _ if self.running => EngineStatus::Running {
                sample_rate: self.config.sample_rate,
                buffer_size: self.config.buffer_size,
            },
            _ => EngineStatus::Stopped,
        }
    }

    fn wants_restart(&self) -> bool {
        self.host.health() == HostHealth::ServerGone
    }

    /// The pretend server is back at once, with a fresh session.
    fn restart_audio(&mut self) {
        (self.host, self.system) = Self::start_host();
    }

    fn play(&mut self) {
        let _ = self.host.session_mut().play();
    }

    fn stop(&mut self) {
        let _ = self.host.session_mut().stop();
    }

    fn is_playing(&self) -> bool {
        self.host.session().is_playing()
    }

    fn round_trip_latency(&self) -> u32 {
        self.host.latency().total()
    }

    fn xruns(&self) -> u64 {
        self.host.xruns()
    }

    fn session(&self) -> Option<&Session> {
        Some(self.host.session())
    }

    fn session_mut(&mut self) -> Option<&mut Session> {
        Some(self.host.session_mut())
    }

    host_midi!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_that_goes_and_comes_back_is_taken_up_again() {
        let t = Instant::now();
        let mut r = Recovery::new(t);
        r.restarted(true, false, t);
        assert!(r.lost && r.reconnect && !r.gave_up);
        r.restarted(false, true, t);
        assert!(!r.lost && r.reconnect && !r.gave_up);
    }

    #[test]
    fn a_server_that_answers_but_cannot_be_used_is_not_asked_again() {
        let t = Instant::now();
        let mut r = Recovery::new(t);
        r.restarted(true, false, t);
        r.restarted(false, false, t);
        // Not "went away" either: the backend's error says what is wrong.
        assert!(!r.lost && !r.reconnect);
        // The user trying again makes it worth asking once more.
        r.restarted_by_user(false, t);
        assert!(!r.lost && r.reconnect);
    }

    #[test]
    fn a_rate_change_that_ends_without_a_server_waits_for_it() {
        let t = Instant::now();
        let mut r = Recovery::new(t);
        r.restarted(true, false, t);
        assert!(r.lost && r.reconnect);
        // Applying settings while it is away keeps saying so.
        r.restarted_by_user(false, t);
        assert!(r.lost && r.reconnect);
        r.restarted_by_user(true, t);
        assert!(!r.lost);
    }

    #[test]
    fn restarts_in_quick_succession_stop_until_the_user_tries() {
        let t = Instant::now();
        let mut r = Recovery::new(t);
        for i in 0..Recovery::MAX_RESTARTS {
            r.restarted(
                i % 2 == 0,
                i % 2 != 0,
                t + Duration::from_secs(u64::from(i)),
            );
            assert!(!r.gave_up, "after {} restarts", i + 1);
        }
        r.restarted(true, false, t + Duration::from_secs(30));
        assert!(r.gave_up);
        r.restarted_by_user(true, t + Duration::from_secs(31));
        assert!(!r.gave_up);

        // Spread out, the same number is no trouble.
        let mut r = Recovery::new(t);
        for i in 0..20 {
            r.restarted(
                true,
                true,
                t + Recovery::WINDOW * i + Duration::from_secs(1),
            );
        }
        assert!(!r.gave_up);
    }

    /// A [`SessionEngine`] on a pretend server, and that server.
    fn on_pretend_server(saved: Option<&AudioConfig>) -> (SessionEngine, DummySystem) {
        let system = DummyBackend::new(Pacing::RealTime).system();
        let server = Rc::new(daw_session::host::PretendServer(system.clone()));
        let mut config = HostConfig::quiet(format!("kadence-engine-test-{}", std::process::id()));
        if let Some(saved) = saved {
            config.sample_rate = saved.sample_rate;
            config.buffer_size = saved.buffer_size;
        }
        (SessionEngine::start_on(server, config), system)
    }

    /// Polls until `engine` reports `status`, as the app's timer would,
    /// without restarting anything.
    fn wait_for_status(engine: &SessionEngine, status: &EngineStatus) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while engine.status() != *status {
            assert!(
                Instant::now() < deadline,
                "status stayed {:?}",
                engine.status()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn running(sample_rate: u32, buffer_size: u32) -> EngineStatus {
        EngineStatus::Running {
            sample_rate,
            buffer_size,
        }
    }

    #[test]
    fn engine_starts_at_the_saved_rate_and_buffer() {
        let saved = AudioConfig {
            device: None,
            sample_rate: 44_100,
            buffer_size: 512,
        };
        let (engine, _system) = on_pretend_server(Some(&saved));
        assert_eq!(engine.backend_kind(), BackendKind::Jack);
        assert_eq!(engine.status(), running(44_100, 512));
        // So applying the saved settings after does not restart.
        assert!(!engine.plan_audio_config(&saved).unwrap().restarts());
    }

    #[test]
    fn saved_settings_no_engine_offers_start_at_the_defaults() {
        let defaults = startup_config(None);
        let started = |sample_rate, buffer_size| {
            let config = startup_config(Some(&AudioConfig {
                device: None,
                sample_rate,
                buffer_size,
            }));
            (config.sample_rate, config.buffer_size)
        };
        let fresh = (defaults.sample_rate, defaults.buffer_size);
        assert_eq!(started(0, 0), fresh);
        assert_eq!(started(u32::MAX, u32::MAX), fresh);
        assert_eq!(started(12_345, 100), fresh);
        // What is offered is kept, each on its own.
        assert_eq!(started(96_000, 0), (96_000, defaults.buffer_size));
        assert_eq!(started(0, 1024), (defaults.sample_rate, 1024));
    }

    #[test]
    fn engine_reports_a_lost_server_and_takes_it_up_again() {
        let (mut engine, system) = on_pretend_server(None);
        assert_eq!(engine.status(), running(48_000, 256));
        assert!(!engine.wants_restart());

        system.shut_down();
        wait_for_status(&engine, &EngineStatus::Failed(EngineError::ServerLost));
        assert!(engine.wants_restart());
        engine.restart_audio();
        assert_eq!(engine.backend_kind(), BackendKind::Silent);
        assert_eq!(
            engine.status(),
            EngineStatus::Failed(EngineError::ServerLost),
            "silent for want of the server, which went away"
        );
        assert!(!engine.wants_restart(), "nothing to go back to yet");

        system.restore();
        assert!(engine.wants_restart());
        engine.restart_audio();
        assert_eq!(engine.backend_kind(), BackendKind::Jack);
        assert_eq!(engine.status(), running(48_000, 256));
        assert!(!engine.wants_restart());
    }

    #[test]
    fn engine_gives_up_on_a_server_that_keeps_failing() {
        let (mut engine, system) = on_pretend_server(None);
        system.shut_down();
        wait_for_status(&engine, &EngineStatus::Failed(EngineError::ServerLost));
        for _ in 0..Recovery::MAX_RESTARTS {
            engine.restart_audio();
            assert_ne!(engine.status(), EngineStatus::Failed(EngineError::Unstable));
        }
        engine.restart_audio();
        assert_eq!(engine.status(), EngineStatus::Failed(EngineError::Unstable));
        system.restore();
        assert!(
            !engine.wants_restart(),
            "gave up even on a server that is back"
        );

        // Applying the settings is the user asking to try again.
        let config = engine.audio_config();
        assert!(engine.plan_audio_config(&config).unwrap().restarts());
        engine.apply_audio_config(&config).unwrap();
        assert_eq!(engine.status(), running(48_000, 256));
    }

    #[test]
    fn engine_follows_a_server_rate_change() {
        let (mut engine, system) = on_pretend_server(None);
        system.run_at(44_100);
        system.change_sample_rate(44_100);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !engine.wants_restart() {
            assert!(Instant::now() < deadline, "the change never showed");
            std::thread::sleep(Duration::from_millis(5));
        }
        engine.restart_audio();
        assert_eq!(engine.status(), running(44_100, 256));
        assert_eq!(engine.audio_config().sample_rate, 44_100);
        assert_eq!(engine.supported_sample_rates(), [44_100]);
    }

    /// #255: a server that switches its rate while the settings are applied
    /// does not make the apply fail after audio restarted.
    #[test]
    fn applying_while_the_server_switches_rate_runs_at_its_new_rate() {
        let (mut engine, system) = on_pretend_server(None);
        system.run_at(96_000);
        let asked = AudioConfig {
            device: Some(SessionEngine::JACK.into()),
            sample_rate: 48_000,
            buffer_size: 512,
        };
        assert!(engine.plan_audio_config(&asked).unwrap().restarts());
        assert_eq!(engine.apply_audio_config(&asked), Ok(()));
        assert_eq!(
            engine.audio_config(),
            AudioConfig {
                sample_rate: 96_000,
                ..asked
            }
        );
        assert_eq!(engine.status(), running(96_000, 512));
    }

    #[test]
    fn applying_refuses_what_the_engine_cannot_run_and_restarts_nothing() {
        let (mut engine, _system) = on_pretend_server(None);
        let before = engine.audio_config();
        for (config, error) in [
            (
                AudioConfig {
                    device: Some("usb".into()),
                    ..before.clone()
                },
                EngineError::UnknownDevice("usb".into()),
            ),
            (
                AudioConfig {
                    sample_rate: 44_100,
                    ..before.clone()
                },
                EngineError::UnsupportedSampleRate(44_100),
            ),
            (
                AudioConfig {
                    buffer_size: 100,
                    ..before.clone()
                },
                EngineError::UnsupportedBufferSize(100),
            ),
        ] {
            assert_eq!(engine.plan_audio_config(&config), Err(error.clone()));
            assert_eq!(engine.apply_audio_config(&config), Err(error));
        }
        assert!(
            !engine.plan_audio_config(&before).unwrap().restarts(),
            "nothing changes"
        );
        assert_eq!(engine.audio_config(), before);
    }

    #[test]
    fn without_a_server_any_listed_rate_runs_silently() {
        let (mut engine, system) = on_pretend_server(None);
        system.shut_down();
        wait_for_status(&engine, &EngineStatus::Failed(EngineError::ServerLost));
        engine.restart_audio();
        assert!(
            !engine
                .plan_audio_config(&engine.audio_config())
                .unwrap()
                .restarts(),
            "a restart cannot mend audio while the server is away"
        );
        let config = AudioConfig {
            device: Some(SessionEngine::SILENT.into()),
            sample_rate: 88_200,
            buffer_size: 128,
        };
        assert_eq!(engine.apply_audio_config(&config), Ok(()));
        assert_eq!(engine.backend_kind(), BackendKind::Silent);
        assert_eq!(engine.audio_config(), config);
        assert_eq!(
            engine.status(),
            EngineStatus::Failed(EngineError::ServerLost)
        );
    }

    #[test]
    fn stub_accepts_supported_config() {
        let mut engine = StubEngine::default();
        assert_eq!(engine.status(), EngineStatus::Stopped);
        let config = AudioConfig {
            device: Some("stub:usb".into()),
            sample_rate: 96_000,
            buffer_size: 128,
        };
        engine.apply_audio_config(&config).unwrap();
        assert_eq!(engine.audio_config(), config);
        assert_eq!(
            engine.status(),
            EngineStatus::Running {
                sample_rate: 96_000,
                buffer_size: 128
            }
        );
    }

    #[test]
    fn stub_rejects_bad_config_and_keeps_old_one() {
        let mut engine = StubEngine::default();
        let before = engine.audio_config();
        let bad_device = AudioConfig {
            device: Some("nope".into()),
            ..AudioConfig::default()
        };
        assert_eq!(
            engine.apply_audio_config(&bad_device),
            Err(EngineError::UnknownDevice("nope".into()))
        );
        let bad_rate = AudioConfig {
            sample_rate: 12_345,
            ..AudioConfig::default()
        };
        assert_eq!(
            engine.apply_audio_config(&bad_rate),
            Err(EngineError::UnsupportedSampleRate(12_345))
        );
        let bad_size = AudioConfig {
            buffer_size: 100,
            ..AudioConfig::default()
        };
        assert_eq!(
            engine.apply_audio_config(&bad_size),
            Err(EngineError::UnsupportedBufferSize(100))
        );
        assert_eq!(engine.audio_config(), before);
    }

    #[test]
    fn stub_transport() {
        let mut engine = StubEngine::default();
        engine.play();
        assert!(engine.is_playing());
        engine.stop();
        assert!(!engine.is_playing());
    }
}
