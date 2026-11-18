//! `Tracks`: the QML list model of the tracks beside the timeline, over
//! [`TrackRow`](crate::tracks::TrackRow)s.
//!
//! Each row is a track; a track header is its delegate and takes the roles
//! as required properties. The edits a header makes are invokables that
//! take the row. `sync` re-reads the rows after a change made elsewhere
//! (undo, the mixer, a new project) and signals only the rows that differ,
//! so a header is left alone unless its track changed.

use std::pin::Pin;

use cxx_qt::CxxQtType;
use cxx_qt_lib::{
    QHash, QHashPair_i32_QByteArray, QList, QMap, QMapPair_QString_QVariant, QModelIndex, QString,
    QVariant, QVector,
};
use daw_model::MidiInput;
use daw_model::commands::TrackField;

use crate::automation_bridge::{lane_kind_value, lane_mode_value};
use crate::model_roles;
use crate::paint::to_qcolor;
use crate::qt_lists::{ModelRole, row, string_list};
use crate::state::{self, AppState};
use crate::tracks::{LaneRow, TrackKind, TrackRow};

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        // Declared once, in the Mixer's bridge; declaring it here too would
        // define its casts twice.
        include!(<QtCore/QAbstractListModel>);
        type QAbstractListModel = crate::mixer_bridge::qobject::QAbstractListModel;
        include!("cxx-qt-lib/qhash.h");
        type QHash_i32_QByteArray = cxx_qt_lib::QHash<cxx_qt_lib::QHashPair_i32_QByteArray>;
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qvariant.h");
        type QVariant = cxx_qt_lib::QVariant;
        include!("cxx-qt-lib/qmodelindex.h");
        type QModelIndex = cxx_qt_lib::QModelIndex;
        include!("cxx-qt-lib/qvector.h");
        type QVector_i32 = cxx_qt_lib::QVector<i32>;
        include!("cxx-qt-lib/qmap.h");
        type QMap_QString_QVariant = cxx_qt_lib::QMap<cxx_qt_lib::QMapPair_QString_QVariant>;
    }

    /// The `kind` role, and the `kind` argument of `add` and `nextNumber`.
    #[qenum(Tracks)]
    enum TrackKind {
        AudioTrack,
        InstrumentTrack,
        BusTrack,
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[base = QAbstractListModel]
        #[qml_element]
        #[qml_singleton]
        /// Hardware inputs the running audio engine has, for the audio
        /// tracks' input menus.
        #[qproperty(i32, input_count)]
        /// How many tracks the list holds, for code that is not a view.
        #[qproperty(i32, count)]
        type Tracks = super::TracksRust;

        /// Re-reads the tracks if anything about them changed elsewhere.
        #[qinvokable]
        fn sync(self: Pin<&mut Tracks>);
        /// The roles of the track at `row` by name, for code that is not a
        /// delegate (the self test); empty when there is no such row.
        #[qinvokable]
        fn get(self: &Tracks, row: i32) -> QMap_QString_QVariant;

        /// Adds a track of `kind` called `name` after the selected track;
        /// QML words the default name with `nextNumber`.
        #[qinvokable]
        fn add(self: Pin<&mut Tracks>, kind: TrackKind, name: &QString);
        /// The number for the next default track name of `kind`, as in
        /// "Audio 3".
        #[qinvokable]
        fn next_number(self: &Tracks, kind: TrackKind) -> i32;
        #[qinvokable]
        fn remove(self: Pin<&mut Tracks>, row: i32);
        /// Moves the track at `from` to row `to`, as one undo step. (Not
        /// `move`, a keyword in Rust and a function in C++.)
        #[qinvokable]
        #[cxx_name = "moveTrack"]
        fn move_track(self: Pin<&mut Tracks>, from: i32, to: i32);
        /// A blank name is ignored.
        #[qinvokable]
        fn rename(self: Pin<&mut Tracks>, row: i32, name: &QString);
        #[qinvokable]
        fn set_muted(self: Pin<&mut Tracks>, row: i32, muted: bool);
        #[qinvokable]
        fn set_soloed(self: Pin<&mut Tracks>, row: i32, soloed: bool);
        /// Arms the track for recording, or disarms it.
        #[qinvokable]
        fn set_armed(self: Pin<&mut Tracks>, row: i32, armed: bool);
        /// Sets the hardware input an audio track records: `first` (from
        /// 0) alone, or as a stereo pair with the one after it.
        #[qinvokable]
        fn set_input(self: Pin<&mut Tracks>, row: i32, first: i32, stereo: bool);
        /// Sets whether an audio track monitors its input while armed.
        #[qinvokable]
        fn set_monitor_auto(self: Pin<&mut Tracks>, row: i32, on: bool);
        /// Sets which live MIDI an instrument track takes: `device` empty
        /// for every input, `channel` -1 for every channel.
        #[qinvokable]
        fn set_midi_input(self: Pin<&mut Tracks>, row: i32, device: &QString, channel: i32);
        /// Swaps the track's instrument for row `choice` of `Instruments`
        /// at its `revision`.
        #[qinvokable]
        fn set_instrument(self: Pin<&mut Tracks>, row: i32, choice: i32, revision: i32);
        /// The id of the track's instrument device, for `DevicePanel.open`;
        /// empty when it has none.
        #[qinvokable]
        fn instrument_device(self: &Tracks, row: i32) -> QString;
    }

    extern "RustQt" {
        /// An edit was made through this model; whatever else shows the
        /// project is stale.
        #[qsignal]
        fn edited(self: Pin<&mut Tracks>);
    }

    unsafe extern "RustQt" {
        #[inherit]
        #[qsignal]
        #[cxx_name = "dataChanged"]
        fn data_changed(
            self: Pin<&mut Tracks>,
            top_left: &QModelIndex,
            bottom_right: &QModelIndex,
            roles: &QVector_i32,
        );

        #[inherit]
        #[cxx_name = "beginInsertRows"]
        fn begin_insert_rows(self: Pin<&mut Tracks>, parent: &QModelIndex, first: i32, last: i32);
        #[inherit]
        #[cxx_name = "endInsertRows"]
        fn end_insert_rows(self: Pin<&mut Tracks>);
        #[inherit]
        #[cxx_name = "beginRemoveRows"]
        fn begin_remove_rows(self: Pin<&mut Tracks>, parent: &QModelIndex, first: i32, last: i32);
        #[inherit]
        #[cxx_name = "endRemoveRows"]
        fn end_remove_rows(self: Pin<&mut Tracks>);

        #[inherit]
        fn index(self: &Tracks, row: i32, column: i32, parent: &QModelIndex) -> QModelIndex;
    }

    extern "RustQt" {
        #[qinvokable]
        #[cxx_override]
        fn data(self: &Tracks, index: &QModelIndex, role: i32) -> QVariant;

        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "roleNames"]
        fn role_names(self: &Tracks) -> QHash_i32_QByteArray;

        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "rowCount"]
        fn row_count(self: &Tracks, parent: &QModelIndex) -> i32;
    }

    impl cxx_qt::Initialize for Tracks {}
}

use qobject::TrackKind as QmlKind;

model_roles!(
    /// What a track header reads.
    enum Role {
        /// The track's id, which stays with it when its row changes.
        TrackId => "trackId",
        /// Whether it is the selected track.
        Selected => "selected",
        Name => "name",
        /// A `Tracks.TrackKind`.
        Kind => "kind",
        Color => "color",
        Muted => "muted",
        Soloed => "soloed",
        Armed => "armed",
        /// The first hardware input an audio track records, from 0.
        Input => "input",
        /// Whether it records a stereo pair from that input on.
        InputStereo => "inputStereo",
        /// Whether an audio track monitors its input while armed.
        MonitorAuto => "monitorAuto",
        /// The MIDI source port an instrument track listens to; empty for
        /// every input.
        MidiDevice => "midiDevice",
        /// The MIDI channel (0 to 15) it listens to; -1 for every channel.
        MidiChannel => "midiChannel",
        /// The instrument's name; empty when the track has none.
        Instrument => "instrument",
        /// The instrument's row in `Instruments`, or -1.
        InstrumentChoice => "instrumentChoice",
        /// Whether the track shows its automation lanes.
        Expanded => "expanded",
        /// The automation lanes, each a map: `kind` (an
        /// `Automation.LaneKind`), `detail` (the bus a send goes to, or the
        /// device's name), `param` (a device parameter's name), `mode` (an
        /// `Automation.LaneMode`) and, for device parameters, how
        /// `ValueText.param` words the value: `control`, `unit` and
        /// `choices`.
        Lanes => "lanes",
    }
);

#[derive(Default)]
pub struct TracksRust {
    input_count: i32,
    count: i32,
    rows: Vec<TrackRow>,
}

fn qml_kind(kind: TrackKind) -> QmlKind {
    match kind {
        TrackKind::Audio => QmlKind::AudioTrack,
        TrackKind::Instrument => QmlKind::InstrumentTrack,
        TrackKind::Bus => QmlKind::BusTrack,
    }
}

/// `None` for a value QML made up.
fn track_kind(kind: QmlKind) -> Option<TrackKind> {
    match kind {
        QmlKind::AudioTrack => Some(TrackKind::Audio),
        QmlKind::InstrumentTrack => Some(TrackKind::Instrument),
        QmlKind::BusTrack => Some(TrackKind::Bus),
        _ => None,
    }
}

fn text(s: &str) -> QVariant {
    QVariant::from(&QString::from(s))
}

fn lane_map(lane: &LaneRow) -> QVariant {
    let param = lane.param.as_ref();
    let mut map = QMap::<QMapPair_QString_QVariant>::default();
    let mut put = |key: &str, value: QVariant| map.insert(QString::from(key), value);
    put("kind", QVariant::from(&lane_kind_value(lane.name.kind)));
    put("detail", text(&lane.name.detail));
    put("param", text(&lane.name.param));
    put("mode", QVariant::from(&lane_mode_value(lane.mode)));
    put("control", text(param.map_or("", |p| p.control_name())));
    put("unit", text(param.map_or("", |p| p.unit_name())));
    let choices = param.map_or(&[][..], |p| p.choices());
    put("choices", QVariant::from(&string_list(choices)));
    QVariant::from(&map)
}

/// Lanes as the `lanes` role holds them: a list of maps (see [`Role::Lanes`]).
pub(crate) fn lane_list(lanes: &[LaneRow]) -> QList<QVariant> {
    let mut list = QList::default();
    for lane in lanes {
        list.append(lane_map(lane));
    }
    list
}

fn value(t: &TrackRow, role: Role) -> QVariant {
    let instrument = t.instrument.as_ref();
    match role {
        Role::TrackId => text(&t.id.to_string()),
        Role::Selected => QVariant::from(&t.selected),
        Role::Name => text(&t.name),
        Role::Kind => QVariant::from(&qml_kind(t.kind).repr),
        Role::Color => QVariant::from(&to_qcolor(t.color)),
        Role::Muted => QVariant::from(&t.muted),
        Role::Soloed => QVariant::from(&t.soloed),
        Role::Armed => QVariant::from(&t.armed),
        Role::Input => QVariant::from(&i32::from(t.audio_input.channels.first())),
        Role::InputStereo => QVariant::from(&matches!(
            t.audio_input.channels,
            daw_model::InputChannels::Stereo(_)
        )),
        Role::MonitorAuto => {
            QVariant::from(&(t.audio_input.monitor == daw_model::Monitoring::Auto))
        }
        Role::MidiDevice => text(t.midi_input.device.as_deref().unwrap_or_default()),
        Role::MidiChannel => QVariant::from(&t.midi_input.channel.map_or(-1, i32::from)),
        Role::Instrument => text(instrument.map_or("", |i| i.name.as_str())),
        Role::InstrumentChoice => QVariant::from(
            &instrument
                .and_then(|i| i32::try_from(i.choice?).ok())
                .unwrap_or(-1),
        ),
        Role::Expanded => QVariant::from(&t.expanded),
        Role::Lanes => QVariant::from(&lane_list(&t.lanes)),
    }
}

impl cxx_qt::Initialize for qobject::Tracks {
    fn initialize(self: Pin<&mut Self>) {
        self.reload();
    }
}

impl qobject::Tracks {
    /// Runs an edit of the track at `row` on the state, then shows it.
    fn edit(self: Pin<&mut Self>, row: i32, f: impl FnOnce(&mut AppState, usize)) {
        if let Ok(row) = usize::try_from(row) {
            self.edit_all(|s| f(s, row));
        }
    }

    /// Runs an edit on the state, then shows it.
    fn edit_all(mut self: Pin<&mut Self>, f: impl FnOnce(&mut AppState)) {
        state::with(|s| s.edit(f));
        self.as_mut().reload();
        self.edited();
    }

    fn sync(self: Pin<&mut Self>) {
        self.reload();
    }

    /// Re-reads the tracks. Rows are tracks by position, as the timeline
    /// lays them out: tracks that came or went change the row count at the
    /// end, and every row whose track is now different, or differently
    /// set, is signalled.
    fn reload(mut self: Pin<&mut Self>) {
        let (rows, inputs) = state::with(|s| (s.track_rows(), i32::from(s.input_count())));
        self.as_mut().set_input_count(inputs);
        if rows == self.rows {
            return;
        }
        let (before, after) = (self.rows.len(), rows.len());
        let changed: Vec<i32> = (0..before.min(after))
            .filter(|&i| rows[i] != self.rows[i])
            .map(|i| i as i32)
            .collect();
        let root = QModelIndex::default();
        if after > before {
            self.as_mut()
                .begin_insert_rows(&root, before as i32, after as i32 - 1);
            self.as_mut().rust_mut().rows = rows;
            self.as_mut().end_insert_rows();
        } else if after < before {
            self.as_mut()
                .begin_remove_rows(&root, after as i32, before as i32 - 1);
            self.as_mut().rust_mut().rows = rows;
            self.as_mut().end_remove_rows();
        } else {
            self.as_mut().rust_mut().rows = rows;
        }
        self.as_mut().set_count(after as i32);
        for row in changed {
            let index = self.index(row, 0, &root);
            self.as_mut()
                .data_changed(&index, &index, &QVector::default());
        }
    }

    fn get(&self, row: i32) -> QMap<QMapPair_QString_QVariant> {
        let mut map = QMap::<QMapPair_QString_QVariant>::default();
        let track = usize::try_from(row).ok().and_then(|r| self.rows.get(r));
        if let Some(t) = track {
            for (role, name) in Role::ALL {
                map.insert(QString::from(*name), value(t, *role));
            }
        }
        map
    }

    fn add(self: Pin<&mut Self>, kind: QmlKind, name: &QString) {
        let name = name.to_string();
        if let Some(kind) = track_kind(kind) {
            self.edit_all(|s| s.add_track(kind, name));
        }
    }

    fn next_number(&self, kind: QmlKind) -> i32 {
        let number = track_kind(kind).map(|k| state::with(|s| s.next_track_number(k)));
        number.and_then(|n| i32::try_from(n).ok()).unwrap_or(1)
    }

    fn remove(self: Pin<&mut Self>, row: i32) {
        self.edit(row, |s, row| s.remove_track(row));
    }

    fn move_track(self: Pin<&mut Self>, from: i32, to: i32) {
        if let Ok(to) = usize::try_from(to) {
            self.edit(from, |s, from| s.move_track(from, to));
        }
    }

    fn rename(self: Pin<&mut Self>, row: i32, name: &QString) {
        let name = name.to_string();
        self.edit(row, |s, row| s.rename_track(row, name));
    }

    fn set_muted(self: Pin<&mut Self>, row: i32, muted: bool) {
        self.edit(row, |s, row| {
            s.set_track_field(row, TrackField::Mute(muted))
        });
    }

    fn set_soloed(self: Pin<&mut Self>, row: i32, soloed: bool) {
        self.edit(row, |s, row| {
            s.set_track_field(row, TrackField::Solo(soloed))
        });
    }

    fn set_armed(self: Pin<&mut Self>, row: i32, armed: bool) {
        self.edit(row, |s, row| {
            s.set_track_field(row, TrackField::Armed(armed))
        });
    }

    fn set_input(self: Pin<&mut Self>, row: i32, first: i32, stereo: bool) {
        if let Ok(first) = u16::try_from(first) {
            self.edit(row, |s, row| s.set_track_input(row, first, stereo));
        }
    }

    fn set_monitor_auto(self: Pin<&mut Self>, row: i32, on: bool) {
        self.edit(row, |s, row| s.set_track_monitor_auto(row, on));
    }

    fn set_midi_input(self: Pin<&mut Self>, row: i32, device: &QString, channel: i32) {
        let device = device.to_string();
        let input = MidiInput {
            device: (!device.is_empty()).then_some(device),
            channel: u8::try_from(channel).ok().filter(|&c| c < 16),
        };
        self.edit(row, |s, row| s.set_track_midi_input(row, input));
    }

    fn set_instrument(self: Pin<&mut Self>, row: i32, choice: i32, revision: i32) {
        // Read before `edit`, which holds the state.
        if let Some(choice) = crate::instruments_bridge::choice_at(choice, revision) {
            self.edit(row, |s, row| s.set_track_instrument(row, &choice));
        }
    }

    fn instrument_device(&self, row: i32) -> QString {
        let track = usize::try_from(row).ok().and_then(|r| self.rows.get(r));
        let device = track.and_then(|t| t.instrument.as_ref());
        QString::from(device.map_or("", |i| i.device.as_str()))
    }

    fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        match (row(&self.rows, index), Role::from_qt(role)) {
            (Some(track), Some(role)) => value(track, role),
            _ => QVariant::default(),
        }
    }

    fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        Role::names()
    }

    fn row_count(&self, _parent: &QModelIndex) -> i32 {
        i32::try_from(self.rows.len()).unwrap_or(i32::MAX)
    }
}
