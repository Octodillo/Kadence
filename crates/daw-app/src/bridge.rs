//! `Engine`: the QML-facing wrapper around an [`EngineHandle`].
//!
//! This is the only place engine state is turned into Qt types. User-facing
//! wording lives in QML so it goes through KI18n.

use std::pin::Pin;

use cxx_qt::CxxQtType;
use cxx_qt_lib::{QList, QString, QStringList};

use crate::engine::{AudioConfig, EngineHandle, EngineStatus, default_engine};
use crate::settings::{self, KConfigStore};

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
    engine: Box<dyn EngineHandle>,
}

impl Default for EngineRust {
    fn default() -> Self {
        Self {
            backend_name: QString::default(),
            running: false,
            error_text: QString::default(),
            playing: false,
            device_names: QStringList::default(),
            device_index: 0,
            sample_rates: QList::default(),
            sample_rate: 0,
            buffer_sizes: QList::default(),
            buffer_size: 0,
            device_ids: Vec::new(),
            engine: default_engine(),
        }
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
        let engine = &self.engine;
        let devices = engine.audio_devices();
        let mut names = QStringList::default();
        // QML replaces this entry with a translated label.
        names.append(QString::from(""));
        for device in &devices {
            names.append(QString::from(device.name.as_str()));
        }
        let backend = QString::from(engine.backend_name().as_str());
        let rates = to_qlist(&engine.supported_sample_rates());
        let sizes = to_qlist(&engine.supported_buffer_sizes());
        self.as_mut().rust_mut().device_ids = devices.into_iter().map(|d| d.id).collect();
        self.as_mut().set_backend_name(backend);
        self.as_mut().set_device_names(names);
        self.as_mut().set_sample_rates(rates);
        self.as_mut().set_buffer_sizes(sizes);

        let saved = settings::load_audio_config(&KConfigStore);
        if self
            .as_mut()
            .rust_mut()
            .engine
            .apply_audio_config(&saved)
            .is_err()
        {
            // A saved device may have been unplugged; fall back to defaults.
            let fallback = AudioConfig::default();
            if let Err(e) = self
                .as_mut()
                .rust_mut()
                .engine
                .apply_audio_config(&fallback)
            {
                self.as_mut()
                    .set_error_text(QString::from(e.to_string().as_str()));
            }
        }
        self.sync();
    }
}

impl qobject::Engine {
    fn toggle_playback(mut self: Pin<&mut Self>) {
        let engine = &mut self.as_mut().rust_mut().engine;
        if engine.is_playing() {
            engine.stop();
        } else {
            engine.play();
        }
        self.sync();
    }

    fn stop(mut self: Pin<&mut Self>) {
        self.as_mut().rust_mut().engine.stop();
        self.sync();
    }

    fn apply_audio_settings(
        mut self: Pin<&mut Self>,
        device_index: i32,
        sample_rate: i32,
        buffer_size: i32,
    ) -> QString {
        let device = usize::try_from(device_index)
            .ok()
            .and_then(|i| i.checked_sub(1))
            .and_then(|i| self.device_ids.get(i).cloned());
        let (Ok(sample_rate), Ok(buffer_size)) =
            (u32::try_from(sample_rate), u32::try_from(buffer_size))
        else {
            return QString::from("invalid audio settings");
        };
        let config = AudioConfig {
            device,
            sample_rate,
            buffer_size,
        };
        let result = self.as_mut().rust_mut().engine.apply_audio_config(&config);
        let message = match result {
            Ok(()) => {
                settings::save_audio_config(&mut KConfigStore, &config);
                QString::default()
            }
            Err(e) => QString::from(e.to_string().as_str()),
        };
        self.as_mut().set_error_text(message.clone());
        self.sync();
        message
    }

    /// Copy engine state into the Qt properties.
    fn sync(mut self: Pin<&mut Self>) {
        let engine = &self.engine;
        let config = engine.audio_config();
        let status = engine.status();
        let running = matches!(status, EngineStatus::Running { .. });
        let playing = engine.is_playing();
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
