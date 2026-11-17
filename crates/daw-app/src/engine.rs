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
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use daw_backend::{DummyBackend, DummySystem, Pacing, PortDirection, PortKind};
use daw_plugin_host::{HostLauncher, SandboxConfig};
pub use daw_session::host::MidiSource;
use daw_session::host::{AudioHost, HostConfig, HostHealth, HostKind};
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
    /// Restart audio with `config`. On error audio keeps running (or stays
    /// failed) and [`audio_config`](Self::audio_config) says with what.
    /// So it does after success: the audio system can come or go during
    /// the restart, and then what runs is not what was asked for.
    fn apply_audio_config(&mut self, config: &AudioConfig) -> Result<(), EngineError>;
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

    /// How an export loads plugins, as the session does: `None` when the
    /// session has no plugin loader either (the stub engine).
    fn export_plugin_loader(&self) -> Option<daw_session::export::LoaderFactory> {
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
/// answers `--plugin-host` and `--scan` before Qt starts (see
/// [`run_plugin_child`](crate::run_plugin_child)). Scan results are cached
/// in `$XDG_CACHE_HOME/kadence/plugins.json`.
fn plugin_loader() -> Option<Box<dyn PluginLoader>> {
    sandbox_loader(|c| c)
}

/// [`plugin_loader`] for an export, which renders faster than real time.
fn export_plugin_loader() -> Option<Box<dyn PluginLoader>> {
    sandbox_loader(daw_session::export::offline_sandbox)
}

fn sandbox_loader(
    adjust: impl FnOnce(SandboxConfig) -> SandboxConfig,
) -> Option<Box<dyn PluginLoader>> {
    let launcher = HostLauncher::current_exe().ok()?;
    let mut loader = SandboxLoader::new(adjust(SandboxConfig::new(launcher)));
    if let Some(file) = plugin_cache_file() {
        loader = loader.with_cache_file(file);
    }
    Some(Box::new(loader))
}

/// Where plugin scan results are kept, shared by the session's loader and
/// the plugin browser's scan.
pub(crate) fn plugin_cache_file() -> Option<PathBuf> {
    crate::paths::cache_dir().map(|dir| dir.join("plugins.json"))
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

/// The configuration the engine starts with: see [`use_quiet_audio`].
fn startup_config() -> HostConfig {
    QUIET
        .get()
        .map_or_else(HostConfig::default, |name| HostConfig::quiet(name.as_str()))
}

/// Starts audio with plugins loading through [`plugin_loader`].
fn start_host(config: HostConfig) -> AudioHost {
    let mut host = AudioHost::start(config);
    host.session_mut().set_plugin_loader(plugin_loader());
    host
}

/// The engine the app starts with: the real one, or the stub when the
/// environment says `KADENCE_AUDIO=stub` (the UI tests do).
pub fn default_engine() -> Box<dyn EngineHandle> {
    if std::env::var("KADENCE_AUDIO").as_deref() == Ok("stub") {
        Box::new(StubEngine::default())
    } else {
        Box::new(SessionEngine::new())
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

    pub fn new() -> Self {
        let host = start_host(startup_config());
        let config = AudioConfig {
            device: None,
            sample_rate: host.sample_rate(),
            buffer_size: host.buffer_size(),
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

impl Default for SessionEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl EngineHandle for SessionEngine {
    fn export_plugin_loader(&self) -> Option<daw_session::export::LoaderFactory> {
        Some(Box::new(export_plugin_loader))
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
            HostKind::Silent => vec![44_100, 48_000, 88_200, 96_000],
        }
    }

    fn supported_buffer_sizes(&self) -> Vec<u32> {
        vec![64, 128, 256, 512, 1024, 2048]
    }

    fn apply_audio_config(&mut self, config: &AudioConfig) -> Result<(), EngineError> {
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
        let wanted = HostConfig {
            sample_rate: config.sample_rate,
            buffer_size: config.buffer_size,
            use_server: true,
            ..self.host.config().clone()
        };
        // Compared with what runs, not with what was last asked for: a
        // server may have settled on another rate since. Also when nothing
        // changed but audio is down: applying the settings again is how the
        // user asks for another try.
        let changed = config.sample_rate != self.config.sample_rate
            || config.buffer_size != self.config.buffer_size
            || !self.host.config().use_server;
        if changed || self.is_failed() {
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
                return Ok(());
            }
            if self.config.sample_rate != config.sample_rate {
                return Err(EngineError::UnsupportedSampleRate(config.sample_rate));
            }
        }
        self.config.device.clone_from(&config.device);
        Ok(())
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
}

impl Default for StubEngine {
    fn default() -> Self {
        let (host, system) = Self::start_host();
        Self {
            config: AudioConfig::default(),
            running: false,
            host,
            system,
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
        vec![44_100, 48_000, 88_200, 96_000]
    }

    fn supported_buffer_sizes(&self) -> Vec<u32> {
        vec![64, 128, 256, 512, 1024, 2048]
    }

    fn apply_audio_config(&mut self, config: &AudioConfig) -> Result<(), EngineError> {
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
        self.config = config.clone();
        self.running = true;
        Ok(())
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
