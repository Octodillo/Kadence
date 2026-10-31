//! `Automation`: the QML singleton for automation lanes in the track
//! headers and the timeline's point menu. Tracks and lanes are addressed by
//! index, like `Project`'s tracks; QML words the names (see `lanes`).

use std::pin::Pin;

use cxx_qt_lib::{QString, QStringList};
use daw_arrange::PointShape;

use crate::lanes::{LaneMode, TargetName};
use crate::qt_lists::string_list;
use crate::state::{self, AppState};

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qstringlist.h");
        type QStringList = cxx_qt_lib::QStringList;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        /// Bumped by every change made through this object, so bindings
        /// re-read at once (Project's revision follows on its next tick).
        #[qproperty(i32, revision)]
        type Automation = super::AutomationRust;

        /// Lanes on track `track`.
        #[qinvokable]
        fn lane_count(self: &Automation, track: i32) -> i32;
        /// Whether the track shows its lanes.
        #[qinvokable]
        fn expanded(self: &Automation, track: i32) -> bool;
        #[qinvokable]
        fn set_expanded(self: Pin<&mut Automation>, track: i32, on: bool);
        /// 0 volume, 1 pan, 2 send level, 3 device parameter.
        #[qinvokable]
        fn lane_kind(self: &Automation, track: i32, lane: i32) -> i32;
        /// The bus a send goes to, or the device's name.
        #[qinvokable]
        fn lane_detail(self: &Automation, track: i32, lane: i32) -> QString;
        /// The parameter's name, for device parameters.
        #[qinvokable]
        fn lane_param(self: &Automation, track: i32, lane: i32) -> QString;
        /// 0 off, 1 read, 2 write.
        #[qinvokable]
        fn lane_mode(self: &Automation, track: i32, lane: i32) -> i32;
        #[qinvokable]
        fn set_lane_mode(self: Pin<&mut Automation>, track: i32, lane: i32, mode: i32);
        #[qinvokable]
        fn remove_lane(self: Pin<&mut Automation>, track: i32, lane: i32);

        /// What the lane's target is at now, from 0 to 1 along the lane's
        /// scale (as the timeline draws it); 0 for no such lane.
        #[qinvokable]
        fn lane_position(self: &Automation, track: i32, lane: i32) -> f64;
        /// The same in the target's own unit: dB, pan from -1 (left) to 1,
        /// or the parameter's plain value.
        #[qinvokable]
        fn lane_value(self: &Automation, track: i32, lane: i32) -> f64;
        /// Whether the lane plays its points, so its control follows them.
        #[qinvokable]
        fn lane_following(self: &Automation, track: i32, lane: i32) -> bool;
        /// Whether moving the lane's control during playback records over
        /// the lane, so it can move while the lane follows its points.
        #[qinvokable]
        fn lane_writes(self: &Automation, track: i32, lane: i32) -> bool;
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
        /// For device-parameter lanes, how ValueText words the value: the
        /// control kind, the unit and a choice parameter's labels (see
        /// `ParamDesc`); empty for other lanes.
        #[qinvokable]
        fn lane_control_kind(self: &Automation, track: i32, lane: i32) -> QString;
        #[qinvokable]
        fn lane_unit(self: &Automation, track: i32, lane: i32) -> QString;
        #[qinvokable]
        fn lane_choices(self: &Automation, track: i32, lane: i32) -> QStringList;

        /// Targets a new lane on `track` can have, like the lane getters.
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

        /// The selected points' shape: 0 linear, 1 step, 2 slow start,
        /// 3 fast start; -1 with nothing selected.
        #[qinvokable]
        fn point_shape(self: &Automation) -> i32;
        #[qinvokable]
        fn set_point_shape(self: Pin<&mut Automation>, shape: i32);
        #[qinvokable]
        fn delete_points(self: Pin<&mut Automation>);
    }
}

#[derive(Default)]
pub struct AutomationRust {
    revision: i32,
}

fn shape_index(s: PointShape) -> i32 {
    match s {
        PointShape::Linear => 0,
        PointShape::Step => 1,
        PointShape::SlowStart => 2,
        PointShape::FastStart => 3,
    }
}

fn shape(i: i32) -> PointShape {
    match i {
        1 => PointShape::Step,
        2 => PointShape::SlowStart,
        3 => PointShape::FastStart,
        _ => PointShape::Linear,
    }
}

fn kind(n: Option<TargetName>) -> i32 {
    n.map_or(-1, |n| n.kind as i32)
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

    fn lane_count(&self, track: i32) -> i32 {
        state::with(|s| s.lane_count(track) as i32)
    }

    fn expanded(&self, track: i32) -> bool {
        state::with(|s| s.lanes_expanded(track))
    }

    fn set_expanded(self: Pin<&mut Self>, track: i32, on: bool) {
        self.edit(|s| s.set_lanes_expanded(track, on));
    }

    fn lane_kind(&self, track: i32, lane: i32) -> i32 {
        state::with(|s| kind(s.lane_name(track, lane)))
    }

    fn lane_detail(&self, track: i32, lane: i32) -> QString {
        text(state::with(|s| s.lane_name(track, lane).map(|n| n.detail)))
    }

    fn lane_param(&self, track: i32, lane: i32) -> QString {
        text(state::with(|s| s.lane_name(track, lane).map(|n| n.param)))
    }

    fn lane_mode(&self, track: i32, lane: i32) -> i32 {
        state::with(|s| s.lane_mode(track, lane) as i32)
    }

    fn set_lane_mode(self: Pin<&mut Self>, track: i32, lane: i32, mode: i32) {
        self.edit(|s| s.set_lane_mode(track, lane, LaneMode::from_index(mode)));
    }

    fn remove_lane(self: Pin<&mut Self>, track: i32, lane: i32) {
        self.edit(|s| s.remove_lane(track, lane));
    }

    fn lane_position(&self, track: i32, lane: i32) -> f64 {
        state::with(|s| s.lane_control(track, lane).map_or(0.0, |c| c.norm))
    }

    fn lane_value(&self, track: i32, lane: i32) -> f64 {
        state::with(|s| s.lane_control(track, lane).map_or(0.0, |c| c.value))
    }

    fn lane_following(&self, track: i32, lane: i32) -> bool {
        state::with(|s| s.lane_control(track, lane).is_some_and(|c| c.following))
    }

    fn lane_writes(&self, track: i32, lane: i32) -> bool {
        state::with(|s| s.lane_control(track, lane).is_some_and(|c| c.writes))
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

    fn lane_control_kind(&self, track: i32, lane: i32) -> QString {
        text(state::with(|s| {
            s.lane_param(track, lane)
                .map(|p| p.control_name().to_owned())
        }))
    }

    fn lane_unit(&self, track: i32, lane: i32) -> QString {
        text(state::with(|s| {
            s.lane_param(track, lane).map(|p| p.unit_name().to_owned())
        }))
    }

    fn lane_choices(&self, track: i32, lane: i32) -> QStringList {
        state::with(|s| string_list(s.lane_param(track, lane).iter().flat_map(|p| p.choices())))
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
                .selected_shape(s.doc.project())
                .map_or(-1, shape_index)
        })
    }

    fn set_point_shape(self: Pin<&mut Self>, i: i32) {
        self.edit(|s| {
            let out = s.arrange.set_point_shape(&mut s.doc, shape(i));
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
