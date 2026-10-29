//! `Mixer`: the QML list model of channel strips, over a [`MixerHandle`].
//!
//! Each row is a strip. Meter levels change many times a second, so they
//! are not roles: they live in the flat `levels` property instead
//! (`[left, right, gain reduction]` per row, gain reduction -1 when the
//! strip has no dynamics insert), refreshed by `tick`.

use std::pin::Pin;

use cxx_qt::CxxQtType;
use cxx_qt_lib::{
    QByteArray, QHash, QHashPair_i32_QByteArray, QList, QModelIndex, QString, QStringList,
    QVariant, QVector,
};

use crate::mixer::{MixerHandle, StripInfo, StripKind, StripLevels, default_mixer};

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++Qt" {
        include!(<QtCore/QAbstractListModel>);
        #[qobject]
        type QAbstractListModel;
    }

    unsafe extern "C++" {
        include!("cxx-qt-lib/qhash.h");
        type QHash_i32_QByteArray = cxx_qt_lib::QHash<cxx_qt_lib::QHashPair_i32_QByteArray>;
        include!("cxx-qt-lib/qvariant.h");
        type QVariant = cxx_qt_lib::QVariant;
        include!("cxx-qt-lib/qmodelindex.h");
        type QModelIndex = cxx_qt_lib::QModelIndex;
        include!("cxx-qt-lib/qvector.h");
        type QVector_i32 = cxx_qt_lib::QVector<i32>;
        include!("cxx-qt-lib/qlist.h");
        type QList_f64 = cxx_qt_lib::QList<f64>;
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qstringlist.h");
        type QStringList = cxx_qt_lib::QStringList;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[base = QAbstractListModel]
        #[qml_element]
        #[qml_singleton]
        /// Per row: left peak, right peak (linear), gain reduction in dB or -1.
        #[qproperty(QList_f64, levels)]
        /// Names of the effects `add_insert` can add, in catalog order.
        #[qproperty(QStringList, effect_names)]
        #[qproperty(bool, any_soloed)]
        type Mixer = super::MixerRust;

        #[qinvokable]
        fn set_volume(self: Pin<&mut Mixer>, row: i32, db: f64);
        #[qinvokable]
        fn set_pan(self: Pin<&mut Mixer>, row: i32, pan: f64);
        #[qinvokable]
        fn set_muted(self: Pin<&mut Mixer>, row: i32, muted: bool);
        #[qinvokable]
        fn set_soloed(self: Pin<&mut Mixer>, row: i32, soloed: bool);
        #[qinvokable]
        fn send_level(self: &Mixer, row: i32, send: i32) -> f64;
        #[qinvokable]
        fn set_send_level(self: Pin<&mut Mixer>, row: i32, send: i32, db: f64);
        /// Appends the effect at `effect` in `effect_names` to the strip.
        #[qinvokable]
        fn add_insert(self: Pin<&mut Mixer>, row: i32, effect: i32) -> bool;
        #[qinvokable]
        fn remove_insert(self: Pin<&mut Mixer>, row: i32, insert: i32);
        /// Advances the preview by `seconds` and refreshes `levels`.
        #[qinvokable]
        fn tick(self: Pin<&mut Mixer>, playing: bool, seconds: f64);
    }

    unsafe extern "RustQt" {
        #[inherit]
        #[qsignal]
        #[cxx_name = "dataChanged"]
        fn data_changed(
            self: Pin<&mut Mixer>,
            top_left: &QModelIndex,
            bottom_right: &QModelIndex,
            roles: &QVector_i32,
        );

        #[inherit]
        fn index(self: &Mixer, row: i32, column: i32, parent: &QModelIndex) -> QModelIndex;
    }

    extern "RustQt" {
        #[qinvokable]
        #[cxx_override]
        fn data(self: &Mixer, index: &QModelIndex, role: i32) -> QVariant;

        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "roleNames"]
        fn role_names(self: &Mixer) -> QHash_i32_QByteArray;

        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "rowCount"]
        fn row_count(self: &Mixer, parent: &QModelIndex) -> i32;
    }

    impl cxx_qt::Initialize for Mixer {}
}

/// Model roles, after Qt::UserRole.
const ROLES: &[&str] = &[
    "name",
    "kind",
    "volumeDb",
    "pan",
    "muted",
    "soloed",
    "silenced",
    "inserts",
    "insertMeters",
    "sends",
];
const USER_ROLE: i32 = 0x0100;

pub struct MixerRust {
    levels: QList<f64>,
    effect_names: QStringList,
    any_soloed: bool,
    strips: Vec<StripInfo>,
    effect_ids: Vec<String>,
    mixer: Box<dyn MixerHandle>,
}

impl Default for MixerRust {
    fn default() -> Self {
        Self {
            levels: QList::default(),
            effect_names: QStringList::default(),
            any_soloed: false,
            strips: Vec::new(),
            effect_ids: Vec::new(),
            mixer: default_mixer(),
        }
    }
}

fn string_list<'a>(items: impl IntoIterator<Item = &'a str>) -> QStringList {
    let mut list = QStringList::default();
    for s in items {
        list.append(QString::from(s));
    }
    list
}

fn kind_name(kind: StripKind) -> &'static str {
    match kind {
        StripKind::Track => "track",
        StripKind::Bus => "bus",
        StripKind::Master => "master",
    }
}

fn flatten(levels: &[StripLevels]) -> QList<f64> {
    let mut list = QList::default();
    for l in levels {
        list.append(f64::from(l.peak[0]));
        list.append(f64::from(l.peak[1]));
        list.append(l.gain_reduction_db.map_or(-1.0, f64::from));
    }
    list
}

impl cxx_qt::Initialize for qobject::Mixer {
    fn initialize(mut self: Pin<&mut Self>) {
        let effects = self.mixer.effects();
        let names = string_list(effects.iter().map(|e| e.name.as_str()));
        self.as_mut().rust_mut().effect_ids = effects.into_iter().map(|e| e.id).collect();
        self.as_mut().set_effect_names(names);
        let strips = self.mixer.strips();
        let soloed = strips.iter().any(|s| s.soloed);
        let levels = vec![StripLevels::default(); strips.len()];
        self.as_mut().rust_mut().strips = strips;
        self.as_mut().set_any_soloed(soloed);
        self.as_mut().set_levels(flatten(&levels));
    }
}

impl qobject::Mixer {
    fn strip_id(&self, row: i32) -> Option<u64> {
        let row = usize::try_from(row).ok()?;
        self.strips.get(row).map(|s| s.id)
    }

    /// Re-reads every strip and tells views all rows may have changed. Row
    /// count never changes in the preview, so no reset is needed.
    fn refresh(mut self: Pin<&mut Self>) {
        let strips = self.mixer.strips();
        let soloed = strips.iter().any(|s| s.soloed);
        let last = strips.len() as i32 - 1;
        self.as_mut().rust_mut().strips = strips;
        self.as_mut().set_any_soloed(soloed);
        if last >= 0 {
            let root = QModelIndex::default();
            let first = self.index(0, 0, &root);
            let last = self.index(last, 0, &root);
            self.as_mut()
                .data_changed(&first, &last, &QVector::default());
        }
    }

    fn set_volume(mut self: Pin<&mut Self>, row: i32, db: f64) {
        if let Some(id) = self.strip_id(row) {
            self.as_mut().rust_mut().mixer.set_volume(id, db);
            // The fader already shows the value; keep the row in sync quietly.
            if let Some(s) = self.as_mut().rust_mut().strips.get_mut(row as usize) {
                s.volume_db = db;
            }
        }
    }

    fn set_pan(mut self: Pin<&mut Self>, row: i32, pan: f64) {
        if let Some(id) = self.strip_id(row) {
            self.as_mut().rust_mut().mixer.set_pan(id, pan);
            if let Some(s) = self.as_mut().rust_mut().strips.get_mut(row as usize) {
                s.pan = pan;
            }
        }
    }

    fn set_muted(mut self: Pin<&mut Self>, row: i32, muted: bool) {
        if let Some(id) = self.strip_id(row) {
            self.as_mut().rust_mut().mixer.set_muted(id, muted);
            self.refresh();
        }
    }

    fn set_soloed(mut self: Pin<&mut Self>, row: i32, soloed: bool) {
        if let Some(id) = self.strip_id(row) {
            self.as_mut().rust_mut().mixer.set_soloed(id, soloed);
            self.refresh();
        }
    }

    fn send_level(&self, row: i32, send: i32) -> f64 {
        let Ok(row) = usize::try_from(row) else {
            return f64::NAN;
        };
        let send = usize::try_from(send).ok();
        self.strips
            .get(row)
            .and_then(|s| s.sends.get(send?))
            .map_or(f64::NAN, |s| s.level_db)
    }

    fn set_send_level(mut self: Pin<&mut Self>, row: i32, send: i32, db: f64) {
        let target = usize::try_from(row)
            .ok()
            .zip(usize::try_from(send).ok())
            .and_then(|(r, s)| Some((r, self.strips.get(r)?.sends.get(s)?.target)));
        if let Some((r, target)) = target {
            let id = self.strips[r].id;
            let mut rust = self.as_mut().rust_mut();
            rust.mixer.set_send_level(id, target, db);
            rust.strips[r].sends[send as usize].level_db = db;
        }
    }

    fn add_insert(mut self: Pin<&mut Self>, row: i32, effect: i32) -> bool {
        let device = usize::try_from(effect)
            .ok()
            .and_then(|i| self.effect_ids.get(i).cloned());
        let (Some(id), Some(device)) = (self.strip_id(row), device) else {
            return false;
        };
        let added = self.as_mut().rust_mut().mixer.add_insert(id, &device);
        if added {
            self.refresh();
        }
        added
    }

    fn remove_insert(mut self: Pin<&mut Self>, row: i32, insert: i32) {
        if let (Some(id), Ok(insert)) = (self.strip_id(row), usize::try_from(insert)) {
            self.as_mut().rust_mut().mixer.remove_insert(id, insert);
            self.refresh();
        }
    }

    fn tick(mut self: Pin<&mut Self>, playing: bool, seconds: f64) {
        let mut rust = self.as_mut().rust_mut();
        rust.mixer.tick(playing, seconds);
        let levels = rust.mixer.levels();
        self.as_mut().set_levels(flatten(&levels));
    }

    fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let Some(s) = usize::try_from(index.row())
            .ok()
            .and_then(|r| self.strips.get(r))
        else {
            return QVariant::default();
        };
        match ROLES.get((role - USER_ROLE) as usize).copied() {
            Some("name") => QVariant::from(&QString::from(s.name.as_str())),
            Some("kind") => QVariant::from(&QString::from(kind_name(s.kind))),
            Some("volumeDb") => QVariant::from(&s.volume_db),
            Some("pan") => QVariant::from(&s.pan),
            Some("muted") => QVariant::from(&s.muted),
            Some("soloed") => QVariant::from(&s.soloed),
            Some("silenced") => QVariant::from(&s.silenced),
            Some("inserts") => {
                QVariant::from(&string_list(s.inserts.iter().map(|i| i.name.as_str())))
            }
            Some("insertMeters") => QVariant::from(&s.inserts.iter().any(|i| i.has_gain_reduction)),
            Some("sends") => {
                QVariant::from(&string_list(s.sends.iter().map(|x| x.target_name.as_str())))
            }
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
        self.strips.len() as i32
    }
}
