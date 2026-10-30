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

use daw_session::Session;
use daw_session::host::{AudioHost, HostConfig, HostKind};

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
            Self::Backend(message) => write!(f, "audio system error: {message}"),
        }
    }
}

impl std::error::Error for EngineError {}

/// The engine as the UI sees it. Called on the UI thread only.
pub trait EngineHandle: Send {
    fn backend_kind(&self) -> BackendKind;
    fn audio_devices(&self) -> Vec<AudioDevice>;
    fn supported_sample_rates(&self) -> Vec<u32>;
    fn supported_buffer_sizes(&self) -> Vec<u32>;
    /// Restart audio with `config`. On error the previous config stays live.
    fn apply_audio_config(&mut self, config: &AudioConfig) -> Result<(), EngineError>;
    fn audio_config(&self) -> AudioConfig;
    fn status(&self) -> EngineStatus;

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

    /// The session playing the project. After
    /// [`apply_audio_config`](Self::apply_audio_config) it may be a new one
    /// that has to be synced with the document again.
    fn session(&self) -> Option<&Session>;
    fn session_mut(&mut self) -> Option<&mut Session>;
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
}

impl SessionEngine {
    const JACK: &'static str = "jack";
    const SILENT: &'static str = "silent";

    pub fn new() -> Self {
        let host = AudioHost::start(HostConfig::default());
        let config = AudioConfig {
            device: None,
            sample_rate: host.sample_rate(),
            buffer_size: host.buffer_size(),
        };
        Self { host, config }
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
        };
        if wanted != *self.host.config() {
            self.host.restart(wanted);
        }
        self.config = config.clone();
        Ok(())
    }

    fn audio_config(&self) -> AudioConfig {
        self.config.clone()
    }

    fn status(&self) -> EngineStatus {
        match (self.host.kind(), self.host.last_error()) {
            (HostKind::Silent, Some(e)) => EngineStatus::Failed(EngineError::Backend(e.to_owned())),
            _ => EngineStatus::Running {
                sample_rate: self.host.sample_rate(),
                buffer_size: self.host.buffer_size(),
            },
        }
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
}

/// A pretend engine with two devices, for testing the settings UI without
/// an audio server. It plays through a silent backend so the transport and
/// arrange view behave as with real audio.
pub struct StubEngine {
    config: AudioConfig,
    running: bool,
    host: AudioHost,
}

impl Default for StubEngine {
    fn default() -> Self {
        Self {
            config: AudioConfig::default(),
            running: false,
            host: AudioHost::start(HostConfig {
                use_server: false,
                ..Default::default()
            }),
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
    const DEVICES: [(&'static str, &'static str); 2] = [
        ("stub:default", "Default output (stub)"),
        ("stub:usb", "USB audio interface (stub)"),
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
        if self.running {
            EngineStatus::Running {
                sample_rate: self.config.sample_rate,
                buffer_size: self.config.buffer_size,
            }
        } else {
            EngineStatus::Stopped
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
