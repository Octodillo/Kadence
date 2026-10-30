//! `Instruments`: the QML list model of instruments a track can play, over
//! the app's [`InstrumentChoices`](crate::instrument::InstrumentChoices).
//!
//! The instrument menus on the track header and the channel strip show its
//! rows and pass a row to `Project.setTrackInstrument` or
//! `Mixer.setInstrument`. Call `refresh` before showing it: plugin
//! instruments join the list as plugins are scanned.

use std::pin::Pin;

use cxx_qt::CxxQtType;
use cxx_qt_lib::{QByteArray, QHash, QHashPair_i32_QByteArray, QModelIndex, QString, QVariant};

use crate::instrument::InstrumentChoice;
use crate::state;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        // Declared once, in the Mixer's bridge; declaring it here too would
        // define its casts twice.
        include!(<QtCore/QAbstractListModel>);
        type QAbstractListModel = crate::mixer_bridge::qobject::QAbstractListModel;
        include!("cxx-qt-lib/qhash.h");
        type QHash_i32_QByteArray = cxx_qt_lib::QHash<cxx_qt_lib::QHashPair_i32_QByteArray>;
        include!("cxx-qt-lib/qvariant.h");
        type QVariant = cxx_qt_lib::QVariant;
        include!("cxx-qt-lib/qmodelindex.h");
        type QModelIndex = cxx_qt_lib::QModelIndex;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[base = QAbstractListModel]
        #[qml_element]
        #[qml_singleton]
        #[qproperty(i32, count)]
        type Instruments = super::InstrumentsRust;

        /// Re-reads the list if it changed since the last read.
        #[qinvokable]
        fn refresh(self: Pin<&mut Instruments>);
    }

    unsafe extern "RustQt" {
        #[inherit]
        #[cxx_name = "beginResetModel"]
        fn begin_reset_model(self: Pin<&mut Instruments>);
        #[inherit]
        #[cxx_name = "endResetModel"]
        fn end_reset_model(self: Pin<&mut Instruments>);
    }

    extern "RustQt" {
        #[qinvokable]
        #[cxx_override]
        fn data(self: &Instruments, index: &QModelIndex, role: i32) -> QVariant;

        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "roleNames"]
        fn role_names(self: &Instruments) -> QHash_i32_QByteArray;

        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "rowCount"]
        fn row_count(self: &Instruments, parent: &QModelIndex) -> i32;
    }

    impl cxx_qt::Initialize for Instruments {}
}

/// Model roles, after Qt::UserRole: the display name, and whether it is
/// one of ours rather than a plugin.
const ROLES: &[&str] = &["name", "builtin"];
const USER_ROLE: i32 = 0x0100;

#[derive(Default)]
pub struct InstrumentsRust {
    count: i32,
    choices: Vec<InstrumentChoice>,
    /// The list's revision when `choices` was read.
    revision: Option<u64>,
}

impl cxx_qt::Initialize for qobject::Instruments {
    fn initialize(self: Pin<&mut Self>) {
        self.refresh();
    }
}

impl qobject::Instruments {
    fn refresh(mut self: Pin<&mut Self>) {
        let (revision, choices) = state::with(|s| {
            let r = s.instruments.revision();
            let changed = self.revision != Some(r);
            (r, changed.then(|| s.instruments.as_slice().to_vec()))
        });
        let Some(choices) = choices else {
            return;
        };
        let count = i32::try_from(choices.len()).unwrap_or(i32::MAX);
        self.as_mut().begin_reset_model();
        let mut rust = self.as_mut().rust_mut();
        rust.choices = choices;
        rust.revision = Some(revision);
        self.as_mut().end_reset_model();
        self.as_mut().set_count(count);
    }

    fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let Some(c) = usize::try_from(index.row())
            .ok()
            .and_then(|r| self.choices.get(r))
        else {
            return QVariant::default();
        };
        match ROLES.get((role - USER_ROLE) as usize).copied() {
            Some("name") => QVariant::from(&QString::from(c.name.as_str())),
            Some("builtin") => QVariant::from(&c.is_builtin()),
            _ => QVariant::default(),
        }
    }

    fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        let mut roles = QHash::<QHashPair_i32_QByteArray>::default();
        for (i, name) in ROLES.iter().enumerate() {
            roles.insert(USER_ROLE + i as i32, QByteArray::from(*name));
        }
        roles
    }

    fn row_count(&self, _parent: &QModelIndex) -> i32 {
        i32::try_from(self.choices.len()).unwrap_or(i32::MAX)
    }
}
