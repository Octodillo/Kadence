//! `AppSettings`: the MIDI and General settings pages, over
//! [`crate::settings`] and [`crate::state`]. Audio settings are on
//! `Engine` (see [`crate::bridge`]).

use std::pin::Pin;
use std::time::Duration;

use cxx_qt::CxxQtType;
use cxx_qt_lib::{QList, QString, QStringList, QUrl};

use crate::qt_lists::{list, string_list};
use crate::settings::{self, GeneralSettings, KConfigStore, MAX_AUTOSAVE_MINUTES};
use crate::state::{self, AppState};

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qstringlist.h");
        type QStringList = cxx_qt_lib::QStringList;
        include!("cxx-qt-lib/qlist.h");
        type QList_bool = cxx_qt_lib::QList<bool>;
        include!("cxx-qt-lib/qurl.h");
        type QUrl = cxx_qt_lib::QUrl;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        /// Every MIDI source the settings page lists, by port name: the
        /// ones there now, then ones in use but unplugged.
        #[qproperty(QStringList, midi_source_names)]
        /// Their names for people (the part after the program's name).
        #[qproperty(QStringList, midi_source_labels)]
        /// Hardware rather than a program.
        #[qproperty(QList_bool, midi_source_physical)]
        /// Given a port of its own.
        #[qproperty(QList_bool, midi_source_used)]
        /// There now.
        #[qproperty(QList_bool, midi_source_present)]
        /// Hardware nobody decided about is used.
        #[qproperty(bool, use_new_midi_devices)]
        /// The sources with ports, which a track can listen to alone.
        #[qproperty(QStringList, midi_input_names)]
        #[qproperty(QStringList, midi_input_labels)]
        /// Where Open and Save As start, as a local path; empty for the
        /// dialogs' own choice.
        #[qproperty(QString, project_folder)]
        /// As a URL, for the file dialogs.
        #[qproperty(QUrl, project_folder_url)]
        /// 0 turns autosave off.
        #[qproperty(i32, autosave_minutes)]
        #[qproperty(i32, max_autosave_minutes)]
        type AppSettings = super::AppSettingsRust;

        /// Uses `name` or stops using it, and remembers the choice.
        #[qinvokable]
        fn use_midi_source(self: Pin<&mut AppSettings>, name: &QString, used: bool);
        #[qinvokable]
        fn choose_use_new_midi_devices(self: Pin<&mut AppSettings>, on: bool);
        /// Called by a timer: notices MIDI devices plugged in or out.
        #[qinvokable]
        fn refresh_midi(self: Pin<&mut AppSettings>);
        /// The label for a source port name, also for one not listed.
        #[qinvokable]
        fn midi_label(self: &AppSettings, name: &QString) -> QString;

        /// Takes a folder URL from a FolderDialog; an empty URL clears it.
        #[qinvokable]
        fn choose_project_folder(self: Pin<&mut AppSettings>, url: &QUrl);
        #[qinvokable]
        fn choose_autosave_minutes(self: Pin<&mut AppSettings>, minutes: i32);
    }

    impl cxx_qt::Initialize for AppSettings {}
}

#[derive(Default)]
pub struct AppSettingsRust {
    midi_source_names: QStringList,
    midi_source_labels: QStringList,
    midi_source_physical: QList<bool>,
    midi_source_used: QList<bool>,
    midi_source_present: QList<bool>,
    use_new_midi_devices: bool,
    midi_input_names: QStringList,
    midi_input_labels: QStringList,
    project_folder: QString,
    project_folder_url: QUrl,
    autosave_minutes: i32,
    max_autosave_minutes: i32,
    general: GeneralSettings,
}

/// What people call a MIDI source: the port's own name, without the
/// program's (`Midi-Bridge:Keys (capture)` is `Keys (capture)`).
pub fn midi_label(name: &str) -> &str {
    name.split_once(':')
        .map(|(_, port)| port.trim())
        .filter(|p| !p.is_empty())
        .unwrap_or(name)
}

/// One row of the MIDI settings list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceRow {
    pub name: String,
    pub physical: bool,
    pub used: bool,
    pub present: bool,
}

/// The rows the settings page shows: sources there now, then sources with
/// ports that are unplugged, so they can be turned off too.
pub fn source_rows(s: &AppState) -> Vec<SourceRow> {
    let inputs = s.engine.midi_inputs();
    let has_port = |n: &str| inputs.iter().any(|(i, _)| i == n);
    let mut rows: Vec<SourceRow> = s
        .engine
        .midi_sources()
        .into_iter()
        .map(|src| SourceRow {
            used: has_port(&src.name),
            physical: src.is_physical,
            present: true,
            name: src.name,
        })
        .collect();
    for (name, _) in &inputs {
        if !rows.iter().any(|r| &r.name == name) {
            rows.push(SourceRow {
                name: name.clone(),
                physical: true,
                used: true,
                present: false,
            });
        }
    }
    rows
}

impl cxx_qt::Initialize for qobject::AppSettings {
    fn initialize(mut self: Pin<&mut Self>) {
        let midi = settings::load_midi_settings(&KConfigStore);
        let general = settings::load_general_settings(&KConfigStore);
        state::with(|s| {
            s.midi = midi;
            s.refresh_midi();
        });
        self.as_mut()
            .set_max_autosave_minutes(MAX_AUTOSAVE_MINUTES as i32);
        self.as_mut().apply_general(general);
        self.sync_midi();
    }
}

impl qobject::AppSettings {
    fn use_midi_source(mut self: Pin<&mut Self>, name: &QString, used: bool) {
        let name = name.to_string();
        state::with(|s| {
            s.midi.set_used(&name, used);
            settings::save_midi_settings(&mut KConfigStore, &s.midi);
            s.refresh_midi();
            s.touch();
        });
        self.as_mut().sync_midi();
    }

    fn choose_use_new_midi_devices(mut self: Pin<&mut Self>, on: bool) {
        state::with(|s| {
            s.midi.use_new_devices = on;
            settings::save_midi_settings(&mut KConfigStore, &s.midi);
            s.refresh_midi();
            s.touch();
        });
        self.as_mut().sync_midi();
    }

    fn refresh_midi(mut self: Pin<&mut Self>) {
        let rows = state::with(|s| {
            if s.refresh_midi() {
                s.touch();
            }
            source_rows(s)
        });
        // The list of sources other clients offer can change without our
        // ports changing (a program started), so compare what is shown.
        if self.midi_source_names != string_list(rows.iter().map(|r| r.name.as_str()))
            || self.midi_source_present != list(rows.iter().map(|r| r.present))
        {
            self.as_mut().sync_midi();
        }
    }

    fn midi_label(&self, name: &QString) -> QString {
        QString::from(midi_label(&name.to_string()))
    }

    /// Copies the MIDI state into the properties.
    fn sync_midi(mut self: Pin<&mut Self>) {
        let (rows, inputs, use_new) = state::with(|s| {
            let inputs: Vec<String> = s.engine.midi_inputs().into_iter().map(|i| i.0).collect();
            (source_rows(s), inputs, s.midi.use_new_devices)
        });
        self.as_mut()
            .set_midi_source_names(string_list(rows.iter().map(|r| r.name.as_str())));
        self.as_mut()
            .set_midi_source_labels(string_list(rows.iter().map(|r| midi_label(&r.name))));
        self.as_mut()
            .set_midi_source_physical(list(rows.iter().map(|r| r.physical)));
        self.as_mut()
            .set_midi_source_used(list(rows.iter().map(|r| r.used)));
        self.as_mut()
            .set_midi_source_present(list(rows.iter().map(|r| r.present)));
        self.as_mut().set_use_new_midi_devices(use_new);
        self.as_mut()
            .set_midi_input_names(string_list(inputs.iter().map(String::as_str)));
        self.as_mut()
            .set_midi_input_labels(string_list(inputs.iter().map(|n| midi_label(n))));
    }

    fn choose_project_folder(mut self: Pin<&mut Self>, url: &QUrl) {
        let folder = (!url.is_empty())
            .then(|| url.to_local_file().map(|p| p.to_string()))
            .flatten()
            .filter(|p| !p.is_empty());
        let general = GeneralSettings {
            project_folder: folder,
            ..self.general.clone()
        };
        settings::save_general_settings(&mut KConfigStore, &general);
        self.as_mut().apply_general(general);
    }

    fn choose_autosave_minutes(mut self: Pin<&mut Self>, minutes: i32) {
        let minutes = u32::try_from(minutes)
            .unwrap_or(0)
            .min(MAX_AUTOSAVE_MINUTES);
        let general = GeneralSettings {
            autosave_minutes: minutes,
            ..self.general.clone()
        };
        settings::save_general_settings(&mut KConfigStore, &general);
        self.as_mut().apply_general(general);
    }

    fn apply_general(mut self: Pin<&mut Self>, general: GeneralSettings) {
        state::with(|s| {
            s.autosave_every = (general.autosave_minutes > 0)
                .then(|| Duration::from_secs(u64::from(general.autosave_minutes) * 60));
        });
        let folder = general.project_folder.clone().unwrap_or_default();
        let url = if folder.is_empty() {
            QUrl::default()
        } else {
            QUrl::from_local_file(&QString::from(folder.as_str()))
        };
        self.as_mut()
            .set_project_folder(QString::from(folder.as_str()));
        self.as_mut().set_project_folder_url(url);
        self.as_mut()
            .set_autosave_minutes(general.autosave_minutes as i32);
        self.as_mut().rust_mut().general = general;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_drop_the_program_name() {
        assert_eq!(midi_label("Midi-Bridge:Keys (capture)"), "Keys (capture)");
        assert_eq!(
            midi_label("a2j:Pads [24] (capture): Pads MIDI 1"),
            "Pads [24] (capture): Pads MIDI 1"
        );
        assert_eq!(midi_label("nameless"), "nameless");
        assert_eq!(midi_label("odd:"), "odd:");
    }
}
