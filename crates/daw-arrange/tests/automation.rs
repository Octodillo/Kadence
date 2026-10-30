//! Automation lanes in the arrange view: layout, point gestures and undo.

use std::sync::Arc;

use daw_arrange::automation::{SILENCE_DB, lane_targets};
use daw_arrange::{Arrange, Env, Hit, LaneScale, ParamCatalog, PointShape, Pointer, Prim, Snap};
use daw_core::event::ParamId;
use daw_core::time::{PPQ, SegmentTempoMap, TickTime};
use daw_model::commands::{AddDevice, AddSend, AddTrack};
use daw_model::{
    AutomationPoint, AutomationTarget, Device, DeviceSource, Document, LaneId, Project, Send,
    Track, TrackId, TrackKind,
};

const BEAT: i64 = PPQ;
const BAR: i64 = 4 * PPQ;

struct Rig {
    doc: Document,
    arrange: Arrange,
    tempo: SegmentTempoMap,
    tracks: Vec<TrackId>,
}

impl Rig {
    /// Two audio tracks; 64 px tracks and 40 px lanes under a 24 px ruler.
    fn new() -> Self {
        let mut doc = Document::new(Project::new("t"));
        let mut tracks = Vec::new();
        for i in 0..2 {
            let t = Track::new(TrackKind::Audio, format!("T{i}"));
            tracks.push(t.id);
            doc.execute(AddTrack::new(i, t)).unwrap();
        }
        doc.seal();
        let mut arrange = Arrange::new();
        arrange.resize(doc.project(), 1000.0, 600.0);
        arrange.snap = Snap::Off;
        Self {
            doc,
            arrange,
            tempo: SegmentTempoMap::new(120.0),
            tracks,
        }
    }

    fn add_lane(&mut self, row: usize, target: AutomationTarget) -> LaneId {
        let o = self
            .arrange
            .add_lane(&mut self.doc, self.tracks[row], target);
        assert!(o.document, "{o:?}");
        let t = self.doc.project().track(self.tracks[row]).unwrap();
        t.automation.iter().find(|l| l.target == target).unwrap().id
    }

    fn points(&self, lane: LaneId) -> Vec<AutomationPoint> {
        let p = self.doc.project();
        let (t, i) = p.lane_location(lane).unwrap();
        p.track(t).unwrap().automation[i].points.clone()
    }

    fn x(&self, tick: i64) -> f64 {
        self.arrange.view.tick_to_x(TickTime(tick))
    }

    /// The y of height `n` (0 bottom, 1 top) in lane `index` of `row`.
    fn y(&self, row: usize, index: usize, n: f64) -> f64 {
        let r = self.arrange.lane_rect(row, index);
        // The lane keeps 4 px free above and below the curve.
        r.bottom() - 4.0 - n * (r.h - 8.0)
    }
}

fn env(tempo: &SegmentTempoMap) -> Env<'_> {
    Env {
        tempo,
        sample_rate: 48_000.0,
        root: None,
        playhead: TickTime(0),
        loop_range: None,
    }
}

impl Rig {
    fn click(&mut self, x: f64, y: f64) {
        self.arrange.press(&mut self.doc, Pointer::left(x, y));
        self.arrange.release(&mut self.doc, Pointer::left(x, y));
    }

    fn drag(&mut self, from: (f64, f64), to: (f64, f64), modify: impl Fn(&mut Pointer)) {
        let tempo = self.tempo.clone();
        let env = env(&tempo);
        let mk = |x, y| {
            let mut p = Pointer::left(x, y);
            modify(&mut p);
            p
        };
        self.arrange.press(&mut self.doc, mk(from.0, from.1));
        for i in 1..=4 {
            let f = f64::from(i) / 4.0;
            let p = mk(from.0 + (to.0 - from.0) * f, from.1 + (to.1 - from.1) * f);
            self.arrange.motion(&mut self.doc, &env, p);
        }
        self.arrange.release(&mut self.doc, mk(to.0, to.1));
    }
}

#[test]
fn expanded_tracks_grow_by_their_lanes() {
    let mut r = Rig::new();
    let a = &r.arrange;
    assert_eq!(a.row_height(0), 64.0);
    assert_eq!(a.row_y(1), 24.0 + 64.0);

    // Adding a lane shows the track's lanes.
    let vol = r.add_lane(0, AutomationTarget::Volume);
    assert!(r.arrange.expanded.contains(&r.tracks[0]));
    assert_eq!(r.arrange.row_height(0), 64.0 + 40.0);
    assert_eq!(r.arrange.row_y(1), 24.0 + 104.0);
    assert_eq!(r.arrange.content_height(r.doc.project()), 104.0 + 64.0);
    r.add_lane(0, AutomationTarget::Pan);
    assert_eq!(r.arrange.row_height(0), 64.0 + 80.0);

    // Hit testing: clips area, then each lane, then the next track.
    let p = r.doc.project();
    let x = r.x(BAR);
    assert!(matches!(r.arrange.hit(p, x, 24.0 + 30.0), Hit::Lane { .. }));
    match r.arrange.hit(p, x, 24.0 + 64.0 + 20.0) {
        Hit::Automation {
            lane, point: None, ..
        } => assert_eq!(lane, Some(vol)),
        h => panic!("{h:?}"),
    }
    match r.arrange.hit(p, x, 24.0 + 144.0 + 10.0) {
        Hit::Lane { row, .. } => assert_eq!(row, 1),
        h => panic!("{h:?}"),
    }

    // Collapsing hides them again; an expanded track without lanes keeps
    // one empty row to add lanes from.
    let project = r.doc.project().clone();
    r.arrange.set_expanded(&project, r.tracks[0], false);
    assert_eq!(r.arrange.row_height(0), 64.0);
    r.arrange.set_expanded(&project, r.tracks[1], true);
    assert_eq!(r.arrange.row_height(1), 104.0);
    match r.arrange.hit(&project, x, r.arrange.row_y(1) + 80.0) {
        Hit::Automation { lane: None, .. } => {}
        h => panic!("{h:?}"),
    }
}

#[test]
fn click_adds_points_and_drag_moves_them_in_one_undo_step() {
    let mut r = Rig::new();
    let lane = r.add_lane(0, AutomationTarget::Pan);
    // Pan runs -1 (bottom) to 1 (top).
    r.click(r.x(BAR), r.y(0, 0, 0.75));
    let pts = r.points(lane);
    assert_eq!(pts.len(), 1);
    assert_eq!(pts[0].tick, TickTime(BAR));
    assert!((pts[0].value - 0.5).abs() < 0.02, "{}", pts[0].value);
    assert!(r.doc.project().track(r.tracks[0]).unwrap().pan == 0.0);

    r.click(r.x(2 * BAR), r.y(0, 0, 0.25));
    assert_eq!(r.points(lane).len(), 2);
    assert!((r.points(lane)[1].value + 0.5).abs() < 0.02);

    // Drag the second point a beat later and to the top.
    let before = r.points(lane);
    r.drag(
        (r.x(2 * BAR), r.y(0, 0, 0.25)),
        (r.x(2 * BAR + BEAT), r.y(0, 0, 1.0)),
        |_| {},
    );
    let after = r.points(lane);
    assert_eq!(after[1].tick, TickTime(2 * BAR + BEAT));
    assert!((after[1].value - 1.0).abs() < 1e-9);
    // One undo puts it back, another removes the second point.
    r.doc.undo().unwrap();
    assert_eq!(r.points(lane), before);
    r.doc.undo().unwrap();
    assert_eq!(r.points(lane).len(), 1);
}

#[test]
fn dragging_from_an_empty_spot_adds_and_moves_one_point() {
    let mut r = Rig::new();
    let lane = r.add_lane(0, AutomationTarget::Volume);
    r.drag(
        (r.x(BAR), r.y(0, 0, 0.5)),
        (r.x(BAR + BEAT), r.y(0, 0, 1.0)),
        |_| {},
    );
    let pts = r.points(lane);
    assert_eq!(pts.len(), 1);
    assert_eq!(pts[0].tick, TickTime(BAR + BEAT));
    assert_eq!(pts[0].value, 6.0);
    // Adding and dragging are one step.
    r.doc.undo().unwrap();
    assert!(r.points(lane).is_empty());
    // Escape during the gesture undoes it too.
    let at = Pointer::left(r.x(BAR), r.y(0, 0, 0.5));
    r.arrange.press(&mut r.doc, at);
    assert_eq!(r.points(lane).len(), 1);
    r.arrange.cancel(&mut r.doc);
    assert!(r.points(lane).is_empty());
}

#[test]
fn points_snap_and_stay_between_their_neighbours() {
    let mut r = Rig::new();
    r.arrange.snap = Snap::Adaptive;
    let lane = r.add_lane(0, AutomationTarget::Pan);
    // A click slightly off the bar lands on it.
    r.click(r.x(BAR) + 3.0, r.y(0, 0, 0.5));
    assert_eq!(r.points(lane)[0].tick, TickTime(BAR));
    r.click(r.x(2 * BAR), r.y(0, 0, 0.5));
    r.click(r.x(3 * BAR), r.y(0, 0, 0.5));
    // Dragging the middle point past the last one stops just before it.
    // (Shift turns snapping off; it would also add to the selection, so
    // select the point alone first.)
    r.click(r.x(2 * BAR), r.y(0, 0, 0.5));
    r.drag(
        (r.x(2 * BAR), r.y(0, 0, 0.5)),
        (r.x(5 * BAR), r.y(0, 0, 0.5)),
        |p| p.shift = true,
    );
    let pts = r.points(lane);
    assert_eq!(pts.len(), 3);
    assert_eq!(pts[1].tick, TickTime(3 * BAR - 1));
    // And it can't go before the start of the song.
    r.click(r.x(BAR), r.y(0, 0, 0.5));
    r.drag(
        (r.x(BAR), r.y(0, 0, 0.5)),
        (r.x(-BAR), r.y(0, 0, 0.5)),
        |p| p.shift = true,
    );
    assert_eq!(r.points(lane)[0].tick, TickTime(0));
}

#[test]
fn selected_points_move_together_and_delete() {
    let mut r = Rig::new();
    let lane = r.add_lane(0, AutomationTarget::Pan);
    for bar in 1..=3 {
        r.click(r.x(bar * BAR), r.y(0, 0, 0.5));
    }
    // Ctrl-click adds the first point to the selection (the last click
    // selected the third).
    let ctrl = |x, y| Pointer {
        ctrl: true,
        ..Pointer::left(x, y)
    };
    let (x1, y1) = (r.x(BAR), r.y(0, 0, 0.5));
    r.arrange.press(&mut r.doc, ctrl(x1, y1));
    r.arrange.release(&mut r.doc, ctrl(x1, y1));
    assert_eq!(
        r.arrange.points.as_ref().unwrap().1,
        [0, 2].into_iter().collect()
    );
    r.drag(
        (r.x(3 * BAR), r.y(0, 0, 0.5)),
        (r.x(3 * BAR), r.y(0, 0, 0.75)),
        |_| {},
    );
    let pts = r.points(lane);
    assert!((pts[0].value - 0.5).abs() < 0.02);
    assert!(pts[1].value.abs() < 1e-9);
    assert!((pts[2].value - 0.5).abs() < 0.02);

    // Delete removes both, in one step.
    let o = r.arrange.delete_selection(&mut r.doc);
    assert!(o.document);
    assert_eq!(r.points(lane).len(), 1);
    assert_eq!(r.points(lane)[0].tick, TickTime(2 * BAR));
    r.doc.undo().unwrap();
    assert_eq!(r.points(lane).len(), 3);

    // A double click deletes one point.
    let at = Pointer::left(r.x(2 * BAR), r.y(0, 0, 0.5));
    r.arrange.double_click(&mut r.doc, at);
    assert_eq!(r.points(lane).len(), 2);
}

#[test]
fn shapes_and_curve_bending() {
    let mut r = Rig::new();
    let lane = r.add_lane(0, AutomationTarget::Pan);
    r.click(r.x(BAR), r.y(0, 0, 0.0));
    r.click(r.x(2 * BAR), r.y(0, 0, 1.0));
    // Select the first point and make it a step.
    r.click(r.x(BAR), r.y(0, 0, 0.0));
    r.arrange.set_point_shape(&mut r.doc, PointShape::Step);
    assert!(r.points(lane)[0].step);
    assert_eq!(
        r.arrange.selected_shape(r.doc.project()),
        Some(PointShape::Step)
    );
    r.arrange.set_point_shape(&mut r.doc, PointShape::SlowStart);
    let p = &r.points(lane)[0];
    assert!(!p.step && p.curve < 0.0);

    // Alt-dragging up over the rising segment makes it rise early.
    r.arrange.set_point_shape(&mut r.doc, PointShape::Linear);
    r.drag(
        (r.x(BAR + 2 * BEAT), r.y(0, 0, 0.5)),
        (r.x(BAR + 2 * BEAT), r.y(0, 0, 0.5) - 40.0),
        |p| p.alt = true,
    );
    let pts = r.points(lane);
    assert_eq!(pts.len(), 2, "alt-drag adds no point");
    assert!((pts[0].curve - 1.0).abs() < 1e-9, "{}", pts[0].curve);
    r.doc.undo().unwrap();
    assert_eq!(r.points(lane)[0].curve, 0.0);
}

#[test]
fn level_lanes_reach_silence_and_lanes_toggle_and_remove() {
    let mut r = Rig::new();
    let lane = r.add_lane(0, AutomationTarget::Volume);
    r.click(r.x(BAR), r.y(0, 0, 0.0));
    assert_eq!(r.points(lane)[0].value, SILENCE_DB);

    r.arrange.set_lane_enabled(&mut r.doc, lane, false);
    let p = r.doc.project();
    assert!(!p.track(r.tracks[0]).unwrap().automation[0].enabled);
    r.doc.undo().unwrap();
    assert!(r.doc.project().track(r.tracks[0]).unwrap().automation[0].enabled);

    r.arrange.remove_lane(&mut r.doc, lane);
    assert!(
        r.doc
            .project()
            .track(r.tracks[0])
            .unwrap()
            .automation
            .is_empty()
    );
    // Still expanded: one empty lane row.
    assert_eq!(r.arrange.row_height(0), 104.0);
    r.doc.undo().unwrap();
    assert_eq!(r.points(lane).len(), 1);
}

struct Catalog;

impl ParamCatalog for Catalog {
    fn automatable(&self, _: &Device) -> Vec<ParamId> {
        vec![0, 1]
    }
    fn scale(&self, _: &Device, param: ParamId) -> Option<LaneScale> {
        (param < 2).then_some(LaneScale::Range {
            min: 20.0,
            max: 20_000.0,
            log: true,
            steps: false,
        })
    }
    fn default_value(&self, _: &Device, _: ParamId) -> Option<f64> {
        Some(1000.0)
    }
}

#[test]
fn targets_list_what_has_no_lane_yet() {
    let mut r = Rig::new();
    let bus = Track::new(TrackKind::Bus, "Bus");
    let bus_id = bus.id;
    r.doc.execute(AddTrack::new(2, bus)).unwrap();
    let send = Send::new(bus_id);
    let send_id = send.id;
    r.doc
        .execute(AddSend {
            track: r.tracks[0],
            send,
        })
        .unwrap();
    let dev = Device::new(
        "EQ",
        DeviceSource::Builtin {
            kind: "daw.eq".into(),
        },
    );
    let dev_id = dev.id;
    r.doc
        .execute(AddDevice {
            track: r.tracks[0],
            index: 0,
            device: dev,
        })
        .unwrap();
    r.arrange.params = Some(Arc::new(Catalog));

    let track = r.doc.project().track(r.tracks[0]).unwrap().clone();
    let all = lane_targets(&track, r.arrange.params.as_ref());
    assert_eq!(all.len(), 5);
    assert_eq!(all[2], AutomationTarget::SendLevel { send: send_id });
    let eq = AutomationTarget::DeviceParam {
        device: dev_id,
        param: 1,
    };
    assert_eq!(all[4], eq);

    let lane = r.add_lane(0, eq);
    let track = r.doc.project().track(r.tracks[0]).unwrap().clone();
    assert_eq!(lane_targets(&track, r.arrange.params.as_ref()).len(), 4);
    // A log-scaled parameter lane, drawn at its default with no points.
    assert_eq!(r.arrange.static_value(&track, eq), 1000.0);
    r.click(r.x(BAR), r.y(0, 0, 0.5));
    let v = r.points(lane)[0].value;
    assert!((v - 632.46).abs() < 5.0, "{v}");
    // The master has no pan.
    let master = r.doc.project().master.clone();
    assert!(!lane_targets(&master, None).contains(&AutomationTarget::Pan));
}

#[test]
fn scene_draws_points_only_for_expanded_tracks() {
    let mut r = Rig::new();
    let lane = r.add_lane(0, AutomationTarget::Pan);
    r.click(r.x(BAR), r.y(0, 0, 0.5));
    r.click(r.x(2 * BAR), r.y(0, 0, 1.0));
    assert_eq!(r.points(lane).len(), 2);
    let point_squares = |s: &daw_arrange::Scene| {
        s.prims
            .iter()
            .filter(|p| matches!(p, Prim::RoundRect { r, .. } if r.w == 7.0 && r.h == 7.0))
            .count()
    };
    let project = r.doc.project().clone();
    let env_tempo = r.tempo.clone();
    let env = env(&env_tempo);
    let s = r.arrange.scene(&project, &env);
    assert_eq!(point_squares(&s), 2);
    r.arrange.set_expanded(&project, r.tracks[0], false);
    let s = r.arrange.scene(&project, &env);
    assert_eq!(point_squares(&s), 0);
}

#[test]
fn many_expanded_lanes_draw_quickly() {
    // 200 tracks with two lanes of 200 points each: a frame stays cheap.
    let mut doc = Document::new(Project::new("big"));
    for i in 0..200 {
        let mut t = Track::new(TrackKind::Audio, format!("T{i}"));
        for target in [AutomationTarget::Volume, AutomationTarget::Pan] {
            let mut l = daw_model::AutomationLane::new(target);
            l.points = (0..200)
                .map(|k| AutomationPoint::new(TickTime(k * BEAT), (k % 2) as f64 * 0.5))
                .collect();
            t.automation.push(l);
        }
        doc.execute(AddTrack::new(i, t)).unwrap();
    }
    let mut a = Arrange::new();
    for t in &doc.project().tracks {
        a.expanded.insert(t.id);
    }
    a.resize(doc.project(), 1920.0, 1080.0);
    let tempo = SegmentTempoMap::new(120.0);
    let env = Env {
        tempo: &tempo,
        sample_rate: 48_000.0,
        root: None,
        playhead: TickTime(0),
        loop_range: None,
    };
    let project = doc.project().clone();
    let start = std::time::Instant::now();
    for _ in 0..10 {
        a.scroll_y += 500.0;
        let _ = a.scene(&project, &env);
    }
    let per_frame = start.elapsed() / 10;
    // Generous for debug builds on slow CI; release is far below a frame.
    assert!(per_frame.as_millis() < 100, "{per_frame:?}");
}
