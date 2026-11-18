//! `Automation`: the QML singleton for editing automation lanes from the
//! track headers and the timeline's point menu. Tracks and lanes are
//! addressed by index. What the lanes are (kind, name, mode) is in the
//! `lanes` role of [`Tracks`](crate::tracks_bridge); QML words the names
//! (see `lanes`).

use std::pin::Pin;

use cxx_qt_lib::{QMap, QMapPair_QString_QVariant, QString, QVariant};
use daw_arrange::PointShape;

use crate::lanes::{LaneKind, LaneMode, TargetName};
use crate::state::{self, AppState};

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qmap.h");
        type QMap_QString_QVariant = cxx_qt_lib::QMap<cxx_qt_lib::QMapPair_QString_QVariant>;
    }

    /// What a lane automates: the `kind` of a lane (in the `lanes` role of
    /// `Tracks`) and of a target.
    #[qenum(Automation)]
    enum LaneKind {
        VolumeLane,
        PanLane,
        SendLane,
        DeviceParamLane,
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

        /// Shows or hides the track's lanes.
        #[qinvokable]
        fn set_expanded(self: Pin<&mut Automation>, track: i32, on: bool);
        #[qinvokable]
        fn set_lane_mode(self: Pin<&mut Automation>, track: i32, lane: i32, mode: LaneMode);
        #[qinvokable]
        fn remove_lane(self: Pin<&mut Automation>, track: i32, lane: i32);

        /// What the lane controls, as it is now, in one read: `position`
        /// from 0 to 1 along the lane's scale (as the timeline draws it),
        /// `value` in the target's own unit (dB, pan from -1 (left) to 1,
        /// or the parameter's plain value), `following` when the lane
        /// plays its points so its control follows them, and `writes` when
        /// moving the control during playback records over the lane. All
        /// zero and false for no such lane.
        #[qinvokable]
        fn lane_control(self: &Automation, track: i32, lane: i32) -> QMap_QString_QVariant;
        /// The user grabbed the lane's control (also around a key press):
        /// starts an undo step, and a take when writing during playback.
        #[qinvokable]
        fn begin_lane_control(self: Pin<&mut Automation>, track: i32, lane: i32);
        /// Moves what the lane controls (fader, pan, send, device
        /// parameter) to `position` on its scale, as the same control on
        /// the Mix page or device panel would.
        #[qinvokable]
        fn set_lane_position(self: Pin<&mut Automation>, track: i32, lane: i32, position: f64);
        /// The user let go of the lane's control.
        #[qinvokable]
        fn end_lane_control(self: Pin<&mut Automation>, track: i32, lane: i32);
        /// Targets a new lane on `track` can have, each named in parts as a
        /// lane is: its kind (a `LaneKind`, -1 for no such target), the bus
        /// or device, and the parameter.
        #[qinvokable]
        fn target_count(self: &Automation, track: i32) -> i32;
        #[qinvokable]
        fn target_kind(self: &Automation, track: i32, target: i32) -> i32;
        #[qinvokable]
        fn target_detail(self: &Automation, track: i32, target: i32) -> QString;
        #[qinvokable]
        fn target_param(self: &Automation, track: i32, target: i32) -> QString;
        #[qinvokable]
        fn add_lane(self: Pin<&mut Automation>, track: i32, target: i32);

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

fn kind(n: Option<TargetName>) -> i32 {
    n.map_or(-1, |n| lane_kind_value(n.kind))
}

fn text(s: Option<String>) -> QString {
    QString::from(s.unwrap_or_default().as_str())
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

    fn lane_control(&self, track: i32, lane: i32) -> QMap<QMapPair_QString_QVariant> {
        let c = state::with(|s| s.lane_control(track, lane));
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
        map
    }

    // These play the change and bump the view's revision themselves (see
    // `AppState::control_lane`); the lanes' own revision doesn't change.
    fn begin_lane_control(self: Pin<&mut Self>, track: i32, lane: i32) {
        state::with(|s| s.begin_lane_control(track, lane));
    }

    fn set_lane_position(self: Pin<&mut Self>, track: i32, lane: i32, position: f64) {
        state::with(|s| s.set_lane_control(track, lane, position));
    }

    fn end_lane_control(self: Pin<&mut Self>, track: i32, lane: i32) {
        state::with(|s| s.end_lane_control(track, lane));
    }

    fn target(&self, track: i32, target: i32) -> Option<TargetName> {
        let i = usize::try_from(target).ok()?;
        state::with(|s| s.lane_targets(track).into_iter().nth(i).map(|(_, n)| n))
    }

    fn target_count(&self, track: i32) -> i32 {
        state::with(|s| s.lane_targets(track).len() as i32)
    }

    fn target_kind(&self, track: i32, target: i32) -> i32 {
        kind(self.target(track, target))
    }

    fn target_detail(&self, track: i32, target: i32) -> QString {
        text(self.target(track, target).map(|n| n.detail))
    }

    fn target_param(&self, track: i32, target: i32) -> QString {
        text(self.target(track, target).map(|n| n.param))
    }

    fn add_lane(self: Pin<&mut Self>, track: i32, target: i32) {
        self.edit(|s| s.add_lane(track, target));
    }

    fn point_shape(&self) -> i32 {
        state::with(|s| {
            s.arrange
                .lanes
                .selected_shape(s.doc.project())
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
