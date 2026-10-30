//! `Automation`: the QML singleton for automation lanes in the track
//! headers and the timeline's point menu. Tracks and lanes are addressed by
//! index, like `Project`'s tracks; QML words the names (see `lanes`).

use std::pin::Pin;

use cxx_qt_lib::QString;
use daw_arrange::PointShape;

use crate::lanes::{LaneMode, TargetName};
use crate::state::{self, AppState};

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
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
