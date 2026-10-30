//! `Engine`: the QML-facing wrapper around an [`EngineHandle`].
//!
//! This is the only place engine state is turned into Qt types. User-facing
//! wording lives in QML so it goes through KI18n.

use std::pin::Pin;

use cxx_qt::CxxQtType;
use cxx_qt_lib::{QList, QString, QStringList};

use crate::engine::{AudioConfig, EngineError, EngineHandle, EngineStatus};
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
        /// `errorNumber` is the device combo box index.
        InvalidDeviceChoice,
        /// `errorNumber` is the sample rate given.
        InvalidSampleRate,
        /// `errorNumber` is the buffer size given.
        InvalidBufferSize,
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        #[qproperty(QString, backend_name)]
        #[qproperty(bool, running)]
        /// An `ErrorContext`; `NoError` when there is nothing to show.
        #[qproperty(i32, error_context)]
        /// An `ErrorKind`; meaningless while `error_context` is `NoError`.
        #[qproperty(i32, error_kind)]
        #[qproperty(i32, error_number)]
        #[qproperty(QString, error_detail)]
        #[qproperty(bool, playing)]
        /// Device display names; index 0 is the backend's default device.
        #[qproperty(QStringList, device_names)]
        #[qproperty(i32, device_index)]
        #[qproperty(QList_i32, sample_rates)]
        #[qproperty(i32, sample_rate)]
        #[qproperty(QList_i32, buffer_sizes)]
        #[qproperty(i32, buffer_size)]
        type Engine = super::EngineRust;

        #[qinvokable]
        fn toggle_playback(self: Pin<&mut Engine>);

        #[qinvokable]
        fn stop(self: Pin<&mut Engine>);

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
    }

    impl cxx_qt::Initialize for Engine {}
}

#[derive(Default)]
pub struct EngineRust {
    backend_name: QString,
    running: bool,
    error_context: i32,
    error_kind: i32,
    error_number: i32,
    error_detail: QString,
    playing: bool,
    device_names: QStringList,
    device_index: i32,
    sample_rates: QList<i32>,
    sample_rate: i32,
    buffer_sizes: QList<i32>,
    buffer_size: i32,
    /// Device ids, parallel to `device_names` (minus the default entry).
    device_ids: Vec<String>,
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
            Self::Engine(EngineError::Backend(message)) => (K::BackendError, 0, message),
            Self::InvalidDeviceChoice(index) => (K::InvalidDeviceChoice, *index, ""),
            Self::InvalidSampleRate(rate) => (K::InvalidSampleRate, *rate, ""),
            Self::InvalidBufferSize(size) => (K::InvalidBufferSize, *size, ""),
        }
    }
}

/// Startup: apply `saved`, else fall back to the saved rate and buffer on
/// the default device, then to plain defaults. Returns what to tell the
/// user, if anything: the first refusal if a fallback worked (that is why
/// the saved settings are not in use), else the last one.
fn start_audio(
    engine: &mut dyn EngineHandle,
    saved: &AudioConfig,
) -> Option<(qobject::ErrorContext, EngineError)> {
    let first = engine.apply_audio_config(saved).err()?;
    let fallbacks = [
        AudioConfig {
            device: None,
            ..saved.clone()
        },
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

/// What `sync` does to the error properties for an engine status.
#[derive(Debug, PartialEq, Eq)]
enum StatusError {
    Keep,
    Clear,
    /// Show this as `AudioFailed`.
    Show(EngineError),
}

/// `current` is the `ErrorContext` on show. A startup or settings error
/// already showing wins over the status, since it says more about why
/// audio is down (and `Failed` also covers "never started"); an
/// `AudioFailed` goes away once audio runs again.
fn status_error(current: i32, status: EngineStatus) -> StatusError {
    use qobject::ErrorContext as C;
    match status {
        EngineStatus::Failed(e) if current == C::NoError.repr || current == C::AudioFailed.repr => {
            StatusError::Show(e)
        }
        EngineStatus::Failed(_) => StatusError::Keep,
        _ if current == C::AudioFailed.repr => StatusError::Clear,
        _ => StatusError::Keep,
    }
}

fn to_qlist(values: &[u32]) -> QList<i32> {
    let mut list = QList::default();
    for &v in values {
        list.append(i32::try_from(v).unwrap_or(i32::MAX));
    }
    list
}

impl cxx_qt::Initialize for qobject::Engine {
    fn initialize(mut self: Pin<&mut Self>) {
        let (devices, backend, rates, sizes) = state::with(|s| {
            let e = &s.engine;
            (
                e.audio_devices(),
                e.backend_name(),
                e.supported_sample_rates(),
                e.supported_buffer_sizes(),
            )
        });
        let mut names = QStringList::default();
        // QML replaces this entry with a translated label.
        names.append(QString::from(""));
        for device in &devices {
            names.append(QString::from(device.name.as_str()));
        }
        self.as_mut().rust_mut().device_ids = devices.into_iter().map(|d| d.id).collect();
        self.as_mut()
            .set_backend_name(QString::from(backend.as_str()));
        self.as_mut().set_device_names(names);
        self.as_mut().set_sample_rates(to_qlist(&rates));
        self.as_mut().set_buffer_sizes(to_qlist(&sizes));

        // Most likely failure: the saved device was unplugged.
        let saved = settings::load_audio_config(&KConfigStore);
        let error = state::with(|s| {
            let error = start_audio(s.engine.as_mut(), &saved);
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
    fn toggle_playback(self: Pin<&mut Self>) {
        state::with(|s| {
            if s.engine.is_playing() {
                s.engine.stop();
            } else {
                s.engine.play();
            }
            s.touch();
        });
        self.sync();
    }

    fn stop(self: Pin<&mut Self>) {
        state::with(|s| {
            s.engine.stop();
            s.touch();
        });
        self.sync();
    }

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
            state::with(|s| {
                let position = s.playhead();
                let r = s.engine.apply_audio_config(&config);
                // A restart brings a fresh session: give it the project and
                // the play position back (after the tempo map, so the
                // position maps to the same bar).
                s.resync();
                if let Some(session) = s.engine.session_mut() {
                    let _ = session.locate(position);
                }
                r
            })
            .map_err(Problem::Engine)?;
            settings::save_audio_config(&mut KConfigStore, &config);
            Ok(())
        });
        let ok = result.is_ok();
        match result {
            Ok(()) => self
                .as_mut()
                .set_error(qobject::ErrorContext::NoError, None),
            Err(problem) => self
                .as_mut()
                .set_error(qobject::ErrorContext::ApplyingSettings, Some(&problem)),
        }
        self.sync();
        ok
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

    /// Copy engine state into the Qt properties.
    fn sync(mut self: Pin<&mut Self>) {
        let (config, status, playing) = state::with(|s| {
            (
                s.engine.audio_config(),
                s.engine.status(),
                s.engine.is_playing(),
            )
        });
        let running = matches!(status, EngineStatus::Running { .. });
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
        self.as_mut().set_playing(playing);
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
    use crate::engine::StubEngine;

    #[test]
    fn problems_become_kind_number_and_detail() {
        let cases = [
            (
                Problem::Engine(EngineError::UnknownDevice("usb:1".into())),
                (ErrorKind::UnknownDevice, 0, "usb:1"),
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
        start_audio(engine, saved).map(|(context, e)| (context.repr, e))
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
    fn status_failure_does_not_hide_a_more_specific_error() {
        let failed = || EngineStatus::Failed(EngineError::Backend("gone".into()));
        let running = EngineStatus::Running {
            sample_rate: 48_000,
            buffer_size: 256,
        };
        let shown = StatusError::Show(EngineError::Backend("gone".into()));
        assert_eq!(status_error(ErrorContext::NoError.repr, failed()), shown);
        assert_eq!(
            status_error(ErrorContext::AudioFailed.repr, failed()),
            shown
        );
        for context in [
            ErrorContext::ApplyingSettings,
            ErrorContext::StartupFallback,
            ErrorContext::StartupFailed,
        ] {
            assert_eq!(status_error(context.repr, failed()), StatusError::Keep);
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

    /// Refuses everything, like a machine with no working audio.
    struct BrokenEngine(StubEngine);

    impl EngineHandle for BrokenEngine {
        fn backend_name(&self) -> String {
            self.0.backend_name()
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
        fn apply_audio_config(&mut self, config: &AudioConfig) -> Result<(), EngineError> {
            Err(EngineError::Backend(format!(
                "no luck at {}",
                config.sample_rate
            )))
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
