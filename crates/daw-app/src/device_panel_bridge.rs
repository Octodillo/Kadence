//! `DevicePanel`: the QML list model of the open device's parameters,
//! over a [`DevicePanel`](crate::device_panel::DevicePanel).
//!
//! Each row is a parameter. Open a device with `open` and its id (the
//! Mixer gives an insert's with `insertDevice`); the panel's properties
//! describe the device, and `active` turns false when it is closed or
//! goes away (removed, or its addition undone).

use std::pin::Pin;

use cxx_qt::CxxQtType;
use cxx_qt_lib::{
    QByteArray, QHash, QHashPair_i32_QByteArray, QModelIndex, QString, QStringList, QVariant,
    QVector,
};
use daw_devices::ParamUnit;

use crate::device_panel::{AppDevicePanel, DeviceView, ParamControl, ParamView};

#[cxx_qt::bridge]
pub mod qobject {
    // QAbstractListModel is declared as a QObject in the Mixer's bridge;
    // declaring it again would define its generated helpers twice.
    unsafe extern "C++" {
        include!(<QtCore/QAbstractListModel>);
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
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[base = QAbstractListModel]
        #[qml_element]
        #[qml_singleton]
        /// A device is open.
        #[qproperty(bool, active, READ, NOTIFY)]
        #[qproperty(QString, device_id, READ, NOTIFY)]
        #[qproperty(QString, device_name, READ, NOTIFY)]
        /// Built-in device id (such as `daw.eq`) or plugin id.
        #[qproperty(QString, device_kind, READ, NOTIFY)]
        #[qproperty(QString, track_name, READ, NOTIFY)]
        /// The device is on the master track (unnamed by default).
        #[qproperty(bool, on_master, READ, NOTIFY)]
        /// Switched on, not bypassed.
        #[qproperty(bool, switched_on, READ, NOTIFY)]
        /// The device plays an instrument track's notes.
        #[qproperty(bool, instrument, READ, NOTIFY)]
        #[qproperty(bool, can_move_earlier, READ, NOTIFY)]
        #[qproperty(bool, can_move_later, READ, NOTIFY)]
        /// Position in the track's chain, from 0.
        #[qproperty(i32, chain_index, READ, NOTIFY)]
        #[qproperty(i32, chain_length, READ, NOTIFY)]
        /// How many parameters (rows) the device shows.
        #[qproperty(i32, param_count, READ, NOTIFY)]
        type DevicePanel = super::DevicePanelRust;

        /// Opens the device with this id. Returns false if there is none.
        #[qinvokable]
        fn open(self: Pin<&mut DevicePanel>, id: &QString) -> bool;
        #[qinvokable]
        fn close(self: Pin<&mut DevicePanel>);
        /// Sets the parameter in `row` to a plain value. While `dragging`,
        /// edits of the same parameter merge into one undo step;
        /// otherwise this is a step of its own.
        #[qinvokable]
        fn set_value(self: Pin<&mut DevicePanel>, row: i32, value: f64, dragging: bool);
        /// Like `set_value`, from a slider position from 0 to 1 along the
        /// parameter's taper.
        #[qinvokable]
        fn set_position(self: Pin<&mut DevicePanel>, row: i32, position: f64, dragging: bool);
        /// The plain value at a slider position, for showing it while
        /// dragging.
        #[qinvokable]
        fn value_at(self: &DevicePanel, row: i32, position: f64) -> f64;
        /// The user let go of a control.
        #[qinvokable]
        fn end_gesture(self: Pin<&mut DevicePanel>);
        /// Returns the parameter in `row` to its default.
        #[qinvokable]
        fn reset(self: Pin<&mut DevicePanel>, row: i32);
        /// Switches the device on, or off (bypassed).
        #[qinvokable]
        fn set_device_enabled(self: Pin<&mut DevicePanel>, on: bool);
        /// Moves the device one place along its chain (-1 earlier, 1 later).
        #[qinvokable]
        fn move_by(self: Pin<&mut DevicePanel>, step: i32);
        /// Picks up edits made elsewhere (undo, the mixer).
        #[qinvokable]
        fn refresh(self: Pin<&mut DevicePanel>);
    }

    unsafe extern "RustQt" {
        #[inherit]
        #[qsignal]
        #[cxx_name = "dataChanged"]
        fn data_changed(
            self: Pin<&mut DevicePanel>,
            top_left: &QModelIndex,
            bottom_right: &QModelIndex,
            roles: &QVector_i32,
        );

        #[inherit]
        #[cxx_name = "beginResetModel"]
        fn begin_reset_model(self: Pin<&mut DevicePanel>);
        #[inherit]
        #[cxx_name = "endResetModel"]
        fn end_reset_model(self: Pin<&mut DevicePanel>);

        #[inherit]
        fn index(self: &DevicePanel, row: i32, column: i32, parent: &QModelIndex) -> QModelIndex;
    }

    extern "RustQt" {
        #[qinvokable]
        #[cxx_override]
        fn data(self: &DevicePanel, index: &QModelIndex, role: i32) -> QVariant;

        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "roleNames"]
        fn role_names(self: &DevicePanel) -> QHash_i32_QByteArray;

        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "rowCount"]
        fn row_count(self: &DevicePanel, parent: &QModelIndex) -> i32;
    }
}

/// Model roles, after Qt::UserRole.
const ROLES: &[&str] = &[
    "paramId",
    "name",
    "value",
    // Slider position from 0 to 1, along the taper.
    "position",
    "defaultValue",
    "minimum",
    "maximum",
    // "continuous", "integer", "toggle" or "choice".
    "control",
    // See `unit_name`.
    "unit",
    "choices",
    "isDefault",
];
const USER_ROLE: i32 = 0x0100;

#[derive(Default)]
pub struct DevicePanelRust {
    active: bool,
    device_id: QString,
    device_name: QString,
    device_kind: QString,
    track_name: QString,
    on_master: bool,
    switched_on: bool,
    instrument: bool,
    can_move_earlier: bool,
    can_move_later: bool,
    chain_index: i32,
    chain_length: i32,
    param_count: i32,
    /// The panel's revision when `view` was read.
    revision: u64,
    view: Option<DeviceView>,
    panel: AppDevicePanel,
}

/// How QML names a unit; it words the value with it.
fn unit_name(unit: ParamUnit) -> &'static str {
    match unit {
        ParamUnit::None => "",
        ParamUnit::Decibels => "dB",
        ParamUnit::Hertz => "Hz",
        ParamUnit::Milliseconds => "ms",
        ParamUnit::Percent => "percent",
        ParamUnit::Semitones => "semitones",
        ParamUnit::Cents => "cents",
        ParamUnit::Octaves => "octaves",
        ParamUnit::Ratio => "ratio",
        ParamUnit::Pan => "pan",
    }
}

fn control_name(control: &ParamControl) -> &'static str {
    match control {
        ParamControl::Continuous { .. } => "continuous",
        ParamControl::Integer => "integer",
        ParamControl::Toggle => "toggle",
        ParamControl::Choice(_) => "choice",
    }
}

impl qobject::DevicePanel {
    fn params(&self) -> &[ParamView] {
        self.view.as_ref().map_or(&[], |v| &v.params)
    }

    fn param(&self, row: i32) -> Option<&ParamView> {
        self.params().get(usize::try_from(row).ok()?)
    }

    fn open(mut self: Pin<&mut Self>, id: &QString) -> bool {
        let opened = self.as_mut().rust_mut().panel.open(&id.to_string());
        self.reload();
        opened
    }

    fn close(mut self: Pin<&mut Self>) {
        self.as_mut().rust_mut().panel.inner.close();
        self.reload();
    }

    fn set_value(mut self: Pin<&mut Self>, row: i32, value: f64, dragging: bool) {
        let Some(param) = self.param(row).map(|p| p.desc.id) else {
            return;
        };
        self.as_mut().rust_mut().panel.edit(|p, doc| {
            let r = p.set_value(doc, param, value);
            if !dragging {
                p.end_gesture(doc);
            }
            r
        });
        self.reload();
    }

    fn set_position(self: Pin<&mut Self>, row: i32, position: f64, dragging: bool) {
        let value = self.value_at(row, position);
        if !value.is_nan() {
            self.set_value(row, value, dragging);
        }
    }

    fn value_at(&self, row: i32, position: f64) -> f64 {
        self.param(row)
            .map_or(f64::NAN, |p| p.desc.from_normalized(position))
    }

    fn end_gesture(mut self: Pin<&mut Self>) {
        self.as_mut().rust_mut().panel.edit(|p, doc| {
            p.end_gesture(doc);
            Ok(())
        });
    }

    fn reset(mut self: Pin<&mut Self>, row: i32) {
        let Some(param) = self.param(row).map(|p| p.desc.id) else {
            return;
        };
        self.as_mut()
            .rust_mut()
            .panel
            .edit(|p, doc| p.reset(doc, param));
        self.reload();
    }

    fn set_device_enabled(mut self: Pin<&mut Self>, on: bool) {
        self.as_mut()
            .rust_mut()
            .panel
            .edit(|p, doc| p.set_enabled(doc, on));
        self.reload();
    }

    fn move_by(mut self: Pin<&mut Self>, step: i32) {
        self.as_mut()
            .rust_mut()
            .panel
            .edit(|p, doc| p.move_by(doc, step as isize));
        self.reload();
    }

    fn refresh(self: Pin<&mut Self>) {
        if self.panel.revision() != self.revision {
            self.reload();
        }
    }

    /// Re-reads the open device: signals the rows that changed, or resets
    /// the model when the parameters themselves did.
    fn reload(mut self: Pin<&mut Self>) {
        let view = self.panel.view();
        let revision = self.panel.revision();
        self.as_mut().rust_mut().revision = revision;
        if view == self.view {
            return;
        }
        let ids = |v: &Option<DeviceView>| {
            v.as_ref()
                .map(|v| v.params.iter().map(|p| p.desc.clone()).collect::<Vec<_>>())
        };
        let same_rows = ids(&view) == ids(&self.view);
        if same_rows {
            let changed: Vec<i32> = {
                let old = self.params();
                let new = view.as_ref().map_or(&[][..], |v| &v.params);
                (0..new.len())
                    .filter(|&i| new[i] != old[i])
                    .map(|i| i as i32)
                    .collect()
            };
            self.as_mut().rust_mut().view = view.clone();
            let root = QModelIndex::default();
            for row in changed {
                let index = self.index(row, 0, &root);
                self.as_mut()
                    .data_changed(&index, &index, &QVector::default());
            }
        } else {
            self.as_mut().begin_reset_model();
            self.as_mut().rust_mut().view = view.clone();
            self.as_mut().end_reset_model();
        }
        let v = view.as_ref();
        let text = |f: fn(&DeviceView) -> &str| QString::from(v.map_or("", f));
        let id = QString::from(v.map(|v| v.id.to_string()).unwrap_or_default().as_str());
        // Read-only properties: set the field, then signal a change.
        macro_rules! update {
            ($field:ident, $signal:ident, $value:expr) => {{
                let value = $value;
                if self.$field != value {
                    self.as_mut().rust_mut().$field = value;
                    self.as_mut().$signal();
                }
            }};
        }
        update!(device_id, device_id_changed, id);
        update!(device_name, device_name_changed, text(|v| &v.name));
        update!(device_kind, device_kind_changed, text(|v| &v.kind));
        update!(track_name, track_name_changed, text(|v| &v.track_name));
        update!(on_master, on_master_changed, v.is_some_and(|v| v.on_master));
        update!(
            switched_on,
            switched_on_changed,
            v.is_some_and(|v| v.enabled)
        );
        update!(
            instrument,
            instrument_changed,
            v.is_some_and(DeviceView::is_instrument)
        );
        update!(
            can_move_earlier,
            can_move_earlier_changed,
            v.is_some_and(|v| v.can_move(-1))
        );
        update!(
            can_move_later,
            can_move_later_changed,
            v.is_some_and(|v| v.can_move(1))
        );
        update!(
            chain_index,
            chain_index_changed,
            v.map_or(-1, |v| v.index as i32)
        );
        update!(
            chain_length,
            chain_length_changed,
            v.map_or(0, |v| v.chain_len as i32)
        );
        update!(
            param_count,
            param_count_changed,
            v.map_or(0, |v| v.params.len() as i32)
        );
        update!(active, active_changed, v.is_some());
    }

    fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let Some(p) = self.param(index.row()) else {
            return QVariant::default();
        };
        let d = &p.desc;
        match ROLES.get((role - USER_ROLE) as usize).copied() {
            Some("paramId") => QVariant::from(&(d.id as i32)),
            Some("name") => QVariant::from(&QString::from(d.name.as_str())),
            Some("value") => QVariant::from(&p.value),
            Some("position") => QVariant::from(&d.to_normalized(p.value)),
            Some("defaultValue") => QVariant::from(&d.default),
            Some("minimum") => QVariant::from(&d.min),
            Some("maximum") => QVariant::from(&d.max),
            Some("control") => QVariant::from(&QString::from(control_name(&d.control))),
            Some("unit") => QVariant::from(&QString::from(unit_name(d.unit))),
            Some("choices") => {
                let mut list = QStringList::default();
                if let ParamControl::Choice(labels) = &d.control {
                    for l in labels {
                        list.append(QString::from(l.as_str()));
                    }
                }
                QVariant::from(&list)
            }
            Some("isDefault") => QVariant::from(&p.is_default()),
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
        self.params().len() as i32
    }
}
