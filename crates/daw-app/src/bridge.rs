//! `Engine`: the QML-facing wrapper around the audio engine
//! ([`EngineHandle`](crate::engine::EngineHandle)).
//!
//! It shows an [`AudioControl`], which decides what starts and what is
//! wrong, as Qt properties. User-facing wording lives in QML so it goes
//! through KI18n.

use std::pin::Pin;

use cxx_qt::CxxQtType;
use cxx_qt_lib::{QList, QString, QStringList};

use crate::audio::{AudioChoice, AudioControl, AudioView, Notice, NoticeContext, Problem};
use crate::engine::{BackendKind, EngineError};
use crate::kconfig::KConfigStore;
use crate::qt_lists::{list, string_list};
use crate::settings;
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
    /// The settings and the problem on show; the properties copy them.
    control: AudioControl,
}

/// The `errorKind`, `errorNumber` and `errorDetail` QML sees for `problem`.
fn error_parts(problem: &Problem) -> (qobject::ErrorKind, i32, &str) {
    use qobject::ErrorKind as K;
    let number = |n: u32| i32::try_from(n).unwrap_or(i32::MAX);
    let clamped = |n: i64| i32::try_from(n).unwrap_or(if n < 0 { i32::MIN } else { i32::MAX });
    match problem {
        Problem::Engine(EngineError::UnknownDevice(id)) => (K::UnknownDevice, 0, id),
        Problem::Engine(EngineError::UnsupportedSampleRate(rate)) => {
            (K::UnsupportedSampleRate, number(*rate), "")
        }
        Problem::Engine(EngineError::UnsupportedBufferSize(size)) => {
            (K::UnsupportedBufferSize, number(*size), "")
        }
        Problem::Engine(EngineError::ServerLost) => (K::ServerLost, 0, ""),
        Problem::Engine(EngineError::Crashed) => (K::EngineCrashed, 0, ""),
        Problem::Engine(EngineError::Unstable) => (K::AudioUnstable, 0, ""),
        Problem::Engine(EngineError::Backend(message)) => (K::BackendError, 0, message),
        Problem::InvalidDeviceChoice(index) => (K::InvalidDeviceChoice, clamped(*index), ""),
        Problem::InvalidSampleRate(rate) => (K::InvalidSampleRate, clamped(*rate), ""),
        Problem::InvalidBufferSize(size) => (K::InvalidBufferSize, clamped(*size), ""),
    }
}

/// The `errorContext` QML sees for a notice's context.
fn error_context(context: NoticeContext) -> qobject::ErrorContext {
    use qobject::ErrorContext as C;
    match context {
        NoticeContext::ApplyingSettings => C::ApplyingSettings,
        NoticeContext::StartupFallback => C::StartupFallback,
        NoticeContext::StartupFailed => C::StartupFailed,
        NoticeContext::AudioFailed => C::AudioFailed,
    }
}

/// The settings page's combo box values as an [`AudioChoice`], or why
/// they are none.
fn choice_from(device: i32, sample_rate: i32, buffer_size: i32) -> Result<AudioChoice, Problem> {
    Ok(AudioChoice {
        device: usize::try_from(device).map_err(|_| Problem::InvalidDeviceChoice(device.into()))?,
        sample_rate: u32::try_from(sample_rate)
            .map_err(|_| Problem::InvalidSampleRate(sample_rate.into()))?,
        buffer_size: u32::try_from(buffer_size)
            .map_err(|_| Problem::InvalidBufferSize(buffer_size.into()))?,
    })
}

fn to_qlist(values: &[u32]) -> QList<i32> {
    list(values.iter().map(|&v| i32::try_from(v).unwrap_or(i32::MAX)))
}

impl cxx_qt::Initialize for qobject::Engine {
    fn initialize(mut self: Pin<&mut Self>) {
        // Most likely failure: the saved device was unplugged.
        let saved = settings::load_audio_config(&KConfigStore);
        let view = {
            let mut rust = self.as_mut().rust_mut();
            state::with(|s| rust.control.start(s, saved.as_ref()))
        };
        self.show(&view);
    }
}

impl qobject::Engine {
    fn apply_audio_settings(
        mut self: Pin<&mut Self>,
        device_index: i32,
        sample_rate: i32,
        buffer_size: i32,
    ) -> bool {
        let choice = choice_from(device_index, sample_rate, buffer_size);
        let (ok, view) = {
            let mut rust = self.as_mut().rust_mut();
            state::with(|s| match choice {
                Ok(choice) => rust.control.apply(s, &mut KConfigStore, choice),
                Err(problem) => (false, rust.control.refuse(s, problem)),
            })
        };
        self.show(&view);
        ok
    }

    fn poll(mut self: Pin<&mut Self>) {
        let view = {
            let mut rust = self.as_mut().rust_mut();
            state::with(|s| rust.control.poll(s))
        };
        if let Some(view) = view {
            self.show(&view);
        }
    }

    /// Copies `view` into the properties: the notice first, as it was set
    /// before the lists were read again. Setters skip a value that did not
    /// change, so QML only rebuilds what did.
    fn show(mut self: Pin<&mut Self>, view: &AudioView) {
        self.as_mut().set_notice(view.notice.as_ref());
        let backend = match view.backend {
            BackendKind::Jack => qobject::Backend::JackBackend,
            BackendKind::Silent => qobject::Backend::SilentBackend,
            BackendKind::Stub => qobject::Backend::StubBackend,
        };
        // QML replaces the first entry, and empty device names, with
        // translated labels.
        let names =
            string_list(std::iter::once("").chain(view.devices.iter().map(|d| d.name.as_str())));
        self.as_mut().set_device_names(names);
        self.as_mut().set_backend_kind(backend.repr);
        self.as_mut().set_sample_rates(to_qlist(&view.sample_rates));
        self.as_mut().set_buffer_sizes(to_qlist(&view.buffer_sizes));
        self.as_mut().set_running(view.running);
        self.as_mut()
            .set_device_index(i32::try_from(view.device_index).unwrap_or(0));
        self.as_mut()
            .set_sample_rate(i32::try_from(view.sample_rate).unwrap_or(0));
        self.as_mut()
            .set_buffer_size(i32::try_from(view.buffer_size).unwrap_or(0));
    }

    /// Shows `notice` (or nothing) through the error properties. The
    /// context goes last so that QML reacting to its change sees the
    /// matching kind and arguments.
    fn set_notice(mut self: Pin<&mut Self>, notice: Option<&Notice>) {
        let (context, (kind, number, detail)) =
            notice.map_or((qobject::ErrorContext::NoError.repr, (0, 0, "")), |n| {
                let (kind, number, detail) = error_parts(&n.problem);
                (error_context(n.context).repr, (kind.repr, number, detail))
            });
        self.as_mut().set_error_kind(kind);
        self.as_mut().set_error_number(number);
        self.as_mut().set_error_detail(QString::from(detail));
        self.as_mut().set_error_context(context);
    }
}

#[cfg(test)]
mod tests {
    use super::qobject::ErrorKind;
    use super::*;

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
            let (k, n, d) = error_parts(&problem);
            // qenums are not Debug, so compare their values.
            assert_eq!((k.repr, n, d), (kind.repr, number, detail), "{problem:?}");
        }
    }

    #[test]
    fn negative_combo_values_are_no_choice() {
        assert_eq!(
            choice_from(2, 48_000, 256),
            Ok(AudioChoice {
                device: 2,
                sample_rate: 48_000,
                buffer_size: 256,
            })
        );
        assert_eq!(
            choice_from(-1, 48_000, 256),
            Err(Problem::InvalidDeviceChoice(-1))
        );
        assert_eq!(choice_from(0, -1, 256), Err(Problem::InvalidSampleRate(-1)));
        assert_eq!(
            choice_from(0, 48_000, -2),
            Err(Problem::InvalidBufferSize(-2))
        );
    }
}
