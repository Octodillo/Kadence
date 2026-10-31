//! `Mixer`: the QML list model of channel strips, over a [`MixerHandle`].
//!
//! Each row is a strip. `tick` advances the meters and signals only the
//! meter roles, so a strip's controls are left alone while its meters move.
//! Meter roles are positions from 0 to 1 (see [`MeterDisplay`]); gain
//! reduction is in dB, -1 when the strip has no dynamics insert.

use std::pin::Pin;

use cxx_qt::CxxQtType;
use cxx_qt_lib::{
    QByteArray, QHash, QHashPair_i32_QByteArray, QList, QModelIndex, QString, QStringList,
    QVariant, QVector,
};

use crate::mixer::{
    Control, MeterBallistics, MeterDisplay, MixerHandle, StripId, StripInfo, StripKind,
    default_mixer,
};

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
        type QList_bool = cxx_qt_lib::QList<bool>;
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
        /// The meter roles of every row, flattened: left, right, left hold,
        /// right hold, gain reduction. For tests and overviews.
        #[qproperty(QList_f64, levels)]
        /// Names of the effects `add_insert` can add, in catalog order.
        #[qproperty(QStringList, effect_names)]
        #[qproperty(bool, any_soloed)]
        /// Touch writing: controls moved during playback are recorded as
        /// automation.
        #[qproperty(bool, write_automation, READ, NOTIFY)]
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
        /// Adds a send to the bus at `choice` in the row's `sendChoices`.
        #[qinvokable]
        fn add_send(self: Pin<&mut Mixer>, row: i32, choice: i32) -> bool;
        #[qinvokable]
        fn remove_send(self: Pin<&mut Mixer>, row: i32, send: i32);
        #[qinvokable]
        fn set_send_pre_fader(self: Pin<&mut Mixer>, row: i32, send: i32, pre_fader: bool);
        /// Routes the row's output to `choice` in its `outputChoices`.
        #[qinvokable]
        fn set_output(self: Pin<&mut Mixer>, row: i32, choice: i32);
        /// Appends the effect at `effect` in `effect_names` to the strip.
        #[qinvokable]
        fn add_insert(self: Pin<&mut Mixer>, row: i32, effect: i32) -> bool;
        /// Appends plugin `plugin` (a plugin id) of `format` ("clap",
        /// "lv2" or "vst3") to the strip, as a device called `name`.
        #[qinvokable]
        fn add_plugin_insert(
            self: Pin<&mut Mixer>,
            row: i32,
            format: &QString,
            plugin: &QString,
            name: &QString,
        ) -> bool;
        #[qinvokable]
        fn remove_insert(self: Pin<&mut Mixer>, row: i32, insert: i32);
        /// The id of an insert's device, for `DevicePanel.open`; empty if
        /// there is none.
        #[qinvokable]
        fn insert_device(self: &Mixer, row: i32, insert: i32) -> QString;
        /// The id of the strip's instrument device; empty if it has none.
        #[qinvokable]
        fn instrument_device(self: &Mixer, row: i32) -> QString;
        /// Swaps the strip's instrument for row `choice` of `Instruments`
        /// at its `revision`.
        #[qinvokable]
        fn set_instrument(self: Pin<&mut Mixer>, row: i32, choice: i32, revision: i32);
        /// Turns touch writing on or off.
        #[qinvokable]
        fn set_writing(self: Pin<&mut Mixer>, on: bool);
        /// The user grabbed (`pressed`) or let go of a control: `control`
        /// is "volume", "pan" or "send" (with `send` its index).
        #[qinvokable]
        fn touch(self: Pin<&mut Mixer>, row: i32, control: &QString, send: i32, pressed: bool);
        /// Double-click: puts a control (as for `touch`) back to its
        /// default, taking back the first click's edit.
        #[qinvokable]
        fn reset(self: Pin<&mut Mixer>, row: i32, control: &QString, send: i32);
        /// Picks up edits made elsewhere, records held controls and
        /// refreshes `levels`; `seconds` since the last call.
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
        #[cxx_name = "beginResetModel"]
        fn begin_reset_model(self: Pin<&mut Mixer>);
        #[inherit]
        #[cxx_name = "endResetModel"]
        fn end_reset_model(self: Pin<&mut Mixer>);

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
    "hasInstrument",
    "instrument",
    "instrumentChoice",
    "inserts",
    "insertsEnabled",
    "insertMeters",
    "sends",
    "sendLevels",
    "sendPreFader",
    "sendChoices",
    "outputChoices",
    "outputIndex",
    "levelLeft",
    "levelRight",
    "holdLeft",
    "holdRight",
    "gainReduction",
];
/// The roles `tick` changes.
const METER_ROLES: &[&str] = &[
    "levelLeft",
    "levelRight",
    "holdLeft",
    "holdRight",
    "gainReduction",
];
const USER_ROLE: i32 = 0x0100;

pub struct MixerRust {
    levels: QList<f64>,
    effect_names: QStringList,
    any_soloed: bool,
    write_automation: bool,
    /// The mixer's revision when `strips` was read.
    revision: u64,
    strips: Vec<StripInfo>,
    meters: Vec<MeterDisplay>,
    ballistics: MeterBallistics,
    effect_ids: Vec<String>,
    mixer: Box<dyn MixerHandle>,
}

impl Default for MixerRust {
    fn default() -> Self {
        Self {
            levels: QList::default(),
            effect_names: QStringList::default(),
            any_soloed: false,
            write_automation: false,
            revision: 0,
            strips: Vec::new(),
            meters: Vec::new(),
            ballistics: MeterBallistics::default(),
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

fn flatten(meters: &[MeterDisplay]) -> QList<f64> {
    let mut list = QList::default();
    for m in meters {
        for v in [m.level[0], m.level[1], m.hold[0], m.hold[1]] {
            list.append(v);
        }
        list.append(m.gain_reduction_db.unwrap_or(-1.0));
    }
    list
}

fn role(name: &str) -> i32 {
    let i = ROLES.iter().position(|r| *r == name).expect("known role");
    USER_ROLE + i as i32
}

fn roles(names: &[&str]) -> QVector<i32> {
    let mut v = QVector::default();
    for n in names {
        v.append(role(n));
    }
    v
}

impl cxx_qt::Initialize for qobject::Mixer {
    fn initialize(mut self: Pin<&mut Self>) {
        let effects = self.mixer.effects();
        let names = string_list(effects.iter().map(|e| e.name.as_str()));
        self.as_mut().rust_mut().effect_ids = effects.into_iter().map(|e| e.id).collect();
        self.as_mut().set_effect_names(names);
        let strips = self.mixer.strips();
        let soloed = strips.iter().any(|s| s.soloed);
        let meters = vec![MeterDisplay::default(); strips.len()];
        self.as_mut().set_levels(flatten(&meters));
        let revision = self.mixer.revision();
        let mut rust = self.as_mut().rust_mut();
        rust.strips = strips;
        rust.meters = meters;
        rust.revision = revision;
        self.as_mut().set_any_soloed(soloed);
    }
}

impl qobject::Mixer {
    fn strip_id(&self, row: i32) -> Option<StripId> {
        let row = usize::try_from(row).ok()?;
        self.strips.get(row).map(|s| s.id)
    }

    fn set_volume(mut self: Pin<&mut Self>, row: i32, db: f64) {
        if let Some(id) = self.strip_id(row) {
            self.as_mut().rust_mut().mixer.set_volume(id, db);
            self.reload();
        }
    }

    fn set_pan(mut self: Pin<&mut Self>, row: i32, pan: f64) {
        if let Some(id) = self.strip_id(row) {
            self.as_mut().rust_mut().mixer.set_pan(id, pan);
            self.reload();
        }
    }

    fn set_muted(mut self: Pin<&mut Self>, row: i32, muted: bool) {
        if let Some(id) = self.strip_id(row) {
            self.as_mut().rust_mut().mixer.set_muted(id, muted);
            self.reload();
        }
    }

    fn set_soloed(mut self: Pin<&mut Self>, row: i32, soloed: bool) {
        if let Some(id) = self.strip_id(row) {
            self.as_mut().rust_mut().mixer.set_soloed(id, soloed);
            self.reload();
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
            self.as_mut()
                .rust_mut()
                .mixer
                .set_send_level(id, target, db);
            self.reload();
        }
    }

    /// The strip at `row` and the target of its send at `send`.
    fn send_target(&self, row: i32, send: i32) -> Option<(StripId, StripId)> {
        let strip = self.strips.get(usize::try_from(row).ok()?)?;
        let target = strip.sends.get(usize::try_from(send).ok()?)?.target;
        Some((strip.id, target))
    }

    fn add_send(mut self: Pin<&mut Self>, row: i32, choice: i32) -> bool {
        let target = usize::try_from(row)
            .ok()
            .and_then(|r| self.strips.get(r))
            .and_then(|s| Some((s.id, s.send_choices.get(usize::try_from(choice).ok()?)?.id)));
        let Some((id, target)) = target else {
            return false;
        };
        let added = self.as_mut().rust_mut().mixer.add_send(id, target);
        self.reload();
        added
    }

    fn remove_send(mut self: Pin<&mut Self>, row: i32, send: i32) {
        if let Some((id, target)) = self.send_target(row, send) {
            self.as_mut().rust_mut().mixer.remove_send(id, target);
            self.reload();
        }
    }

    fn set_send_pre_fader(mut self: Pin<&mut Self>, row: i32, send: i32, pre_fader: bool) {
        if let Some((id, target)) = self.send_target(row, send) {
            self.as_mut()
                .rust_mut()
                .mixer
                .set_send_pre_fader(id, target, pre_fader);
            self.reload();
        }
    }

    fn set_output(mut self: Pin<&mut Self>, row: i32, choice: i32) {
        let target = usize::try_from(row)
            .ok()
            .and_then(|r| self.strips.get(r))
            .and_then(|s| {
                Some((
                    s.id,
                    s.output_choices.get(usize::try_from(choice).ok()?)?.id,
                ))
            });
        if let Some((id, target)) = target {
            self.as_mut().rust_mut().mixer.set_output(id, target);
            self.reload();
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
            self.reload();
        }
        added
    }

    fn add_plugin_insert(
        mut self: Pin<&mut Self>,
        row: i32,
        format: &QString,
        plugin: &QString,
        name: &QString,
    ) -> bool {
        let format = daw_plugins::kind_from_name(&format.to_string());
        let (Some(id), Some(format)) = (self.strip_id(row), format) else {
            return false;
        };
        let added = self.as_mut().rust_mut().mixer.add_plugin_insert(
            id,
            format,
            &plugin.to_string(),
            &name.to_string(),
        );
        if added {
            self.reload();
        }
        added
    }

    fn remove_insert(mut self: Pin<&mut Self>, row: i32, insert: i32) {
        if let (Some(id), Ok(insert)) = (self.strip_id(row), usize::try_from(insert)) {
            self.as_mut().rust_mut().mixer.remove_insert(id, insert);
            self.reload();
        }
    }

    fn insert_device(&self, row: i32, insert: i32) -> QString {
        let uid = usize::try_from(row)
            .ok()
            .zip(usize::try_from(insert).ok())
            .and_then(|(r, i)| self.strips.get(r)?.inserts.get(i))
            .map_or("", |i| i.uid.as_str());
        QString::from(uid)
    }

    fn instrument_device(&self, row: i32) -> QString {
        let uid = usize::try_from(row)
            .ok()
            .and_then(|r| self.strips.get(r)?.instrument.as_ref())
            .map_or("", |i| i.uid.as_str());
        QString::from(uid)
    }

    fn set_instrument(mut self: Pin<&mut Self>, row: i32, choice: i32, revision: i32) {
        let choice = crate::instruments_bridge::choice_at(choice, revision);
        if let (Some(id), Some(choice)) = (self.strip_id(row), choice) {
            self.as_mut().rust_mut().mixer.set_instrument(id, &choice);
            self.reload();
        }
    }

    fn set_writing(mut self: Pin<&mut Self>, on: bool) {
        self.as_mut().rust_mut().mixer.set_write_automation(on);
        let on = self.mixer.write_automation();
        if on != self.write_automation {
            self.as_mut().rust_mut().write_automation = on;
            self.as_mut().write_automation_changed();
        }
        self.reload();
    }

    /// The strip and control QML names: "volume", "pan" or "send" with
    /// `send` its index.
    fn control(&self, row: i32, control: &QString, send: i32) -> Option<(StripId, Control)> {
        let id = self.strip_id(row)?;
        let control = match control.to_string().as_str() {
            "volume" => Control::Volume,
            "pan" => Control::Pan,
            "send" => {
                let target = usize::try_from(send)
                    .ok()
                    .and_then(|i| self.strips.get(row as usize)?.sends.get(i))?
                    .target;
                Control::Send(target)
            }
            _ => return None,
        };
        Some((id, control))
    }

    fn touch(mut self: Pin<&mut Self>, row: i32, control: &QString, send: i32, pressed: bool) {
        if let Some((id, control)) = self.control(row, control, send) {
            self.as_mut().rust_mut().mixer.touch(id, control, pressed);
            self.reload();
        }
    }

    fn reset(mut self: Pin<&mut Self>, row: i32, control: &QString, send: i32) {
        if let Some((id, control)) = self.control(row, control, send) {
            self.as_mut().rust_mut().mixer.reset(id, control);
            self.reload();
        }
    }

    /// Re-reads the strips, which may also have changed elsewhere (another
    /// view, undo, a new project): resets the model if strips came or went,
    /// otherwise signals the rows that changed.
    fn reload(mut self: Pin<&mut Self>) {
        let strips = self.mixer.strips();
        let revision = self.mixer.revision();
        self.as_mut().rust_mut().revision = revision;
        if strips == self.strips {
            return;
        }
        let soloed = strips.iter().any(|s| s.soloed);
        let same_rows = strips.len() == self.strips.len()
            && strips.iter().zip(&self.strips).all(|(a, b)| a.id == b.id);
        if same_rows {
            let changed: Vec<i32> = (0..strips.len())
                .filter(|&i| strips[i] != self.strips[i])
                .map(|i| i as i32)
                .collect();
            self.as_mut().rust_mut().strips = strips;
            let root = QModelIndex::default();
            for row in changed {
                let index = self.index(row, 0, &root);
                self.as_mut()
                    .data_changed(&index, &index, &QVector::default());
            }
        } else {
            self.as_mut().begin_reset_model();
            let meters = vec![MeterDisplay::default(); strips.len()];
            let flat = flatten(&meters);
            let mut rust = self.as_mut().rust_mut();
            rust.meters = meters;
            rust.ballistics = MeterBallistics::default();
            rust.strips = strips;
            self.as_mut().end_reset_model();
            self.as_mut().set_levels(flat);
        }
        self.as_mut().set_any_soloed(soloed);
    }

    fn tick(mut self: Pin<&mut Self>, playing: bool, seconds: f64) {
        self.as_mut().rust_mut().mixer.tick(playing, seconds);
        if self.mixer.revision() != self.revision {
            self.as_mut().reload();
        }
        let mut rust = self.as_mut().rust_mut();
        let levels = rust.mixer.levels();
        let meters = rust.ballistics.update(&levels, seconds).to_vec();
        if meters == rust.meters {
            return;
        }
        rust.meters = meters;
        let flat = flatten(&self.meters);
        self.as_mut().set_levels(flat);
        let count = self.strips.len() as i32;
        if count > 0 {
            let root = QModelIndex::default();
            let first = self.index(0, 0, &root);
            let last = self.index(count - 1, 0, &root);
            self.as_mut()
                .data_changed(&first, &last, &roles(METER_ROLES));
        }
    }

    fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let row = usize::try_from(index.row()).ok();
        let Some(s) = row.and_then(|r| self.strips.get(r)) else {
            return QVariant::default();
        };
        let m = row
            .and_then(|r| self.meters.get(r))
            .copied()
            .unwrap_or_default();
        match ROLES.get((role - USER_ROLE) as usize).copied() {
            Some("name") => QVariant::from(&QString::from(s.name.as_str())),
            Some("kind") => QVariant::from(&QString::from(kind_name(s.kind))),
            Some("volumeDb") => QVariant::from(&s.volume_db),
            Some("pan") => QVariant::from(&s.pan),
            Some("muted") => QVariant::from(&s.muted),
            Some("soloed") => QVariant::from(&s.soloed),
            Some("silenced") => QVariant::from(&s.silenced),
            Some("hasInstrument") => QVariant::from(&s.has_instrument),
            Some("instrument") => QVariant::from(&QString::from(
                s.instrument.as_ref().map_or("", |i| i.name.as_str()),
            )),
            Some("instrumentChoice") => QVariant::from(
                &s.instrument_choice
                    .and_then(|i| i32::try_from(i).ok())
                    .unwrap_or(-1),
            ),
            Some("inserts") => {
                QVariant::from(&string_list(s.inserts.iter().map(|i| i.name.as_str())))
            }
            Some("insertsEnabled") => {
                let mut on = QList::<bool>::default();
                for i in &s.inserts {
                    on.append(i.enabled);
                }
                QVariant::from(&on)
            }
            Some("insertMeters") => QVariant::from(&s.inserts.iter().any(|i| i.has_gain_reduction)),
            Some("sends") => {
                QVariant::from(&string_list(s.sends.iter().map(|x| x.target_name.as_str())))
            }
            Some("sendLevels") => {
                let mut levels = QList::<f64>::default();
                for x in &s.sends {
                    levels.append(x.level_db);
                }
                QVariant::from(&levels)
            }
            Some("sendPreFader") => {
                let mut pre = QList::<bool>::default();
                for x in &s.sends {
                    pre.append(x.pre_fader);
                }
                QVariant::from(&pre)
            }
            Some("sendChoices") => {
                QVariant::from(&string_list(s.send_choices.iter().map(|c| c.name.as_str())))
            }
            Some("outputChoices") => QVariant::from(&string_list(
                s.output_choices.iter().map(|c| c.name.as_str()),
            )),
            Some("outputIndex") => {
                let i = s
                    .output
                    .as_ref()
                    .and_then(|o| s.output_choices.iter().position(|c| c.id == o.id));
                QVariant::from(&i.map_or(-1, |i| i as i32))
            }
            Some("levelLeft") => QVariant::from(&m.level[0]),
            Some("levelRight") => QVariant::from(&m.level[1]),
            Some("holdLeft") => QVariant::from(&m.hold[0]),
            Some("holdRight") => QVariant::from(&m.hold[1]),
            Some("gainReduction") => QVariant::from(&m.gain_reduction_db.unwrap_or(-1.0)),
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
