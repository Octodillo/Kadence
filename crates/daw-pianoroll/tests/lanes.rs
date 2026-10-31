//! Automation lanes under the piano roll, against a real document.

use daw_arrange::{Button, Env, PointShape, Pointer, Prim, Snap};
use daw_core::time::{PPQ, SegmentTempoMap, TickTime};
use daw_midi::NoteData;
use daw_midi::model::new_note;
use daw_model::commands::{AddClip, AddLane, AddTrack};
use daw_model::{
    AutomationLane, AutomationPoint, AutomationTarget, Clip, ClipContent, Document, LaneId,
    MidiClip, Project, Track, TrackKind,
};
use daw_pianoroll::{Area, PianoRoll};

const Q: i64 = PPQ;

struct Rig {
    doc: Document,
    pr: PianoRoll,
    tempo: SegmentTempoMap,
    pan: LaneId,
}

impl Rig {
    /// A clip with one note on a track with a pan lane and a volume lane,
    /// tick 0 at the keyboard's edge, snapping off.
    fn new() -> Self {
        let mut doc = Document::new(Project::new("t"));
        let track = Track::new(TrackKind::Instrument, "keys");
        let tid = track.id;
        doc.execute(AddTrack::new(0, track)).unwrap();
        let note = new_note(NoteData::new(0, Q, 60, 0.8));
        let clip = Clip::new(
            "part",
            TickTime(0),
            TickTime(4 * Q),
            ClipContent::Midi(MidiClip { notes: vec![note] }),
        );
        let cid = clip.id;
        doc.execute(AddClip::new(tid, clip)).unwrap();
        let pan = AutomationLane::new(AutomationTarget::Pan);
        let id = pan.id;
        doc.execute(AddLane {
            track: tid,
            lane: pan,
        })
        .unwrap();
        doc.execute(AddLane {
            track: tid,
            lane: AutomationLane::new(AutomationTarget::Volume),
        })
        .unwrap();
        doc.seal();
        let mut pr = PianoRoll::new();
        pr.snap = Snap::Off;
        pr.resize(800.0, 600.0);
        pr.open(doc.project(), cid);
        pr.sync(doc.project());
        pr.view.scroll_x = 0.0;
        Self {
            doc,
            pr,
            tempo: SegmentTempoMap::new(120.0),
            pan: id,
        }
    }

    fn press(&mut self, p: Pointer) {
        let tempo = self.tempo.clone();
        let env = Env {
            tempo: &tempo,
            sample_rate: 48_000.0,
            root: None,
            playhead: TickTime(0),
            loop_range: None,
        };
        self.pr.press(&mut self.doc, &env, p);
    }

    fn click(&mut self, x: f64, y: f64) {
        self.press(Pointer::left(x, y));
        self.pr.release(&mut self.doc, Pointer::left(x, y));
    }

    /// Where pan `value` at `tick` is in the pan lane.
    fn at(&self, tick: i64, value: f64) -> (f64, f64) {
        let f = self.pr.lane_frame(self.doc.project(), self.pan).unwrap();
        (f.tick_x(TickTime(tick)), f.value_y(value))
    }

    fn points(&self) -> Vec<AutomationPoint> {
        let p = self.doc.project();
        let (track, index) = p.lane_location(self.pan).unwrap();
        p.track(track).unwrap().automation[index].points.clone()
    }
}

#[test]
fn lanes_sit_under_the_velocity_lane_and_hide() {
    let mut r = Rig::new();
    let vel = r.pr.velocity_rect();
    let lanes = r.pr.lanes_rect();
    assert_eq!(lanes.y, vel.bottom());
    assert_eq!(lanes.h, 2.0 * r.pr.lane_height);
    assert_eq!(lanes.bottom(), r.pr.height);
    assert_eq!(r.pr.notes_rect().bottom(), vel.y);
    assert_eq!(r.pr.area(100.0, lanes.y + 5.0), Area::Lanes);
    assert_eq!(r.pr.area(10.0, lanes.y + 5.0), Area::Corner);
    assert_eq!(r.pr.area(100.0, vel.y + 5.0), Area::Velocity);

    r.pr.lanes_shown = false;
    assert_eq!(r.pr.lanes_height(), 0.0);
    assert_eq!(r.pr.velocity_rect().bottom(), r.pr.height);
    assert!(r.pr.lane_frame(r.doc.project(), r.pan).is_none());
}

#[test]
fn a_click_adds_a_point_where_the_lane_is_drawn_and_drags_in_one_step() {
    let mut r = Rig::new();
    // The lane's time starts at the keyboard's edge, as the notes' does.
    let (x, y) = r.at(Q, 0.5);
    assert_eq!(x, r.pr.keyboard_width + r.pr.view.tick_to_x(TickTime(Q)));
    r.press(Pointer::left(x, y));
    let (x2, y2) = r.at(2 * Q, -1.0);
    r.pr.motion(&mut r.doc, Pointer::left(x2, y2));
    r.pr.release(&mut r.doc, Pointer::left(x2, y2));
    let pts = r.points();
    assert_eq!(pts.len(), 1);
    assert_eq!(pts[0].tick, TickTime(2 * Q));
    assert_eq!(pts[0].value, -1.0);
    // Adding and dragging are one step.
    r.doc.undo().unwrap();
    assert!(r.points().is_empty());
}

#[test]
fn notes_and_points_are_never_selected_together() {
    let mut r = Rig::new();
    let (x, y) = r.at(Q, 0.0);
    // A lane press leaves the notes, so Delete takes the point.
    r.pr.select_all(r.doc.project());
    r.click(x, y);
    assert!(r.pr.selection.is_empty());
    assert!(r.pr.lanes.points_selected(r.doc.project()));
    r.pr.delete_selection(&mut r.doc);
    assert!(r.points().is_empty());
    assert_eq!(r.pr.notes(r.doc.project()).len(), 1);

    // Select All leaves the points, so Delete takes the notes.
    r.click(x, y);
    r.pr.select_all(r.doc.project());
    assert!(!r.pr.lanes.points_selected(r.doc.project()));
    r.pr.delete_selection(&mut r.doc);
    assert!(r.pr.notes(r.doc.project()).is_empty());
    assert_eq!(r.points().len(), 1);

    // So does a press among the notes.
    r.click(x, y);
    let notes = r.pr.notes_rect();
    r.click(notes.x + 400.0, notes.y + 10.0);
    assert!(!r.pr.lanes.points_selected(r.doc.project()));
}

#[test]
fn hiding_the_lanes_drops_their_point_selection() {
    let mut r = Rig::new();
    let (x, y) = r.at(Q, 0.0);
    r.click(x, y);
    r.pr.lanes_shown = false;
    r.pr.sync(r.doc.project());
    assert!(!r.pr.lanes.points_selected(r.doc.project()));
    r.pr.delete_selection(&mut r.doc);
    assert_eq!(r.points().len(), 1);
}

#[test]
fn lanes_share_their_part_of_a_short_roll() {
    let mut r = Rig::new();
    // 100 px under the ruler: the two lanes share half of it.
    r.pr.resize(800.0, r.pr.ruler_height + 100.0);
    assert_eq!(r.pr.lane_count(), 2);
    assert!((r.pr.lanes_height() - 50.0).abs() < 1e-9);
    assert!((r.pr.lane_row_height() - 25.0).abs() < 1e-9);
    // Every lane is in reach, the last one too.
    assert!((r.pr.lane_rect(1).bottom() - r.pr.height).abs() < 1e-9);
    // The velocity lane has no say in it.
    r.pr.velocity_height = 10.0;
    assert!((r.pr.lanes_height() - 50.0).abs() < 1e-9);
}

#[test]
fn ending_drags_before_another_edit_stops_a_lane_drag() {
    let mut r = Rig::new();
    let (x, y) = r.at(Q, 0.0);
    r.press(Pointer::left(x, y));
    r.pr.end_drag(&mut r.doc);
    r.doc.undo().unwrap();
    assert!(r.points().is_empty());
    // Motion afterwards doesn't bring the point back.
    let (x2, y2) = r.at(2 * Q, 1.0);
    r.pr.motion(&mut r.doc, Pointer::left(x2, y2));
    assert!(r.points().is_empty());
}

#[test]
fn double_click_deletes_a_point_and_the_menu_sets_shapes() {
    let mut r = Rig::new();
    let (x, y) = r.at(Q, 0.0);
    r.click(x, y);
    let (x2, y2) = r.at(3 * Q, 1.0);
    r.click(x2, y2);
    assert_eq!(r.points().len(), 2);

    let mut right = Pointer::left(x, y);
    right.button = Button::Right;
    r.press(right);
    r.pr.release(&mut r.doc, right);
    assert_eq!(r.pr.take_lane_menu_request(), Some(true));
    assert_eq!(r.pr.take_lane_menu_request(), None);
    r.pr.set_point_shape(&mut r.doc, PointShape::Step);
    assert!(r.points()[0].step);
    assert_eq!(r.pr.point_shape(r.doc.project()), Some(PointShape::Step));

    r.pr.double_click(&mut r.doc, Pointer::left(x2, y2));
    assert_eq!(r.points().len(), 1);
}

#[test]
fn escape_undoes_a_lane_gesture_and_a_press_ends_a_lost_one() {
    let mut r = Rig::new();
    let (x, y) = r.at(Q, 0.0);
    r.press(Pointer::left(x, y));
    assert_eq!(r.points().len(), 1);
    r.pr.cancel(&mut r.doc);
    assert!(r.points().is_empty());

    // A drag whose release never comes, then a press among the notes:
    // motion afterwards no longer moves the point.
    r.press(Pointer::left(x, y));
    let notes = r.pr.notes_rect();
    r.press(Pointer::left(notes.x + 400.0, notes.y + 10.0));
    let before = r.points();
    let (x2, y2) = r.at(3 * Q, 1.0);
    r.pr.motion(&mut r.doc, Pointer::left(x2, y2));
    assert_eq!(r.points(), before);
}

#[test]
fn the_scene_draws_lane_points() {
    let mut r = Rig::new();
    let (x, y) = r.at(Q, 0.0);
    r.click(x, y);
    let tempo = r.tempo.clone();
    let env = Env {
        tempo: &tempo,
        sample_rate: 48_000.0,
        root: None,
        playhead: TickTime(0),
        loop_range: None,
    };
    let s = r.pr.scene(r.doc.project(), &env);
    let lanes = r.pr.lanes_rect();
    let points = s
        .prims
        .iter()
        .filter(|p| matches!(p, Prim::RoundRect { r: b, .. } if lanes.contains(b.x + b.w / 2.0, b.y + b.h / 2.0)))
        .count();
    assert_eq!(points, 1);
}
