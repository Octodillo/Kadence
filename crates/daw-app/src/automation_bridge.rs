//! `Automation`: the QML singleton for the rows under the arrange view's
//! tracks (their controls and automation lanes) and the timeline's point
//! menu. Tracks, their rows and their lanes are addressed by index. What
//! the rows are (kind, name, fold, lane, mode) is in the `rows` role of
//! [`Tracks`](crate::tracks_bridge); QML words the names (see `lanes`).

use std::pin::Pin;

use cxx_qt_lib::{QMap, QMapPair_QString_QVariant, QString, QVariant};
use daw_arrange::PointShape;

use crate::lanes::{LaneKind, LaneMode};
use crate::state::{self, AppState};

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qmap.h");
        type QMap_QString_QVariant = cxx_qt_lib::QMap<cxx_qt_lib::QMapPair_QString_QVariant>;
    }

    /// What a row under a track is (its `kind` in the `rows` role of
    /// `Tracks`): a control, which is also what a lane automates, or the
    /// heading of a group of them.
    #[qenum(Automation)]
    enum LaneKind {
        VolumeLane,
        PanLane,
        SendLane,
        DeviceParamLane,
        InstrumentGroup,
        EffectsGroup,
        SendsGroup,
        /// One effect, under Effects.
        DeviceGroup,
        /// A device's parameters past its first few.
        MoreGroup,
    }

    /// A lane's mode. Off: the control keeps its own value. Read: the lane
    /// plays. Write: it plays, and moving its control during playback
    /// records over it.
    #[qenum(Automation)]
    enum LaneMode {
        LaneOff,
        LaneRead,
        LaneWrite,
    }

    /// How the line leaves an automation point.
    #[qenum(Automation)]
    enum PointShape {
        LinearShape,
        StepShape,
        SlowStartShape,
        FastStartShape,
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        /// Bumped by every change made through this object, so views
        /// re-read at once (Project's revision follows on its next tick).
        #[qproperty(i32, revision)]
        type Automation = super::AutomationRust;

        /// Shows or hides the rows under the track.
        #[qinvokable]
        fn set_expanded(self: Pin<&mut Automation>, track: i32, on: bool);
        /// Opens or closes the group row `row` of the track is the heading
        /// of. An open group lists everything in it; a closed one only
        /// what a lane automates.
        #[qinvokable]
        fn set_row_open(self: Pin<&mut Automation>, track: i32, row: i32, on: bool);
        /// Sets the mode of the row's lane. Write gives a control without
        /// a lane one, to record into.
        #[qinvokable]
        fn set_row_mode(self: Pin<&mut Automation>, track: i32, row: i32, mode: LaneMode);
        #[qinvokable]
        fn set_lane_mode(self: Pin<&mut Automation>, track: i32, lane: i32, mode: LaneMode);
        #[qinvokable]
        fn remove_lane(self: Pin<&mut Automation>, track: i32, lane: i32);

        /// The row's control as it is now, in one read: `position` from 0
        /// to 1 along its scale (as the timeline draws its lane), `value`
        /// in the control's own unit (dB, pan from -1 (left) to 1, or the
        /// parameter's plain value), `following` when its lane plays its
        /// points so the control follows them, `writes` when moving the
        /// control during playback records over the lane, and `editable`
        /// unless nothing describes the parameter (a plugin that is not
        /// loaded), when the control stays where it is. All zero and false
        /// for a row without a control.
        #[qinvokable]
        fn row_control(self: &Automation, track: i32, row: i32) -> QMap_QString_QVariant;
        /// The user grabbed the row's control (also around a key press):
        /// starts an undo step, and a take when writing during playback.
        #[qinvokable]
        fn begin_row_control(self: Pin<&mut Automation>, track: i32, row: i32);
        /// Moves the row's control (fader, pan, send, device parameter) to
        /// `position` on its scale, as the same control on the Mix page or
        /// device panel would.
        #[qinvokable]
        fn set_row_position(self: Pin<&mut Automation>, track: i32, row: i32, position: f64);
        /// The user let go of the row's control.
        #[qinvokable]
        fn end_row_control(self: Pin<&mut Automation>, track: i32, row: i32);

        /// The selected points' shape, a `PointShape`; -1 with nothing
        /// selected.
        #[qinvokable]
        fn point_shape(self: &Automation) -> i32;
        /// Gives the selected points a `PointShape`.
        #[qinvokable]
        fn set_point_shape(self: Pin<&mut Automation>, shape: PointShape);
        #[qinvokable]
        fn delete_points(self: Pin<&mut Automation>);
    }
}

#[derive(Default)]
pub struct AutomationRust {
    revision: i32,
}

/// A point shape as QML's `Automation.PointShape` value.
pub(crate) fn shape_index(s: PointShape) -> i32 {
    use qobject::PointShape as Qml;
    let shape = match s {
        PointShape::Linear => Qml::LinearShape,
        PointShape::Step => Qml::StepShape,
        PointShape::SlowStart => Qml::SlowStartShape,
        PointShape::FastStart => Qml::FastStartShape,
    };
    shape.repr
}

/// The shape QML's `Automation.PointShape` names; linear for a value that
/// is none of them.
pub(crate) fn shape(shape: qobject::PointShape) -> PointShape {
    use qobject::PointShape as Qml;
    match shape {
        Qml::StepShape => PointShape::Step,
        Qml::SlowStartShape => PointShape::SlowStart,
        Qml::FastStartShape => PointShape::FastStart,
        _ => PointShape::Linear,
    }
}

/// A lane kind as QML's `Automation.LaneKind` value.
pub(crate) fn lane_kind_value(kind: LaneKind) -> i32 {
    use qobject::LaneKind as Qml;
    let kind = match kind {
        LaneKind::Volume => Qml::VolumeLane,
        LaneKind::Pan => Qml::PanLane,
        LaneKind::Send => Qml::SendLane,
        LaneKind::DeviceParam => Qml::DeviceParamLane,
        LaneKind::Instrument => Qml::InstrumentGroup,
        LaneKind::Effects => Qml::EffectsGroup,
        LaneKind::Sends => Qml::SendsGroup,
        LaneKind::Device => Qml::DeviceGroup,
        LaneKind::More => Qml::MoreGroup,
    };
    kind.repr
}

/// A lane mode as QML's `Automation.LaneMode` value.
pub(crate) fn lane_mode_value(mode: LaneMode) -> i32 {
    use qobject::LaneMode as Qml;
    let mode = match mode {
        LaneMode::Off => Qml::LaneOff,
        LaneMode::Read => Qml::LaneRead,
        LaneMode::Write => Qml::LaneWrite,
    };
    mode.repr
}

fn lane_mode(mode: qobject::LaneMode) -> LaneMode {
    use qobject::LaneMode as Qml;
    match mode {
        Qml::LaneOff => LaneMode::Off,
        Qml::LaneWrite => LaneMode::Write,
        _ => LaneMode::Read,
    }
}

impl qobject::Automation {
    /// Runs an edit on the state and tells QML.
    fn edit(mut self: Pin<&mut Self>, f: impl FnOnce(&mut AppState)) {
        state::with(|s| {
            s.end_drag();
            f(s);
            s.sync();
            s.touch();
        });
        let next = self.revision.wrapping_add(1);
        self.as_mut().set_revision(next);
    }

    fn set_expanded(self: Pin<&mut Self>, track: i32, on: bool) {
        self.edit(|s| s.set_lanes_expanded(track, on));
    }

    fn set_lane_mode(self: Pin<&mut Self>, track: i32, lane: i32, mode: qobject::LaneMode) {
        self.edit(|s| s.set_lane_mode(track, lane, lane_mode(mode)));
    }

    fn remove_lane(self: Pin<&mut Self>, track: i32, lane: i32) {
        self.edit(|s| s.remove_lane(track, lane));
    }

    fn set_row_open(self: Pin<&mut Self>, track: i32, row: i32, on: bool) {
        self.edit(|s| s.set_row_open(track, row, on));
    }

    fn set_row_mode(self: Pin<&mut Self>, track: i32, row: i32, mode: qobject::LaneMode) {
        self.edit(|s| s.set_row_mode(track, row, lane_mode(mode)));
    }

    fn row_control(&self, track: i32, row: i32) -> QMap<QMapPair_QString_QVariant> {
        let c = state::with(|s| s.target_control(track, s.row_target(track, row)?));
        let mut map = QMap::<QMapPair_QString_QVariant>::default();
        let mut put = |key: &str, value: QVariant| map.insert(QString::from(key), value);
        put(
            "position",
            QVariant::from(&c.as_ref().map_or(0.0, |c| c.norm)),
        );
        put(
            "value",
            QVariant::from(&c.as_ref().map_or(0.0, |c| c.value)),
        );
        put(
            "following",
            QVariant::from(&c.as_ref().is_some_and(|c| c.following)),
        );
        put(
            "writes",
            QVariant::from(&c.as_ref().is_some_and(|c| c.writes)),
        );
        put(
            "editable",
            QVariant::from(&c.as_ref().is_some_and(|c| c.editable)),
        );
        map
    }

    // These play the change and bump the view's revision themselves (see
    // `AppState::begin_control` and its fellows); the lanes' own revision
    // doesn't change.
    fn begin_row_control(self: Pin<&mut Self>, track: i32, row: i32) {
        state::with(|s| {
            if let Some(target) = s.row_target(track, row) {
                s.begin_control(track, target);
            }
        });
    }

    fn set_row_position(self: Pin<&mut Self>, track: i32, row: i32, position: f64) {
        state::with(|s| {
            if let Some(target) = s.row_target(track, row) {
                s.set_control(track, target, position);
            }
        });
    }

    fn end_row_control(self: Pin<&mut Self>, track: i32, row: i32) {
        state::with(|s| {
            if let Some(target) = s.row_target(track, row) {
                s.end_control(track, target);
            }
        });
    }

    fn point_shape(&self) -> i32 {
        state::with(|s| {
            s.arrange
                .point_shape(s.doc.project())
                .map_or(-1, shape_index)
        })
    }

    fn set_point_shape(self: Pin<&mut Self>, to: qobject::PointShape) {
        self.edit(|s| {
            let out = s.arrange.set_point_shape(&mut s.doc, shape(to));
            s.apply(out);
        });
    }

    fn delete_points(self: Pin<&mut Self>) {
        self.edit(|s| {
            let out = s.arrange.delete_points(&mut s.doc);
            s.apply(out);
        });
    }
}
