//! The audio settings and what is wrong with audio: which settings start,
//! what replaces refused ones, when audio restarts, and which problem the
//! user is shown. A front end shows an [`AudioControl`]'s [`AudioView`] and
//! [`Notice`] and passes the user's choices back.

use crate::engine::{
    AudioConfig, AudioDevice, BackendKind, EngineError, EngineHandle, EngineStatus,
};
use crate::message::{Message, RecordProblem};
use crate::settings::{self, SettingsStore};
use crate::state::AppState;

/// What was going on when the problem in a [`Notice`] happened. The front
/// end picks the headline from this and the reason from the [`Problem`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeContext {
    /// The user's new audio settings were refused; the old ones stay.
    ApplyingSettings,
    /// The saved settings were refused at startup and others are in use.
    StartupFallback,
    /// Audio could not start at all, not even with the defaults.
    StartupFailed,
    /// The engine reports audio as failed: it stopped, or (without an
    /// audio server) it plays silently.
    AudioFailed,
}

/// Something the user has to be told about, as data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    Engine(EngineError),
    /// The front end passed a device choice that is not in the list: this
    /// number.
    InvalidDeviceChoice(i64),
    /// The front end passed a number that is no sample rate (negative).
    InvalidSampleRate(i64),
    /// The front end passed a number that is no buffer size (negative).
    InvalidBufferSize(i64),
}

/// The user's choice on the audio settings page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioChoice {
    /// As in [`AudioView::device_index`], of the view last returned.
    pub device: usize,
    pub sample_rate: u32,
    pub buffer_size: u32,
}

/// The problem on show, and when it happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub context: NoticeContext,
    pub problem: Problem,
}

impl Notice {
    fn engine(context: NoticeContext, e: EngineError) -> Self {
        Self {
            context,
            problem: Problem::Engine(e),
        }
    }
}

/// What the audio settings show.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioView {
    pub backend: BackendKind,
    pub running: bool,
    /// The devices besides the backend's default one, which a choice
    /// numbers from 1 (see `device_index`).
    pub devices: Vec<AudioDevice>,
    /// 0 for the default device, else 1 + its place in `devices`.
    pub device_index: usize,
    pub sample_rates: Vec<u32>,
    pub sample_rate: u32,
    pub buffer_sizes: Vec<u32>,
    pub buffer_size: u32,
    /// The problem to show, if any.
    pub notice: Option<Notice>,
}

/// The audio settings and the problem on show, over the app's engine.
#[derive(Debug, Default)]
pub struct AudioControl {
    notice: Option<Notice>,
    /// The engine status the view was last made from.
    status: Option<EngineStatus>,
    /// Device ids as last shown, for [`AudioControl::apply`]'s index.
    device_ids: Vec<String>,
}

impl AudioControl {
    /// Starts audio with the `saved` settings. If they are refused it
    /// falls back to the saved rate and buffer on the default device, then
    /// to what the engine runs at, then to the defaults, and the notice
    /// says why the saved settings are not in use. Returns the view.
    pub fn start(&mut self, s: &mut AppState, saved: Option<&AudioConfig>) -> AudioView {
        if let Some((context, e)) = start_audio(s.engine.as_mut(), saved) {
            self.notice = Some(Notice::engine(context, e));
        }
        // The engine may have restarted with a new session.
        s.resync();
        self.sync(s)
    }

    /// Applies the user's `choice` and saves it in `store` if it took.
    /// Returns whether it took, and the view; its notice says why not.
    pub fn apply(
        &mut self,
        s: &mut AppState,
        store: &mut dyn SettingsStore,
        choice: AudioChoice,
    ) -> (bool, AudioView) {
        let result = self.config_from(choice).and_then(|config| {
            replacing_session(s, |engine| engine.apply_audio_config(&config))
                .map_err(Problem::Engine)?;
            // What runs now, which is not what was asked for when the audio
            // server came or went during the restart.
            settings::save_audio_config(store, &s.engine.audio_config());
            Ok(())
        });
        let ok = result.is_ok();
        self.notice = match result {
            // Straight to what `sync` would show, so a front end sees one
            // change (and does not notify again) when audio stays failed.
            Ok(()) => match s.engine.status() {
                EngineStatus::Failed(e) => Some(Notice::engine(NoticeContext::AudioFailed, e)),
                _ => None,
            },
            Err(problem) => Some(Notice {
                context: NoticeContext::ApplyingSettings,
                problem,
            }),
        };
        (ok, self.sync(s))
    }

    /// Refuses a choice the front end could not make an [`AudioChoice`]
    /// of, for `problem`; the settings in use stay. Returns the view.
    pub fn refuse(&mut self, s: &AppState, problem: Problem) -> AudioView {
        self.notice = Some(Notice {
            context: NoticeContext::ApplyingSettings,
            problem,
        });
        self.sync(s)
    }

    /// Looks at the audio system again: it may have gone away, come back
    /// or changed its sample rate since the last look. Restarts audio where
    /// that mends it. Returns the view if anything changed. Call it every
    /// second or so.
    pub fn poll(&mut self, s: &mut AppState) -> Option<AudioView> {
        let status = poll_audio(s);
        if self.status.as_ref() == Some(&status) {
            return None;
        }
        // A failure that is new outranks a settings refusal still on show
        // from earlier, which `sync` alone would keep.
        if let EngineStatus::Failed(e) = status {
            self.notice = Some(Notice::engine(NoticeContext::AudioFailed, e));
        }
        Some(self.sync(s))
    }

    /// Reads the engine again: after a restart it may be another backend
    /// (a JACK server came or went) with other devices and rates.
    fn sync(&mut self, s: &AppState) -> AudioView {
        let e = &s.engine;
        let (config, status) = (e.audio_config(), e.status());
        let devices = e.audio_devices();
        self.device_ids = devices.iter().map(|d| d.id.clone()).collect();
        match status_error(self.notice.as_ref().map(|n| n.context), &status) {
            StatusError::Keep => {}
            StatusError::Clear => self.notice = None,
            StatusError::Show(e) => {
                self.notice = Some(Notice::engine(NoticeContext::AudioFailed, e))
            }
        }
        let device_index = config
            .device
            .as_ref()
            .and_then(|id| self.device_ids.iter().position(|d| d == id))
            .map_or(0, |i| i + 1);
        let view = AudioView {
            backend: e.backend_kind(),
            running: matches!(status, EngineStatus::Running { .. }),
            devices,
            device_index,
            sample_rates: e.supported_sample_rates(),
            sample_rate: config.sample_rate,
            buffer_sizes: e.supported_buffer_sizes(),
            buffer_size: config.buffer_size,
            notice: self.notice.clone(),
        };
        self.status = Some(status);
        view
    }

    /// Turns `choice` into an [`AudioConfig`].
    fn config_from(&self, choice: AudioChoice) -> Result<AudioConfig, Problem> {
        // Choice 0 is the system default; 1.. are `device_ids`.
        let device = match choice.device {
            0 => None,
            i => Some(self.device_ids.get(i - 1).cloned().ok_or_else(|| {
                Problem::InvalidDeviceChoice(i64::try_from(i).unwrap_or(i64::MAX))
            })?),
        };
        Ok(AudioConfig {
            device,
            sample_rate: choice.sample_rate,
            buffer_size: choice.buffer_size,
        })
    }
}

/// Startup: apply `saved`, else fall back to the saved rate and buffer on
/// the default device, then to what the engine runs at already (a JACK
/// server picks its own rate), then to plain defaults. Returns what to tell the
/// user, if anything: the first refusal if a fallback worked (that is why
/// the saved settings are not in use), else the last one.
///
/// With nothing saved there is nothing to apply or refuse: the engine
/// keeps what it runs at. And an audio server picks its own sample rate, so
/// a saved one it does not run at (saved while there was no server, or
/// before the server changed) gives way without a message.
fn start_audio(
    engine: &mut dyn EngineHandle,
    saved: Option<&AudioConfig>,
) -> Option<(NoticeContext, EngineError)> {
    let saved = saved?;
    let current = engine.audio_config();
    let first = engine.apply_audio_config(saved).err()?;
    if matches!(first, EngineError::UnsupportedSampleRate(_))
        && engine.backend_kind() == BackendKind::Jack
    {
        let at_server_rate = AudioConfig {
            sample_rate: engine.audio_config().sample_rate,
            ..saved.clone()
        };
        if engine.apply_audio_config(&at_server_rate).is_ok() {
            return None;
        }
    }
    let fallbacks = [
        AudioConfig {
            device: None,
            ..saved.clone()
        },
        current,
        AudioConfig::default(),
    ];
    let mut last = first.clone();
    for fallback in &fallbacks {
        match engine.apply_audio_config(fallback) {
            Ok(()) => return Some((NoticeContext::StartupFallback, first)),
            Err(e) => last = e,
        }
    }
    Some((NoticeContext::StartupFailed, last))
}

/// Runs `change`, which may restart audio and with it replace the session.
/// The take made so far is kept, and the fresh session gets the project
/// and the play position back (after the tempo map, so the position maps
/// to the same bar).
fn replacing_session<R>(s: &mut AppState, change: impl FnOnce(&mut dyn EngineHandle) -> R) -> R {
    s.stop_recording();
    let position = s.playhead();
    let r = change(s.engine.as_mut());
    s.resync();
    if let Some(session) = s.engine.session_mut() {
        let _ = session.locate(position);
    }
    r
}

/// Restarts audio when the engine says that mends it (see
/// [`EngineHandle::wants_restart`]) and returns the status after that. A
/// recording does not survive the restart, and the user is told.
fn poll_audio(s: &mut AppState) -> EngineStatus {
    if s.engine.wants_restart() {
        if s.is_recording() {
            // First, so that a problem with the take itself, reported as
            // the recording stops, is what stays on show.
            s.report(Message::Record(RecordProblem::AudioRestarted));
        }
        replacing_session(s, |engine| engine.restart_audio());
    }
    s.engine.status()
}

/// What `sync` does to the notice for an engine status.
#[derive(Debug, PartialEq, Eq)]
enum StatusError {
    Keep,
    Clear,
    /// Show this as `AudioFailed`.
    Show(EngineError),
}

/// `current` is the context of the notice on show. A failed status wins
/// over a startup message, since it is why there is no sound now (a saved
/// "jack" device is also gone when the server is). It does not replace a
/// settings refusal the user just caused; that one says what to fix. An
/// `AudioFailed` goes away once audio runs again.
fn status_error(current: Option<NoticeContext>, status: &EngineStatus) -> StatusError {
    match status {
        EngineStatus::Failed(e) if current != Some(NoticeContext::ApplyingSettings) => {
            StatusError::Show(e.clone())
        }
        EngineStatus::Failed(_) => StatusError::Keep,
        _ if current == Some(NoticeContext::AudioFailed) => StatusError::Clear,
        _ => StatusError::Keep,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::StubEngine;
    use std::cell::RefCell;
    use std::rc::Rc;

    use crate::settings::MemoryStore;

    #[test]
    fn a_lost_audio_server_shows_and_is_mended_by_the_next_poll() {
        let engine = StubEngine::default();
        let system = engine.system().clone();
        let mut s = AppState::with_engine(Box::new(engine));
        s.set_bpm(90.0).unwrap();
        assert_eq!(poll_audio(&mut s), EngineStatus::Stopped);

        system.shut_down();
        // The audio thread notices at its next cycle.
        let lost = EngineStatus::Failed(EngineError::ServerLost);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while s.engine.status() != lost && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(s.engine.status(), lost);
        // The restart brings a fresh session, which gets the project back.
        assert_eq!(poll_audio(&mut s), EngineStatus::Stopped);
        assert!(!s.engine.wants_restart());
        assert_eq!(s.tempo_at_playhead().0, 90.0);
        let session = s.engine.session().unwrap();
        assert_eq!(session.tempo().segments()[0].bpm, 90.0);
    }

    #[test]
    fn startup_with_good_settings_reports_nothing() {
        let mut engine = StubEngine::default();
        let saved = AudioConfig {
            device: Some("stub:usb".into()),
            sample_rate: 96_000,
            buffer_size: 128,
        };
        assert_eq!(start_audio(&mut engine, Some(&saved)), None);
        assert_eq!(engine.audio_config(), saved);
    }

    #[test]
    fn startup_fallback_reports_why_the_saved_settings_failed() {
        // Device gone and rate unsupported: the first fallback still fails
        // on the rate, the defaults work. The user should hear about the
        // device, which is what made the saved settings unusable.
        let mut engine = StubEngine::default();
        let saved = AudioConfig {
            device: Some("stub:gone".into()),
            sample_rate: 12_345,
            buffer_size: 128,
        };
        assert_eq!(
            start_audio(&mut engine, Some(&saved)),
            Some((
                NoticeContext::StartupFallback,
                EngineError::UnknownDevice("stub:gone".into())
            ))
        );
        assert_eq!(engine.audio_config(), AudioConfig::default());
    }

    #[test]
    fn startup_fallback_keeps_saved_rate_on_default_device() {
        let mut engine = StubEngine::default();
        let saved = AudioConfig {
            device: Some("stub:gone".into()),
            sample_rate: 96_000,
            buffer_size: 128,
        };
        let error = start_audio(&mut engine, Some(&saved));
        assert_eq!(error.map(|(c, _)| c), Some(NoticeContext::StartupFallback));
        assert_eq!(
            engine.audio_config(),
            AudioConfig {
                device: None,
                ..saved
            }
        );
    }

    #[test]
    fn status_failure_wins_over_startup_but_not_settings_errors() {
        let failed = EngineStatus::Failed(EngineError::Backend("gone".into()));
        let running = EngineStatus::Running {
            sample_rate: 48_000,
            buffer_size: 256,
        };
        let shown = StatusError::Show(EngineError::Backend("gone".into()));
        for context in [
            None,
            Some(NoticeContext::AudioFailed),
            Some(NoticeContext::StartupFallback),
            Some(NoticeContext::StartupFailed),
        ] {
            assert_eq!(status_error(context, &failed), shown);
        }
        assert_eq!(
            status_error(Some(NoticeContext::ApplyingSettings), &failed),
            StatusError::Keep
        );
        for context in [
            NoticeContext::ApplyingSettings,
            NoticeContext::StartupFallback,
            NoticeContext::StartupFailed,
        ] {
            assert_eq!(status_error(Some(context), &running), StatusError::Keep);
        }
        assert_eq!(
            status_error(Some(NoticeContext::AudioFailed), &running),
            StatusError::Clear
        );
        assert_eq!(
            status_error(None, &EngineStatus::Stopped),
            StatusError::Keep
        );
    }

    fn choice(device: usize, sample_rate: u32) -> AudioChoice {
        AudioChoice {
            device,
            sample_rate,
            buffer_size: 128,
        }
    }

    fn refusal(problem: Problem) -> Option<Notice> {
        Some(Notice {
            context: NoticeContext::ApplyingSettings,
            problem,
        })
    }

    #[test]
    fn applied_settings_are_saved_and_refused_ones_say_why() {
        let mut s = AppState::with_engine(Box::new(StubEngine::default()));
        let mut store = MemoryStore::default();
        let mut control = AudioControl::default();
        let view = control.start(&mut s, None);
        assert_eq!(view.notice, None);
        let device = view.devices.len();
        assert!(device > 0, "the stub offers devices");

        let (ok, view) = control.apply(&mut s, &mut store, choice(device, 96_000));
        assert!(ok);
        assert_eq!((view.device_index, view.sample_rate), (device, 96_000));
        let saved = settings::load_audio_config(&store);
        assert_eq!(saved.as_ref().map(|c| c.sample_rate), Some(96_000));

        // A choice not in the list.
        let (ok, view) = control.apply(&mut s, &mut store, choice(device + 1, 48_000));
        assert!(!ok);
        let number = i64::try_from(device + 1).unwrap();
        assert_eq!(view.notice, refusal(Problem::InvalidDeviceChoice(number)));
        // One the engine refuses: the old settings stay, in use and saved.
        let (ok, view) = control.apply(&mut s, &mut store, choice(device, 12_345));
        assert!(!ok);
        assert_eq!(
            view.notice,
            refusal(Problem::Engine(EngineError::UnsupportedSampleRate(12_345)))
        );
        assert_eq!(view.sample_rate, 96_000);
        assert_eq!(settings::load_audio_config(&store), saved);
        // One the front end could not make.
        let view = control.refuse(&s, Problem::InvalidSampleRate(-1));
        assert_eq!(view.notice, refusal(Problem::InvalidSampleRate(-1)));
        // A good choice clears it.
        let (ok, view) = control.apply(&mut s, &mut store, choice(0, 48_000));
        assert!(ok);
        assert_eq!(view.notice, None);
    }

    #[test]
    fn a_new_failure_outranks_a_settings_refusal_and_polls_report_changes_only() {
        let engine = FakeEngine::default();
        let status = engine.status.clone();
        let mut s = AppState::with_engine(Box::new(engine));
        let mut store = MemoryStore::default();
        let mut control = AudioControl::default();
        control.start(&mut s, None);
        assert_eq!(control.poll(&mut s), None, "nothing changed");

        let (ok, _) = control.apply(&mut s, &mut store, choice(99, 48_000));
        assert!(!ok);
        assert_eq!(control.poll(&mut s), None, "the refusal stays on show");

        // Audio fails: that replaces the refusal, which `sync` alone keeps.
        let lost = EngineError::ServerLost;
        *status.borrow_mut() = Some(EngineStatus::Failed(lost.clone()));
        let view = control.poll(&mut s).expect("the status changed");
        assert_eq!(
            view.notice,
            Some(Notice::engine(NoticeContext::AudioFailed, lost))
        );
        assert_eq!(control.poll(&mut s), None);
        // Audio runs again: the failure goes.
        *status.borrow_mut() = None;
        let view = control.poll(&mut s).expect("the status changed");
        assert_eq!(view.notice, None);
    }

    #[test]
    fn settings_that_apply_while_audio_stays_failed_show_failed_audio() {
        let failed = EngineError::Backend("no server".into());
        let engine = FakeEngine {
            status: Rc::new(RefCell::new(Some(EngineStatus::Failed(failed.clone())))),
            ..FakeEngine::default()
        };
        let mut s = AppState::with_engine(Box::new(engine));
        let mut store = MemoryStore::default();
        let mut control = AudioControl::default();
        control.start(&mut s, None);
        let (ok, view) = control.apply(&mut s, &mut store, choice(0, 48_000));
        assert!(ok);
        assert_eq!(
            view.notice,
            Some(Notice::engine(NoticeContext::AudioFailed, failed))
        );
    }

    /// [`StubEngine`] made difficult: running only at `fixed_rate` (like a
    /// JACK server the app cannot change), refusing every configuration
    /// (like a machine with no working audio), or reporting `status`.
    #[derive(Default)]
    struct FakeEngine {
        stub: StubEngine,
        fixed_rate: Option<u32>,
        refuse_all: bool,
        /// Shared, so a test can change it while the app holds the engine.
        status: Rc<RefCell<Option<EngineStatus>>>,
    }

    impl EngineHandle for FakeEngine {
        fn backend_kind(&self) -> BackendKind {
            self.stub.backend_kind()
        }
        fn audio_devices(&self) -> Vec<AudioDevice> {
            self.stub.audio_devices()
        }
        fn supported_sample_rates(&self) -> Vec<u32> {
            self.fixed_rate
                .map_or_else(|| self.stub.supported_sample_rates(), |r| vec![r])
        }
        fn supported_buffer_sizes(&self) -> Vec<u32> {
            self.stub.supported_buffer_sizes()
        }
        fn apply_audio_config(&mut self, config: &AudioConfig) -> Result<(), EngineError> {
            if self.refuse_all {
                return Err(EngineError::Backend(format!(
                    "no luck at {}",
                    config.sample_rate
                )));
            }
            if self.fixed_rate.is_some_and(|r| r != config.sample_rate) {
                return Err(EngineError::UnsupportedSampleRate(config.sample_rate));
            }
            self.stub.apply_audio_config(config)
        }
        fn audio_config(&self) -> AudioConfig {
            let config = self.stub.audio_config();
            AudioConfig {
                sample_rate: self.fixed_rate.unwrap_or(config.sample_rate),
                ..config
            }
        }
        fn status(&self) -> EngineStatus {
            self.status
                .borrow()
                .clone()
                .unwrap_or_else(|| self.stub.status())
        }
        fn play(&mut self) {}
        fn stop(&mut self) {}
        fn is_playing(&self) -> bool {
            false
        }
        fn session(&self) -> Option<&daw_session::Session> {
            None
        }
        fn session_mut(&mut self) -> Option<&mut daw_session::Session> {
            None
        }
    }

    #[test]
    fn startup_falls_back_to_the_rate_the_engine_runs_at() {
        // Fresh config (48 kHz) against a 44.1 kHz server: a fallback, not
        // "could not start".
        let mut engine = FakeEngine {
            fixed_rate: Some(44_100),
            ..FakeEngine::default()
        };
        assert_eq!(
            start_audio(&mut engine, Some(&AudioConfig::default())),
            Some((
                NoticeContext::StartupFallback,
                EngineError::UnsupportedSampleRate(48_000)
            ))
        );
        assert_eq!(engine.audio_config().sample_rate, 44_100);
    }

    #[test]
    fn startup_failure_reports_the_last_refusal() {
        let mut engine = FakeEngine {
            refuse_all: true,
            ..FakeEngine::default()
        };
        let saved = AudioConfig {
            sample_rate: 96_000,
            ..AudioConfig::default()
        };
        assert_eq!(
            start_audio(&mut engine, Some(&saved)),
            Some((
                NoticeContext::StartupFailed,
                EngineError::Backend("no luck at 48000".into())
            ))
        );
    }
}
