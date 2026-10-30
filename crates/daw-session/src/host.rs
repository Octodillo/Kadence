//! Runs a [`Session`] on a real audio backend.
//!
//! [`AudioHost`] opens JACK (on Plasma that is PipeWire through
//! `pipewire-jack`) and moves the session's processor into the backend's
//! callback. If no audio server answers, it falls back to the dummy backend
//! pacing itself in real time, so the app still runs (silently) and the
//! playhead still moves.

use daw_backend::{
    AudioBackend, BackendConfig, BackendError, DummyBackend, JackBackend, Latency, Pacing,
};
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

/// A backend with a session running on it.
pub struct AudioHost {
    backend: Box<dyn AudioBackend>,
    kind: HostKind,
    session: Session,
    config: HostConfig,
    last_error: Option<String>,
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
            auto_connect: true,
            midi_capacity: 1024,
        })?;
        let rate = backend.sample_rate();
        let (session, mut processor) = Session::new(SessionConfig {
            engine: EngineConfig {
                sample_rate: rate,
                max_frames: MAX_FRAMES,
                ..Default::default()
            },
            inputs: 2,
            outputs: 2,
        });
        // Backend MIDI arrives with frame offsets inside the period, which is
        // what the processor wants too.
        backend.start(Box::new(move |cycle| {
            processor.process(
                cycle.frames as usize,
                cycle.audio_in,
                cycle.audio_out,
                cycle.midi_in,
            );
        }))?;
        Ok(Self {
            backend,
            kind,
            session,
            config: config.clone(),
            last_error: None,
        })
    }

    /// Restarts audio with a new configuration. The session is recreated at
    /// the new rate, so sync it again afterwards; the play position is kept.
    pub fn restart(&mut self, config: HostConfig) {
        let position = self.session.position();
        let looped = self.session.loop_range();
        let playing = self.session.is_playing();
        let tempo = self.session.tempo().clone();
        // Saved while the old engine still plays them; restored at the new rate.
        let plugins = self.session.take_plugins();
        let _ = self.backend.close();
        *self = Self::start(config);
        self.session.set_plugins(plugins);
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

impl Drop for AudioHost {
    fn drop(&mut self) {
        // Stop the callback (and drop the processor) before the session.
        let _ = self.backend.close();
    }
}
