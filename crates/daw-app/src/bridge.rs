//! `Engine`: the QML-facing wrapper around an [`EngineHandle`].
//!
//! This is the only place engine state is turned into Qt types. User-facing
//! wording lives in QML so it goes through KI18n.

use std::pin::Pin;

use cxx_qt::CxxQtType;
use cxx_qt_lib::{QList, QString, QStringList};

use crate::engine::{AudioConfig, EngineStatus};
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

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        #[qproperty(QString, backend_name)]
        #[qproperty(bool, running)]
        #[qproperty(QString, error_text)]
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

        /// Apply and save new audio settings. Returns "" on success, or the
        /// backend's error message; the old settings stay live on error.
        #[qinvokable]
        fn apply_audio_settings(
            self: Pin<&mut Engine>,
            device_index: i32,
            sample_rate: i32,
            buffer_size: i32,
        ) -> QString;
    }

    impl cxx_qt::Initialize for Engine {}
}

#[derive(Default)]
pub struct EngineRust {
    backend_name: QString,
    running: bool,
    error_text: QString,
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

        let saved = settings::load_audio_config(&KConfigStore);
        let error = state::with(|s| {
            let mut error = None;
            if let Err(e) = s.engine.apply_audio_config(&saved) {
                // Most likely the saved device was unplugged: keep the saved
                // rate and buffer on the default device, then plain
                // defaults. Say why.
                let mut message = e.to_string();
                let fallbacks = [
                    AudioConfig {
                        device: None,
                        ..saved
                    },
                    AudioConfig::default(),
                ];
                for fallback in &fallbacks {
                    match s.engine.apply_audio_config(fallback) {
                        Ok(()) => break,
                        Err(e) => message = e.to_string(),
                    }
                }
                error = Some(message);
            }
            // The engine may have restarted with a new session.
            s.resync();
            error
        });
        if let Some(e) = error {
            self.as_mut().set_error_text(QString::from(e.as_str()));
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
    ) -> QString {
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
            .map_err(|e| e.to_string())?;
            settings::save_audio_config(&mut KConfigStore, &config);
            Ok(())
        });
        let message = QString::from(result.err().unwrap_or_default().as_str());
        self.as_mut().set_error_text(message.clone());
        self.sync();
        message
    }

    /// Turn the settings page's combo box values into an [`AudioConfig`].
    fn audio_config_from(
        &self,
        device_index: i32,
        sample_rate: i32,
        buffer_size: i32,
    ) -> Result<AudioConfig, String> {
        // Index 0 is the system default; 1.. are `device_ids`.
        let device = match usize::try_from(device_index) {
            Ok(0) => None,
            Ok(i) => Some(
                self.device_ids
                    .get(i - 1)
                    .cloned()
                    .ok_or_else(|| format!("no audio device at index {device_index}"))?,
            ),
            Err(_) => return Err(format!("no audio device at index {device_index}")),
        };
        let sample_rate =
            u32::try_from(sample_rate).map_err(|_| format!("invalid sample rate {sample_rate}"))?;
        let buffer_size =
            u32::try_from(buffer_size).map_err(|_| format!("invalid buffer size {buffer_size}"))?;
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
        if let EngineStatus::Failed(message) = status {
            self.as_mut()
                .set_error_text(QString::from(message.as_str()));
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
