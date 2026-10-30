//! What the UI needs from the audio engine, without naming the engine.
//!
//! [`EngineHandle`] is the only door from the front end into audio. It uses
//! plain Rust types so a second front end (a GNOME one is planned) can share
//! it unchanged; nothing here may mention Qt. The real implementation arrives
//! with the audio backend (WU2). Until then [`StubEngine`] stands in.

use std::fmt;

/// An audio device the user can pick in the settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioDevice {
    /// Stable identifier, stored in the config file.
    pub id: String,
    /// Human-readable name for the device list.
    pub name: String,
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
    /// The backend refused to start; the message is for the user.
    #[allow(dead_code, reason = "only the real backend (WU2) fails")]
    Failed(String),
}

/// Why an [`EngineHandle`] call was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineError {
    UnknownDevice(String),
    Unsupported(String),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownDevice(id) => write!(f, "unknown audio device “{id}”"),
            Self::Unsupported(what) => write!(f, "not supported: {what}"),
        }
    }
}

impl std::error::Error for EngineError {}

/// The engine as the UI sees it. Called on the UI thread only.
pub trait EngineHandle: Send {
    /// Name of the audio backend, such as "JACK (PipeWire)".
    fn backend_name(&self) -> String;
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
}

/// The engine the app starts with. WU2 swaps this for the real backend.
pub fn default_engine() -> Box<dyn EngineHandle> {
    Box::new(StubEngine::default())
}

/// A pretend engine with two devices and no sound, so the UI can be built
/// and tested before the audio backend exists.
#[derive(Debug, Default)]
pub struct StubEngine {
    config: AudioConfig,
    running: bool,
    playing: bool,
}

impl StubEngine {
    const DEVICES: [(&'static str, &'static str); 2] = [
        ("stub:default", "Default output (stub)"),
        ("stub:usb", "USB audio interface (stub)"),
    ];
}

impl EngineHandle for StubEngine {
    fn backend_name(&self) -> String {
        "Stub (no audio)".to_owned()
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
            return Err(EngineError::Unsupported(format!(
                "sample rate {} Hz",
                config.sample_rate
            )));
        }
        if !self.supported_buffer_sizes().contains(&config.buffer_size) {
            return Err(EngineError::Unsupported(format!(
                "buffer size {}",
                config.buffer_size
            )));
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
        self.playing = true;
    }

    fn stop(&mut self) {
        self.playing = false;
    }

    fn is_playing(&self) -> bool {
        self.playing
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
        assert!(engine.apply_audio_config(&bad_rate).is_err());
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
