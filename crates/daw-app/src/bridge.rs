//! `Engine`: the QML-facing wrapper around an [`EngineHandle`].
//!
//! This is the only place engine state is turned into Qt types. User-facing
//! wording lives in QML so it goes through KI18n.

use std::pin::Pin;

use cxx_qt::CxxQtType;
use cxx_qt_lib::{QList, QString, QStringList};

use crate::engine::{AudioConfig, BackendKind, EngineError, EngineHandle, EngineStatus};
use crate::message::{Message, RecordProblem};
use crate::qt_lists::{list, string_list};
use crate::settings::{self, KConfigStore};
use crate::state;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qstringlist.h");
        type QStringList = cxx_qt_lib::QStringList;
        include!("cxx-qt-lib/qlist.h");
        type QList_i32 = cxx_qt_lib::QList<i32>;
    }

    /// What was going on when the error in `errorKind` happened. QML picks
    /// the headline from this and the reason from `errorKind`.
    #[qenum(Engine)]
    enum ErrorContext {
        /// No error to show.
        NoError,
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

    /// Why it went wrong. `errorNumber` and `errorDetail` hold the data the
    /// message needs, so it can be worded and formatted in QML.
    #[qenum(Engine)]
    enum ErrorKind {
        /// `errorDetail` is the device id.
        UnknownDevice,
        /// `errorNumber` is the rate in Hz.
        UnsupportedSampleRate,
        /// `errorNumber` is the size in samples.
        UnsupportedBufferSize,
        /// `errorDetail` is the audio system's own (untranslated) message.
        BackendError,
        /// The audio server went away and is not back yet.
        ServerLost,
        /// The engine stopped after an internal error.
        EngineCrashed,
        /// Audio kept failing and the engine stopped restarting it.
        AudioUnstable,
        /// `errorNumber` is the device combo box index.
        InvalidDeviceChoice,
        /// `errorNumber` is the sample rate given.
        InvalidSampleRate,
        /// `errorNumber` is the buffer size given.
        InvalidBufferSize,
    }

    /// The audio system in `backendKind`; QML names it.
    #[qenum(Engine)]
    enum Backend {
        JackBackend,
        SilentBackend,
        /// The test engine.
        StubBackend,
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        /// A `Backend`.
        #[qproperty(i32, backend_kind)]
        #[qproperty(bool, running)]
        /// An `ErrorContext`; `NoError` when there is nothing to show.
        #[qproperty(i32, error_context)]
        /// An `ErrorKind`; meaningless while `error_context` is `NoError`.
        #[qproperty(i32, error_kind)]
        #[qproperty(i32, error_number)]
        #[qproperty(QString, error_detail)]
        /// Device display names; index 0 is the backend's default device.
        #[qproperty(QStringList, device_names)]
        #[qproperty(i32, device_index)]
        #[qproperty(QList_i32, sample_rates)]
        #[qproperty(i32, sample_rate)]
        #[qproperty(QList_i32, buffer_sizes)]
        #[qproperty(i32, buffer_size)]
        type Engine = super::EngineRust;

        /// Apply and save new audio settings. Returns false if they were
        /// refused, in which case the old settings stay live and the error
        /// properties say why.
        #[qinvokable]
        fn apply_audio_settings(
            self: Pin<&mut Engine>,
            device_index: i32,
            sample_rate: i32,
            buffer_size: i32,
        ) -> bool;

        /// Looks at the audio system again: it may have gone away, come
        /// back or changed its sample rate since the last look. Restarts
        /// audio where that mends it and updates the properties. Call it
        /// every second or so.
        #[qinvokable]
        fn poll(self: Pin<&mut Engine>);
    }

    impl cxx_qt::Initialize for Engine {}
}

#[derive(Default)]
pub struct EngineRust {
    backend_kind: i32,
    running: bool,
    error_context: i32,
    error_kind: i32,
    error_number: i32,
    error_detail: QString,
    device_names: QStringList,
    device_index: i32,
    sample_rates: QList<i32>,
    sample_rate: i32,
    buffer_sizes: QList<i32>,
    buffer_size: i32,
    /// Device ids, parallel to `device_names` (minus the default entry).
    device_ids: Vec<String>,
    /// The engine status the properties were last made from.
    status: Option<EngineStatus>,
}

/// Something the UI has to tell the user about, as data. Kept free of Qt
/// so it can be unit tested; see [`Problem::parts`].
#[derive(Clone, Debug, PartialEq, Eq)]
enum Problem {
    Engine(EngineError),
    /// The settings page passed a device index that is not in the list.
    InvalidDeviceChoice(i32),
    InvalidSampleRate(i32),
    InvalidBufferSize(i32),
}

impl Problem {
    /// The `errorKind`, `errorNumber` and `errorDetail` QML sees.
    fn parts(&self) -> (qobject::ErrorKind, i32, &str) {
        use qobject::ErrorKind as K;
        let number = |n: u32| i32::try_from(n).unwrap_or(i32::MAX);
        match self {
            Self::Engine(EngineError::UnknownDevice(id)) => (K::UnknownDevice, 0, id),
            Self::Engine(EngineError::UnsupportedSampleRate(rate)) => {
                (K::UnsupportedSampleRate, number(*rate), "")
            }
            Self::Engine(EngineError::UnsupportedBufferSize(size)) => {
                (K::UnsupportedBufferSize, number(*size), "")
            }
            Self::Engine(EngineError::ServerLost) => (K::ServerLost, 0, ""),
            Self::Engine(EngineError::Crashed) => (K::EngineCrashed, 0, ""),
            Self::Engine(EngineError::Unstable) => (K::AudioUnstable, 0, ""),
            Self::Engine(EngineError::Backend(message)) => (K::BackendError, 0, message),
            Self::InvalidDeviceChoice(index) => (K::InvalidDeviceChoice, *index, ""),
            Self::InvalidSampleRate(rate) => (K::InvalidSampleRate, *rate, ""),
            Self::InvalidBufferSize(size) => (K::InvalidBufferSize, *size, ""),
        }
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
) -> Option<(qobject::ErrorContext, EngineError)> {
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
            Ok(()) => return Some((qobject::ErrorContext::StartupFallback, first)),
            Err(e) => last = e,
        }
    }
    Some((qobject::ErrorContext::StartupFailed, last))
}

/// Runs `change`, which may restart audio and with it replace the session.
/// The take made so far is kept, and the fresh session gets the project
/// and the play position back (after the tempo map, so the position maps
/// to the same bar).
///
/// A take in progress ends, and the user is told: a restart the settings
/// page asked for stops it as surely as one the engine made itself.
fn replacing_session<R>(
    s: &mut state::AppState,
    change: impl FnOnce(&mut dyn EngineHandle) -> R,
) -> R {
    if s.is_recording() {
        // First, so that a problem with the take itself, reported as
        // the recording stops, is what stays on show.
        s.report(Message::Record(RecordProblem::AudioRestarted));
    }
    s.stop_recording();
    let position = s.playhead();
    let r = change(s.engine.as_mut());
    s.resync();
    if let Some(session) = s.engine.session_mut() {
        let _ = session.locate(position);
    }
    r
}

/// Applies audio settings the user chose. When that restarts audio, a
/// recording ends first and the user is told, as for any restart.
fn apply_settings(s: &mut state::AppState, config: &AudioConfig) -> Result<(), EngineError> {
    let plan = s.engine.plan_audio_config(config)?;
    if plan.restarts() {
        replacing_session(s, |engine| engine.apply_audio_plan(plan));
    } else {
        s.engine.apply_audio_plan(plan);
    }
    Ok(())
}

/// Restarts audio when the engine says that mends it (see
/// [`EngineHandle::wants_restart`]) and returns the status after that. A
/// recording does not survive the restart, and the user is told.
fn poll_audio(s: &mut state::AppState) -> EngineStatus {
    if s.engine.wants_restart() {
        replacing_session(s, |engine| engine.restart_audio());
    }
    s.engine.status()
}

/// What `sync` does to the error properties for an engine status.
#[derive(Debug, PartialEq, Eq)]
enum StatusError {
    Keep,
    Clear,
    /// Show this as `AudioFailed`.
    Show(EngineError),
}

/// `current` is the `ErrorContext` on show. A failed status wins over a
/// startup message, since it is why there is no sound now (a saved "jack"
/// device is also gone when the server is). It does not replace a
/// settings refusal the user just caused; that one says what to fix.
/// An `AudioFailed` goes away once audio runs again.
fn status_error(current: i32, status: EngineStatus) -> StatusError {
    use qobject::ErrorContext as C;
    match status {
        EngineStatus::Failed(e) if current != C::ApplyingSettings.repr => StatusError::Show(e),
        EngineStatus::Failed(_) => StatusError::Keep,
        _ if current == C::AudioFailed.repr => StatusError::Clear,
        _ => StatusError::Keep,
    }
}

fn to_qlist(values: &[u32]) -> QList<i32> {
    list(values.iter().map(|&v| i32::try_from(v).unwrap_or(i32::MAX)))
}

impl cxx_qt::Initialize for qobject::Engine {
    fn initialize(mut self: Pin<&mut Self>) {
        // Most likely failure: the saved device was unplugged.
        let saved = settings::load_audio_config(&KConfigStore);
        let error = state::with(|s| {
            let error = start_audio(s.engine.as_mut(), saved.as_ref());
            // The engine may have restarted with a new session.
            s.resync();
            error
        });
        if let Some((context, e)) = error {
            self.as_mut().set_error(context, Some(&Problem::Engine(e)));
        }
        self.sync();
    }
}

impl qobject::Engine {
    fn apply_audio_settings(
        mut self: Pin<&mut Self>,
        device_index: i32,
        sample_rate: i32,
        buffer_size: i32,
    ) -> bool {
        let result = self
            .as_ref()
            .audio_config_from(device_index, sample_rate, buffer_size);
        let result = result.and_then(|config| {
            state::with(|s| apply_settings(s, &config)).map_err(Problem::Engine)?;
            // What runs now, which is not what was asked for when the
            // audio server came or went during the restart.
            let config = state::with(|s| s.engine.audio_config());
            settings::save_audio_config(&mut KConfigStore, &config);
            Ok(())
        });
        let ok = result.is_ok();
        match result {
            // Go straight to what sync() would show, so QML sees one change
            // (and Main.qml does not notify again) when audio stays failed.
            Ok(()) => match state::with(|s| s.engine.status()) {
                EngineStatus::Failed(e) => self.as_mut().set_error(
                    qobject::ErrorContext::AudioFailed,
                    Some(&Problem::Engine(e)),
                ),
                _ => self
                    .as_mut()
                    .set_error(qobject::ErrorContext::NoError, None),
            },
            Err(problem) => self
                .as_mut()
                .set_error(qobject::ErrorContext::ApplyingSettings, Some(&problem)),
        }
        self.sync();
        ok
    }

    fn poll(mut self: Pin<&mut Self>) {
        let status = state::with(poll_audio);
        if self.status.as_ref() == Some(&status) {
            return;
        }
        // A failure that is new outranks a settings refusal still on show
        // from earlier, which `sync` alone would keep.
        if let EngineStatus::Failed(e) = status {
            self.as_mut().set_error(
                qobject::ErrorContext::AudioFailed,
                Some(&Problem::Engine(e)),
            );
        }
        self.sync();
    }

    /// Show `problem` (or nothing, with `NoError`) through the error
    /// properties. The context goes last so that QML reacting to its change
    /// sees the matching kind and arguments.
    fn set_error(
        mut self: Pin<&mut Self>,
        context: qobject::ErrorContext,
        problem: Option<&Problem>,
    ) {
        let (kind, number, detail) = problem.map_or((0, 0, ""), |p| {
            let (kind, number, detail) = p.parts();
            (kind.repr, number, detail)
        });
        self.as_mut().set_error_kind(kind);
        self.as_mut().set_error_number(number);
        self.as_mut().set_error_detail(QString::from(detail));
        self.as_mut().set_error_context(context.repr);
    }

    /// Turn the settings page's combo box values into an [`AudioConfig`].
    fn audio_config_from(
        &self,
        device_index: i32,
        sample_rate: i32,
        buffer_size: i32,
    ) -> Result<AudioConfig, Problem> {
        // Index 0 is the system default; 1.. are `device_ids`.
        let device = match usize::try_from(device_index) {
            Ok(0) => None,
            Ok(i) => Some(
                self.device_ids
                    .get(i - 1)
                    .cloned()
                    .ok_or(Problem::InvalidDeviceChoice(device_index))?,
            ),
            Err(_) => return Err(Problem::InvalidDeviceChoice(device_index)),
        };
        let sample_rate =
            u32::try_from(sample_rate).map_err(|_| Problem::InvalidSampleRate(sample_rate))?;
        let buffer_size =
            u32::try_from(buffer_size).map_err(|_| Problem::InvalidBufferSize(buffer_size))?;
        Ok(AudioConfig {
            device,
            sample_rate,
            buffer_size,
        })
    }

    /// Re-reads what the engine offers: after a restart it may be another
    /// backend (a JACK server came or went) with other devices and rates.
    fn refresh_devices(mut self: Pin<&mut Self>) {
        let (devices, backend, rates, sizes) = state::with(|s| {
            let e = &s.engine;
            (
                e.audio_devices(),
                e.backend_kind(),
                e.supported_sample_rates(),
                e.supported_buffer_sizes(),
            )
        });
        let ids: Vec<String> = devices.iter().map(|d| d.id.clone()).collect();
        let backend = match backend {
            BackendKind::Jack => qobject::Backend::JackBackend,
            BackendKind::Silent => qobject::Backend::SilentBackend,
            BackendKind::Stub => qobject::Backend::StubBackend,
        };
        // Setting a property to its value still notifies, and QML would
        // rebuild the combo boxes; only set what changed.
        if ids != self.device_ids || self.device_names.len() != devices.len() as isize + 1 {
            // QML replaces the first entry, and empty device names, with
            // translated labels.
            let names =
                string_list(std::iter::once("").chain(devices.iter().map(|d| d.name.as_str())));
            self.as_mut().rust_mut().device_ids = ids;
            self.as_mut().set_device_names(names);
        }
        if self.backend_kind != backend.repr {
            self.as_mut().set_backend_kind(backend.repr);
        }
        let (rates, sizes) = (to_qlist(&rates), to_qlist(&sizes));
        if self.sample_rates != rates {
            self.as_mut().set_sample_rates(rates);
        }
        if self.buffer_sizes != sizes {
            self.as_mut().set_buffer_sizes(sizes);
        }
    }

    /// Copy engine state into the Qt properties.
    fn sync(mut self: Pin<&mut Self>) {
        self.as_mut().refresh_devices();
        let (config, status) = state::with(|s| (s.engine.audio_config(), s.engine.status()));
        let running = matches!(status, EngineStatus::Running { .. });
        self.as_mut().rust_mut().status = Some(status.clone());
        let device_index = config
            .device
            .as_ref()
            .and_then(|id| self.device_ids.iter().position(|d| d == id))
            .map_or(0, |i| i as i32 + 1);
        match status_error(self.error_context, status) {
            StatusError::Keep => {}
            StatusError::Clear => self
                .as_mut()
                .set_error(qobject::ErrorContext::NoError, None),
            StatusError::Show(e) => self.as_mut().set_error(
                qobject::ErrorContext::AudioFailed,
                Some(&Problem::Engine(e)),
            ),
        }
        self.as_mut().set_running(running);
        self.as_mut().set_device_index(device_index);
        self.as_mut()
            .set_sample_rate(i32::try_from(config.sample_rate).unwrap_or(0));
        self.as_mut()
            .set_buffer_size(i32::try_from(config.buffer_size).unwrap_or(0));
    }
}

#[cfg(test)]
mod tests {
    use super::qobject::{ErrorContext, ErrorKind};
    use super::*;
    use crate::engine::{AudioPlan, StubEngine};

    #[test]
    fn a_lost_audio_server_shows_and_is_mended_by_the_next_poll() {
        let engine = StubEngine::default();
        let system = engine.system().clone();
        let mut s = state::AppState::with_engine(Box::new(engine));
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

    /// The stub, but every applied setting restarts audio, as on a server
    /// whose buffer size changes.
    struct RestartingEngine(StubEngine);

    impl EngineHandle for RestartingEngine {
        fn backend_kind(&self) -> BackendKind {
            self.0.backend_kind()
        }
        fn audio_devices(&self) -> Vec<crate::engine::AudioDevice> {
            self.0.audio_devices()
        }
        fn supported_sample_rates(&self) -> Vec<u32> {
            self.0.supported_sample_rates()
        }
        fn supported_buffer_sizes(&self) -> Vec<u32> {
            self.0.supported_buffer_sizes()
        }
        fn plan_audio_config(&self, config: &AudioConfig) -> Result<AudioPlan, EngineError> {
            self.0.plan_audio_config(config)?;
            Ok(AudioPlan::new(config.clone(), true))
        }
        fn apply_audio_plan(&mut self, plan: AudioPlan) {
            self.0.apply_audio_plan(plan)
        }
        fn audio_config(&self) -> AudioConfig {
            self.0.audio_config()
        }
        fn status(&self) -> EngineStatus {
            self.0.status()
        }
        fn play(&mut self) {
            self.0.play()
        }
        fn stop(&mut self) {
            self.0.stop()
        }
        fn is_playing(&self) -> bool {
            self.0.is_playing()
        }
        fn session(&self) -> Option<&daw_session::Session> {
            self.0.session()
        }
        fn session_mut(&mut self) -> Option<&mut daw_session::Session> {
            self.0.session_mut()
        }
    }

    /// A state with an armed audio track, recording a take.
    fn recording_on(engine: Box<dyn EngineHandle>) -> (state::AppState, daw_model::TrackId) {
        let mut s = state::AppState::with_engine(engine);
        let mut track = daw_model::Track::new(daw_model::TrackKind::Audio, "Guitar");
        track.armed = true;
        let id = track.id;
        s.doc
            .execute(daw_model::commands::AddTrack::new(0, track))
            .unwrap();
        s.sync();
        s.start_recording().unwrap();
        assert!(s.is_recording(), "{:?}", s.status);
        std::thread::sleep(std::time::Duration::from_millis(100));
        (s, id)
    }

    #[test]
    fn settings_that_restart_audio_end_the_take_and_say_so() {
        let (mut s, track) = recording_on(Box::new(RestartingEngine(StubEngine::default())));
        let config = AudioConfig {
            buffer_size: 512,
            ..AudioConfig::default()
        };
        apply_settings(&mut s, &config).unwrap();
        assert!(!s.is_recording());
        assert_eq!(s.doc.project().track(track).unwrap().clips.len(), 1);
        assert_eq!(
            s.status,
            Some(Message::Record(RecordProblem::AudioRestarted).fields())
        );
        assert_eq!(s.engine.audio_config(), config);
    }

    #[test]
    fn settings_that_restart_nothing_leave_the_take_running() {
        let (mut s, _) = recording_on(Box::new(StubEngine::default()));
        let config = AudioConfig {
            buffer_size: 512,
            ..AudioConfig::default()
        };
        apply_settings(&mut s, &config).unwrap();
        assert!(s.is_recording());
        assert_eq!(s.status, None);
        s.stop_recording();
    }

    #[test]
    fn refused_settings_leave_the_take_running() {
        let (mut s, _) = recording_on(Box::new(RestartingEngine(StubEngine::default())));
        let config = AudioConfig {
            buffer_size: 100,
            ..AudioConfig::default()
        };
        assert_eq!(
            apply_settings(&mut s, &config),
            Err(EngineError::UnsupportedBufferSize(100))
        );
        assert!(s.is_recording());
        s.stop_recording();
    }

    #[test]
    fn problems_become_kind_number_and_detail() {
        let cases = [
            (
                Problem::Engine(EngineError::UnknownDevice("usb:1".into())),
                (ErrorKind::UnknownDevice, 0, "usb:1"),
            ),
            (
                Problem::Engine(EngineError::ServerLost),
                (ErrorKind::ServerLost, 0, ""),
            ),
            (
                Problem::Engine(EngineError::Crashed),
                (ErrorKind::EngineCrashed, 0, ""),
            ),
            (
                Problem::Engine(EngineError::Unstable),
                (ErrorKind::AudioUnstable, 0, ""),
            ),
            (
                Problem::Engine(EngineError::UnsupportedSampleRate(12_345)),
                (ErrorKind::UnsupportedSampleRate, 12_345, ""),
            ),
            (
                Problem::Engine(EngineError::UnsupportedBufferSize(100)),
                (ErrorKind::UnsupportedBufferSize, 100, ""),
            ),
            (
                Problem::Engine(EngineError::UnsupportedSampleRate(u32::MAX)),
                (ErrorKind::UnsupportedSampleRate, i32::MAX, ""),
            ),
            (
                Problem::Engine(EngineError::Backend("server gone".into())),
                (ErrorKind::BackendError, 0, "server gone"),
            ),
            (
                Problem::InvalidDeviceChoice(7),
                (ErrorKind::InvalidDeviceChoice, 7, ""),
            ),
            (
                Problem::InvalidSampleRate(-1),
                (ErrorKind::InvalidSampleRate, -1, ""),
            ),
            (
                Problem::InvalidBufferSize(-2),
                (ErrorKind::InvalidBufferSize, -2, ""),
            ),
        ];
        for (problem, (kind, number, detail)) in cases {
            let (k, n, d) = problem.parts();
            // qenums are not Debug, so compare their values.
            assert_eq!((k.repr, n, d), (kind.repr, number, detail), "{problem:?}");
        }
    }

    /// `start_audio` with the context as its value (qenums are not Debug).
    fn start(engine: &mut dyn EngineHandle, saved: &AudioConfig) -> Option<(i32, EngineError)> {
        start_audio(engine, Some(saved)).map(|(context, e)| (context.repr, e))
    }

    #[test]
    fn startup_with_good_settings_reports_nothing() {
        let mut engine = StubEngine::default();
        let saved = AudioConfig {
            device: Some("stub:usb".into()),
            sample_rate: 96_000,
            buffer_size: 128,
        };
        assert_eq!(start(&mut engine, &saved), None);
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
            start(&mut engine, &saved),
            Some((
                ErrorContext::StartupFallback.repr,
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
        let error = start(&mut engine, &saved);
        assert_eq!(
            error.map(|(c, _)| c),
            Some(ErrorContext::StartupFallback.repr)
        );
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
        let failed = || EngineStatus::Failed(EngineError::Backend("gone".into()));
        let running = EngineStatus::Running {
            sample_rate: 48_000,
            buffer_size: 256,
        };
        let shown = StatusError::Show(EngineError::Backend("gone".into()));
        for context in [
            ErrorContext::NoError,
            ErrorContext::AudioFailed,
            ErrorContext::StartupFallback,
            ErrorContext::StartupFailed,
        ] {
            assert_eq!(status_error(context.repr, failed()), shown);
        }
        assert_eq!(
            status_error(ErrorContext::ApplyingSettings.repr, failed()),
            StatusError::Keep
        );
        for context in [
            ErrorContext::ApplyingSettings,
            ErrorContext::StartupFallback,
            ErrorContext::StartupFailed,
        ] {
            assert_eq!(
                status_error(context.repr, running.clone()),
                StatusError::Keep
            );
        }
        assert_eq!(
            status_error(ErrorContext::AudioFailed.repr, running.clone()),
            StatusError::Clear
        );
        assert_eq!(
            status_error(ErrorContext::NoError.repr, EngineStatus::Stopped),
            StatusError::Keep
        );
    }

    /// Runs only at 44.1 kHz, like a JACK server the app cannot change.
    struct FixedRateEngine(StubEngine);

    impl EngineHandle for FixedRateEngine {
        fn backend_kind(&self) -> BackendKind {
            self.0.backend_kind()
        }
        fn audio_devices(&self) -> Vec<crate::engine::AudioDevice> {
            Vec::new()
        }
        fn supported_sample_rates(&self) -> Vec<u32> {
            vec![44_100]
        }
        fn supported_buffer_sizes(&self) -> Vec<u32> {
            self.0.supported_buffer_sizes()
        }
        fn plan_audio_config(&self, config: &AudioConfig) -> Result<AudioPlan, EngineError> {
            if config.sample_rate != 44_100 {
                return Err(EngineError::UnsupportedSampleRate(config.sample_rate));
            }
            self.0.plan_audio_config(config)
        }
        fn apply_audio_plan(&mut self, plan: AudioPlan) {
            self.0.apply_audio_plan(plan)
        }
        fn audio_config(&self) -> AudioConfig {
            let config = self.0.audio_config();
            AudioConfig {
                sample_rate: 44_100,
                ..config
            }
        }
        fn status(&self) -> EngineStatus {
            self.0.status()
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
        let mut engine = FixedRateEngine(StubEngine::default());
        assert_eq!(
            start(&mut engine, &AudioConfig::default()),
            Some((
                ErrorContext::StartupFallback.repr,
                EngineError::UnsupportedSampleRate(48_000)
            ))
        );
        assert_eq!(engine.audio_config().sample_rate, 44_100);
    }

    /// Refuses everything, like a machine with no working audio.
    struct BrokenEngine(StubEngine);

    impl EngineHandle for BrokenEngine {
        fn backend_kind(&self) -> BackendKind {
            self.0.backend_kind()
        }
        fn audio_devices(&self) -> Vec<crate::engine::AudioDevice> {
            self.0.audio_devices()
        }
        fn supported_sample_rates(&self) -> Vec<u32> {
            self.0.supported_sample_rates()
        }
        fn supported_buffer_sizes(&self) -> Vec<u32> {
            self.0.supported_buffer_sizes()
        }
        fn plan_audio_config(&self, config: &AudioConfig) -> Result<AudioPlan, EngineError> {
            Err(EngineError::Backend(format!(
                "no luck at {}",
                config.sample_rate
            )))
        }
        fn apply_audio_plan(&mut self, _: AudioPlan) {
            unreachable!("nothing is ever planned")
        }
        fn audio_config(&self) -> AudioConfig {
            self.0.audio_config()
        }
        fn status(&self) -> EngineStatus {
            self.0.status()
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
    fn startup_failure_reports_the_last_refusal() {
        let mut engine = BrokenEngine(StubEngine::default());
        let saved = AudioConfig {
            sample_rate: 96_000,
            ..AudioConfig::default()
        };
        assert_eq!(
            start(&mut engine, &saved),
            Some((
                ErrorContext::StartupFailed.repr,
                EngineError::Backend("no luck at 48000".into())
            ))
        );
    }
}
