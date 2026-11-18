//! Piano-roll gestures and actions against a real document.

use daw_arrange::{Button, Env, Pointer, Prim, Snap};
use daw_core::time::{PPQ, SegmentTempoMap, TickTime};
use daw_midi::edit::Quantize;
use daw_midi::grid::Grid;
use daw_midi::model::{compile_track, new_note};
use daw_midi::{EditNote, NoteData};
use daw_model::commands::{AddClip, AddTrack};
use daw_model::{Clip, ClipContent, ClipId, Document, MidiClip, Note, Project, Track, TrackKind};
use daw_pianoroll::{Accidentals, Area, PianoRoll, note_name};

const Q: i64 = PPQ;

struct Rig {
    doc: Document,
    pr: PianoRoll,
    clip: ClipId,
    tempo: SegmentTempoMap,
}

impl Rig {
    /// A clip from beat 1 (tick 960) four beats long, 96 px per beat,
    /// sixteenth-note snapping.
    fn new(mut notes: Vec<Note>) -> Self {
        notes.sort_by_key(|n| (n.start, n.key, n.id));
        let mut doc = Document::new(Project::new("t"));
        let track = Track::new(TrackKind::Instrument, "keys");
        let tid = track.id;
        doc.execute(AddTrack::new(0, track)).unwrap();
        let clip = Clip::new(
            "part",
            TickTime(Q),
            TickTime(4 * Q),
            ClipContent::Midi(MidiClip { notes }),
        );
        let cid = clip.id;
        doc.execute(AddClip::new(tid, clip)).unwrap();
        doc.seal();
        let mut pr = PianoRoll::new();
        pr.snap = Snap::Fixed(Grid::note(16));
        pr.resize(800.0, 600.0);
        pr.open(doc.project(), cid);
        // Put tick 0 at the left edge and key 72 at the top for easy maths,
        // at the default row height whatever opening fitted.
        pr.key_height = PianoRoll::new().key_height;
        pr.view.scroll_x = 0.0;
        pr.scroll_y = (127.0 - 72.0) * pr.key_height;
        Self {
            doc,
            pr,
            clip: cid,
            tempo: SegmentTempoMap::new(120.0),
        }
    }

    fn env(&self) -> Env<'_> {
        Env {
            tempo: &self.tempo,
            sample_rate: 48_000.0,
            root: None,
            playhead: TickTime(0),
            loop_range: None,
        }
    }

    /// Item x for a clip-relative tick.
    fn x(&self, t: i64) -> f64 {
        self.pr.keyboard_width + self.pr.view.tick_to_x(TickTime(Q + t))
    }

    /// Item y for the middle of a key's row.
    fn y(&self, key: u8) -> f64 {
        self.pr.ruler_height + (127.0 - key as f64) * self.pr.key_height - self.pr.scroll_y
            + self.pr.key_height / 2.0
    }

    fn notes(&self) -> Vec<NoteData> {
        self.pr
            .notes(self.doc.project())
            .iter()
            .map(|n| n.data())
            .collect()
    }

    fn press(&mut self, x: f64, y: f64) {
        let env_tempo = self.tempo.clone();
        let env = Env {
            tempo: &env_tempo,
            sample_rate: 48_000.0,
            root: None,
            playhead: TickTime(0),
            loop_range: None,
        };
        self.pr.press(&mut self.doc, &env, Pointer::left(x, y));
    }

    fn press_with(&mut self, p: Pointer) {
        let env_tempo = self.tempo.clone();
        let env = Env {
            tempo: &env_tempo,
            sample_rate: 48_000.0,
            root: None,
            playhead: TickTime(0),
            loop_range: None,
        };
        self.pr.press(&mut self.doc, &env, p);
    }

    fn drag(&mut self, from: (f64, f64), to: (f64, f64)) {
        self.press(from.0, from.1);
        let steps = 5;
        for i in 1..=steps {
            let f = i as f64 / steps as f64;
            let p = Pointer::left(from.0 + (to.0 - from.0) * f, from.1 + (to.1 - from.1) * f);
            self.pr.motion(&mut self.doc, p);
        }
        self.pr.release(&mut self.doc, Pointer::left(to.0, to.1));
    }
}

fn n(start: i64, len: i64, key: u8) -> Note {
    new_note(NoteData::new(start, len, key, 0.8))
}

#[test]
fn layout_areas() {
    let r = Rig::new(vec![]);
    assert_eq!(r.pr.area(10.0, 100.0), Area::Keyboard);
    assert_eq!(r.pr.area(100.0, 10.0), Area::Ruler);
    assert_eq!(r.pr.area(100.0, 100.0), Area::Notes);
    assert_eq!(r.pr.area(100.0, 590.0), Area::Velocity);
    assert_eq!(r.pr.area(10.0, 10.0), Area::Corner);
    assert_eq!(note_name(60, Accidentals::Sharps), "C4");
    assert_eq!(note_name(61, Accidentals::Sharps), "C♯4");
    assert_eq!(note_name(61, Accidentals::Flats), "D♭4");
    assert_eq!(note_name(60, Accidentals::Flats), "C4");
    assert_eq!(note_name(0, Accidentals::Flats), "C-1");
    assert_eq!(note_name(127, Accidentals::Flats), "G9");
}

#[test]
fn a_finger_scrolls_from_the_empty_grid_and_edits_everywhere_else() {
    let rig = Rig::new(vec![n(0, Q, 60)]);
    let project = rig.doc.project();
    let pans = |x, y| rig.pr.touch_pans(project, x, y);
    assert!(pans(rig.x(2 * Q), rig.y(60)), "beside the note");
    assert!(pans(rig.x(Q / 2), rig.y(64)), "above the note");
    assert!(!pans(rig.x(Q / 2), rig.y(60)), "on the note");
    assert!(!pans(rig.x(Q), rig.pr.ruler_height / 2.0), "on the ruler");
    assert!(!pans(rig.pr.keyboard_width / 2.0, rig.y(60)), "on the keys");
    assert!(!pans(rig.x(Q), 599.0), "on the velocity lane");
}

#[test]
fn double_click_adds_and_deletes_notes() {
    let mut r = Rig::new(vec![]);
    // A quarter-beat and a bit into the clip, on key 64.
    let (x, y) = (r.x(Q / 4 + 30), r.y(64));
    r.pr.double_click(&mut r.doc, Pointer::left(x, y));
    assert_eq!(r.notes(), [NoteData::new(Q / 4, Q / 4, 64, 0.8)]);
    assert_eq!(r.pr.selection.len(), 1);
    r.pr.double_click(&mut r.doc, Pointer::left(x, y));
    assert!(r.notes().is_empty());
    r.doc.undo().unwrap();
    assert_eq!(r.notes().len(), 1);
    // Outside the clip is refused.
    let p = Pointer::left(r.x(5 * Q), y);
    let out = r.pr.double_click(&mut r.doc, p);
    assert!(out.error.is_some());
}

#[test]
fn a_new_note_sounds_while_the_double_click_is_held() {
    let mut r = Rig::new(vec![]);
    let (x, y) = (r.x(Q / 4 + 30), r.y(64));
    r.press(x, y);
    assert_eq!(r.pr.audition(), None, "empty space is silent");
    r.pr.double_click(&mut r.doc, Pointer::left(x, y));
    let sounding = r.pr.audition().expect("the new note sounds");
    assert_eq!((sounding.key, sounding.velocity), (64, 0.8));
    // A hand that is not quite still neither resizes it nor silences it.
    r.pr.motion(&mut r.doc, Pointer::left(x + 2.0, y + 1.0));
    assert_eq!(r.notes(), [NoteData::new(Q / 4, Q / 4, 64, 0.8)]);
    assert!(r.pr.audition().is_some());
    let out = r.pr.release(&mut r.doc, Pointer::left(x + 2.0, y + 1.0));
    assert_eq!(r.pr.audition(), None);
    assert!(out.view, "the key is redrawn as released");
    // Nothing follows the pointer once it is up.
    r.pr.motion(&mut r.doc, Pointer::left(x + 200.0, y));
    assert_eq!(r.notes(), [NoteData::new(Q / 4, Q / 4, 64, 0.8)]);
}

#[test]
fn dragging_a_held_new_note_sets_its_length_in_the_same_undo_step() {
    let mut r = Rig::new(vec![]);
    let steps = r.doc.history().undo_len();
    let (x, y) = (r.x(Q / 4 + 30), r.y(64));
    r.press(x, y);
    r.pr.double_click(&mut r.doc, Pointer::left(x, y));
    // The end follows the pointer, on the grid, whatever row it is over.
    for (to, length) in [(Q + 20, Q - Q / 4), (2 * Q - 20, 2 * Q - Q / 4)] {
        let p = Pointer::left(r.x(to), r.y(70));
        r.pr.motion(&mut r.doc, p);
        assert_eq!(r.notes(), [NoteData::new(Q / 4, length, 64, 0.8)]);
        assert_eq!(r.pr.audition().map(|a| a.key), Some(64), "still sounding");
    }
    // Back past its start: never shorter than a grid step.
    let back = Pointer::left(r.x(0), y);
    r.pr.motion(&mut r.doc, back);
    assert_eq!(r.notes(), [NoteData::new(Q / 4, Q / 4, 64, 0.8)]);
    let end = Pointer::left(r.x(Q + Q / 2), y);
    r.pr.motion(&mut r.doc, end);
    r.pr.release(&mut r.doc, end);
    let drawn = [NoteData::new(Q / 4, Q + Q / 4, 64, 0.8)];
    assert_eq!(r.notes(), drawn);
    assert_eq!(r.pr.audition(), None);

    assert_eq!(r.doc.history().undo_len(), steps + 1);
    r.doc.undo().unwrap();
    assert!(r.notes().is_empty());
    r.doc.redo().unwrap();
    assert_eq!(r.notes(), drawn, "redo brings the note back at its length");
}

#[test]
fn alt_sets_a_held_new_notes_length_off_the_grid() {
    let mut r = Rig::new(vec![]);
    let (x, y) = (r.x(Q / 4 + 30), r.y(64));
    r.press(x, y);
    r.pr.double_click(&mut r.doc, Pointer::left(x, y));
    let mut p = Pointer::left(r.x(Q + 20), y);
    p.alt = true;
    r.pr.motion(&mut r.doc, p);
    r.pr.release(&mut r.doc, p);
    let end = r.notes()[0].end().0;
    assert!((end - (Q + 20)).abs() <= 1, "{end}");
}

#[test]
fn a_wobble_inside_a_held_new_note_keeps_its_length() {
    let mut r = Rig::new(vec![]);
    let (x, y) = (r.x(Q / 4 + 30), r.y(64));
    // Off the grid the note starts under the pointer, a sixteenth long.
    let mut p = Pointer::left(x, y);
    p.alt = true;
    r.press_with(p);
    r.pr.double_click(&mut r.doc, p);
    let drawn = r.notes();
    assert_eq!(drawn[0].length.0, Q / 4);
    for (dx, dy) in [(0.0, 5.0), (4.0, -3.0), (-6.0, 0.0), (10.0, 40.0)] {
        let mut wobble = Pointer::left(x + dx, y + dy);
        wobble.alt = true;
        r.pr.motion(&mut r.doc, wobble);
        assert_eq!(r.notes(), drawn, "moved by {dx}, {dy}");
    }
    assert!(r.pr.audition().is_some());
}

#[test]
fn escape_ends_a_held_new_note_and_closes_its_undo_step() {
    let mut r = Rig::new(vec![]);
    let steps = r.doc.history().undo_len();
    let (x, y) = (r.x(Q / 4 + 30), r.y(64));
    r.press(x, y);
    r.pr.double_click(&mut r.doc, Pointer::left(x, y));
    r.pr.cancel(&mut r.doc);
    assert_eq!(r.pr.audition(), None);
    // The pointer is still down, but no longer sets the length.
    let far = Pointer::left(r.x(2 * Q), y);
    r.pr.motion(&mut r.doc, far);
    r.pr.release(&mut r.doc, far);
    assert_eq!(r.notes(), [NoteData::new(Q / 4, Q / 4, 64, 0.8)]);
    // Resizing it afterwards is a step of its own.
    r.drag((r.x(Q / 2 - 2), y), (r.x(Q), y));
    assert_eq!(r.notes(), [NoteData::new(Q / 4, Q - Q / 4, 64, 0.8)]);
    assert_eq!(r.doc.history().undo_len(), steps + 2);
    r.doc.undo().unwrap();
    assert_eq!(r.notes(), [NoteData::new(Q / 4, Q / 4, 64, 0.8)]);
}

#[test]
fn releasing_a_note_that_sounded_redraws_the_keyboard() {
    let mut r = Rig::new(vec![n(0, Q, 60)]);
    let (x, y) = (r.x(Q / 2), r.y(60));
    r.press(x, y);
    assert!(r.pr.audition().is_some());
    let out = r.pr.release(&mut r.doc, Pointer::left(x, y));
    assert!(out.view);
    assert_eq!(r.pr.audition(), None);
    // Nothing sounded on empty space, so nothing to redraw.
    let (x, y) = (r.x(3 * Q), r.y(70));
    r.press(x, y);
    assert!(!r.pr.release(&mut r.doc, Pointer::left(x, y)).view);
}

#[test]
fn a_press_after_a_lost_release_leaves_the_new_note_alone() {
    let mut r = Rig::new(vec![]);
    let (x, y) = (r.x(Q / 4 + 30), r.y(64));
    r.pr.double_click(&mut r.doc, Pointer::left(x, y));
    // Elsewhere on empty space: a rubber band, not a resize.
    r.drag((r.x(2 * Q), r.y(70)), (r.x(3 * Q), r.y(68)));
    assert_eq!(r.notes(), [NoteData::new(Q / 4, Q / 4, 64, 0.8)]);
    assert_eq!(r.pr.audition(), None);
}

#[test]
fn note_names_follow_the_accidentals() {
    // A B flat a beat long: wide enough for its name.
    let mut r = Rig::new(vec![n(0, Q, 70)]);
    let names = |r: &Rig| -> Vec<String> {
        let scene = r.pr.scene(r.doc.project(), &r.env());
        scene.texts().map(String::from).collect()
    };
    assert!(names(&r).iter().any(|t| t == "A♯4"), "{:?}", names(&r));
    r.pr.accidentals = Accidentals::Flats;
    let flat = names(&r);
    assert!(flat.iter().any(|t| t == "B♭4"), "{flat:?}");
    assert!(!flat.iter().any(|t| t.contains('♯')), "{flat:?}");
}

#[test]
fn dragging_moves_the_selection_snapped_in_one_undo_step() {
    let mut r = Rig::new(vec![n(0, Q, 60), n(0, Q, 64)]);
    r.pr.select_all(r.doc.project());
    // Grab the C in the middle and drag about a beat and a bit right, two keys up.
    let from = (r.x(Q / 2), r.y(60));
    let to = (r.x(Q / 2 + Q + 20), r.y(62));
    r.drag(from, to);
    let got: Vec<(i64, u8)> = r.notes().iter().map(|d| (d.start.0, d.key)).collect();
    assert_eq!(got, [(Q, 62), (Q, 66)]);
    r.doc.undo().unwrap();
    let got: Vec<(i64, u8)> = r.notes().iter().map(|d| (d.start.0, d.key)).collect();
    assert_eq!(got, [(0, 60), (0, 64)]);
}

#[test]
fn alt_drag_does_not_snap() {
    let mut r = Rig::new(vec![n(0, Q, 60)]);
    let from = (r.x(Q / 2), r.y(60));
    r.press(from.0, from.1);
    let mut p = Pointer::left(from.0 + 10.0, from.1);
    p.alt = true;
    r.pr.motion(&mut r.doc, p);
    r.pr.release(&mut r.doc, p);
    assert_eq!(r.notes()[0].start.0, 100);
}

#[test]
fn dragging_the_end_resizes() {
    let mut r = Rig::new(vec![n(0, Q, 60)]);
    let from = (r.x(Q) - 2.0, r.y(60));
    let to = (r.x(2 * Q - 10), r.y(60));
    r.drag(from, to);
    assert_eq!(r.notes()[0].length.0, 2 * Q);
}

#[test]
fn rubber_band_selects() {
    let mut r = Rig::new(vec![n(0, Q / 2, 60), n(Q, Q / 2, 62), n(3 * Q, Q / 2, 70)]);
    r.drag((r.x(-Q / 4), r.y(59)), (r.x(Q + 10), r.y(63)));
    assert_eq!(r.pr.selection.len(), 2);
    // Shift-band adds; a plain click on empty space clears.
    let mut p = Pointer::left(r.x(3 * Q - 10), r.y(71));
    p.shift = true;
    r.press_with(p);
    let mut m = Pointer::left(r.x(3 * Q + 50), r.y(69));
    m.shift = true;
    r.pr.motion(&mut r.doc, m);
    r.pr.release(&mut r.doc, m);
    assert_eq!(r.pr.selection.len(), 3);
    r.press(r.x(2 * Q), r.y(50));
    let p = Pointer::left(r.x(2 * Q), r.y(50));
    r.pr.release(&mut r.doc, p);
    assert!(r.pr.selection.is_empty());
}

#[test]
fn ctrl_click_toggles_shift_click_adds_and_keys_select_rows() {
    let mut r = Rig::new(vec![n(0, Q, 60), n(Q, Q, 60), n(0, Q, 64)]);
    r.press(r.x(Q / 2), r.y(60));
    assert_eq!(r.pr.selection.len(), 1);
    // Ctrl toggles, like the arrange view.
    let mut p = Pointer::left(r.x(Q / 2), r.y(64));
    p.ctrl = true;
    r.press_with(p);
    assert_eq!(r.pr.selection.len(), 2);
    r.press_with(p);
    assert_eq!(r.pr.selection.len(), 1);
    // Shift adds and never removes.
    p.ctrl = false;
    p.shift = true;
    r.press_with(p);
    assert_eq!(r.pr.selection.len(), 2);
    r.press_with(p);
    assert_eq!(r.pr.selection.len(), 2);
    // A plain click (no drag) on one of several selected notes selects
    // only it, once released.
    let (x, y) = (r.x(Q / 2), r.y(64));
    r.press(x, y);
    assert_eq!(r.pr.selection.len(), 2, "kept for a drag");
    r.pr.release(&mut r.doc, Pointer::left(x, y));
    let keys: Vec<u8> =
        r.pr.selected_data(r.doc.project())
            .iter()
            .map(|d| d.key)
            .collect();
    assert_eq!(keys, [64]);
    // Click the C4 key on the keyboard strip.
    r.press(10.0, r.y(60));
    let keys: Vec<u8> =
        r.pr.selected_data(r.doc.project())
            .iter()
            .map(|d| d.key)
            .collect();
    assert_eq!(keys, [60, 60]);
    let q = Pointer::left(10.0, r.y(60));
    r.pr.release(&mut r.doc, q);
    // Shift+click a key adds its row; Ctrl+click toggles it.
    let mut k = Pointer::left(10.0, r.y(64));
    k.shift = true;
    r.press_with(k);
    assert_eq!(r.pr.selection.len(), 3);
    k.shift = false;
    k.ctrl = true;
    r.press_with(k);
    assert_eq!(r.pr.selection.len(), 2);
    r.press_with(k);
    assert_eq!(r.pr.selection.len(), 3);
}

#[test]
fn ctrl_band_adds_to_the_selection() {
    let mut r = Rig::new(vec![n(0, Q / 2, 60), n(3 * Q, Q / 2, 70)]);
    r.press(r.x(Q / 4), r.y(60));
    let q = Pointer::left(r.x(Q / 4), r.y(60));
    r.pr.release(&mut r.doc, q);
    assert_eq!(r.pr.selection.len(), 1);
    let mut p = Pointer::left(r.x(3 * Q - 10), r.y(71));
    p.ctrl = true;
    r.press_with(p);
    let mut m = Pointer::left(r.x(3 * Q + 50), r.y(69));
    m.ctrl = true;
    r.pr.motion(&mut r.doc, m);
    r.pr.release(&mut r.doc, m);
    assert_eq!(r.pr.selection.len(), 2);
}

#[test]
fn pressing_a_key_auditions_it_and_sliding_plays_each_key() {
    let mut r = Rig::new(vec![]);
    assert_eq!(r.pr.audition(), None);
    r.press(10.0, r.y(60));
    let a = r.pr.audition().expect("sounding");
    assert_eq!((a.key, a.channel), (60, 0));
    assert!((f64::from(a.velocity) - r.pr.new_velocity).abs() < 1e-6);
    // The pressed key is drawn in the highlight colour.
    let scene = r.pr.scene(r.doc.project(), &r.env());
    let pal = r.pr.palette;
    assert!(scene.prims.iter().any(|p| matches!(p,
        Prim::Rect { r: rect, fill, .. } if *fill == pal.highlight && rect.x == 0.0
    )));
    let q = Pointer::left(10.0, r.y(62));
    let out = r.pr.motion(&mut r.doc, q);
    assert!(out.view);
    assert_eq!(r.pr.audition().map(|a| a.key), Some(62));
    let q = Pointer::left(10.0, r.y(62));
    r.pr.release(&mut r.doc, q);
    assert_eq!(r.pr.audition(), None);
}

#[test]
fn pressing_and_dragging_a_note_auditions_its_pitch() {
    let mut note = n(0, Q, 60);
    note.velocity = 0.5;
    note.channel = 2;
    let mut r = Rig::new(vec![note, n(0, Q, 64)]);
    r.pr.select_all(r.doc.project());
    let from = (r.x(Q / 2), r.y(60));
    r.press(from.0, from.1);
    let a = r.pr.audition().expect("sounding");
    assert_eq!((a.key, a.channel), (60, 2));
    assert!((a.velocity - 0.5).abs() < 1e-6);
    // Moving in time keeps the same note; moving up follows the grabbed note.
    r.pr.motion(&mut r.doc, Pointer::left(from.0 + 40.0, from.1));
    assert_eq!(r.pr.audition(), Some(a));
    let q = Pointer::left(from.0 + 40.0, r.y(63));
    r.pr.motion(&mut r.doc, q);
    assert_eq!(r.pr.audition().map(|a| a.key), Some(63));
    let q = Pointer::left(from.0 + 40.0, r.y(63));
    r.pr.release(&mut r.doc, q);
    assert_eq!(r.pr.audition(), None);
    // Empty space, the velocity lane and the ruler stay silent.
    r.press(r.x(3 * Q), r.y(40));
    assert_eq!(r.pr.audition(), None);
    r.press(r.x(Q / 2), 590.0);
    assert_eq!(r.pr.audition(), None);
    r.press(r.x(Q / 2), 10.0);
    assert_eq!(r.pr.audition(), None);
    // Escape ends it too.
    r.press(10.0, r.y(60));
    r.pr.cancel(&mut r.doc);
    assert_eq!(r.pr.audition(), None);
}

#[test]
fn velocity_lane_draws_velocities() {
    let mut r = Rig::new(vec![n(0, Q / 2, 60), n(Q, Q / 2, 62), n(2 * Q, Q / 2, 64)]);
    let vel = r.pr.velocity_rect();
    let top = vel.y + 4.0;
    let bottom = vel.bottom();
    r.drag((r.x(-10), bottom), (r.x(2 * Q + 10), top));
    let v: Vec<f64> = r.notes().iter().map(|d| d.velocity).collect();
    assert!(v[0] < 0.1 && v[2] > 0.9, "{v:?}");
    assert!(v[0] < v[1] && v[1] < v[2]);
    // One undo step restores all three.
    r.doc.undo().unwrap();
    assert!(r.notes().iter().all(|d| d.velocity == 0.8));
}

#[test]
fn dragging_near_a_stem_sets_its_velocity() {
    let mut r = Rig::new(vec![n(0, Q / 2, 60), n(Q, Q / 2, 62)]);
    let vel = r.pr.velocity_rect();
    let lane_top = vel.y + 4.0;
    let lane_h = vel.h - 4.0;
    // A few pixels to the side of the stem, straight down, as a finger
    // or a not-quite-steady mouse would.
    let x = r.x(0) + 7.0;
    r.drag((x, lane_top), (x + 1.0, lane_top + lane_h * 0.75));
    let v: Vec<f64> = r.notes().iter().map(|d| d.velocity).collect();
    assert!((v[0] - 0.25).abs() < 0.02, "{v:?}");
    assert_eq!(v[1], 0.8);
    // Too far away to the side: nothing changes.
    let x = r.x(0) + 30.0;
    r.drag((x, lane_top), (x, lane_top + lane_h * 0.5));
    assert!((r.notes()[0].velocity - 0.25).abs() < 0.02);
}

#[test]
fn grabbing_a_chord_stem_moves_the_whole_chord() {
    let mut r = Rig::new(vec![n(0, Q / 2, 60), n(0, Q / 2, 64), n(Q, Q / 2, 62)]);
    let vel = r.pr.velocity_rect();
    let lane_top = vel.y + 4.0;
    let lane_h = vel.h - 4.0;
    let x = r.x(0) + 4.0;
    r.drag((x, lane_top), (x, lane_top + lane_h * 0.5));
    let v: Vec<f64> = r.notes().iter().map(|d| d.velocity).collect();
    assert!(
        (v[0] - 0.5).abs() < 0.02 && (v[1] - 0.5).abs() < 0.02,
        "{v:?}"
    );
    assert_eq!(v[2], 0.8);
}

#[test]
fn double_click_delete_stops_the_audition() {
    let mut r = Rig::new(vec![n(0, Q, 60)]);
    let (x, y) = (r.x(Q / 2), r.y(60));
    r.press(x, y);
    assert!(r.pr.audition().is_some());
    r.pr.double_click(&mut r.doc, Pointer::left(x, y));
    assert!(r.notes().is_empty());
    assert_eq!(r.pr.audition(), None);
}

#[test]
fn ruler_click_locates_snapped() {
    let mut r = Rig::new(vec![]);
    let env_tempo = r.tempo.clone();
    let env = Env {
        tempo: &env_tempo,
        sample_rate: 48_000.0,
        root: None,
        playhead: TickTime(0),
        loop_range: None,
    };
    let x = r.pr.keyboard_width + r.pr.view.tick_to_x(TickTime(Q + 10));
    let out = r.pr.press(&mut r.doc, &env, Pointer::left(x, 5.0));
    assert_eq!(out.locate, Some(TickTime(Q)));
}

#[test]
fn right_click_asks_for_a_menu_and_selects() {
    let mut r = Rig::new(vec![n(0, Q, 60)]);
    let mut p = Pointer::left(r.x(Q / 2), r.y(60));
    p.button = Button::Right;
    r.press_with(p);
    assert!(r.pr.take_menu_request());
    assert!(!r.pr.take_menu_request());
    assert_eq!(r.pr.selection.len(), 1);
}

#[test]
fn actions() {
    let mut r = Rig::new(vec![n(10, Q, 60), n(Q + 30, Q / 2, 64)]);
    r.pr.select_all(r.doc.project());
    r.pr.quantize(&mut r.doc, Quantize::default());
    let starts: Vec<i64> = r.notes().iter().map(|d| d.start.0).collect();
    assert_eq!(starts, [0, Q]);
    r.pr.transpose(&mut r.doc, 12);
    assert_eq!(r.notes()[0].key, 72);
    r.pr.nudge(&mut r.doc, 1);
    assert_eq!(r.notes()[0].start.0, Q / 4);
    r.pr.nudge(&mut r.doc, -1);
    r.pr.set_velocity(&mut r.doc, 0.5);
    assert!(r.notes().iter().all(|d| d.velocity == 0.5));
    r.pr.legato(&mut r.doc);
    assert_eq!(r.notes()[0].length.0, Q);
    r.pr.duplicate(&mut r.doc);
    assert_eq!(r.notes().len(), 4);
    assert_eq!(r.pr.selection.len(), 2);
    r.pr.delete_selection(&mut r.doc);
    assert_eq!(r.notes().len(), 2);
    r.pr.select_all(r.doc.project());
    r.pr.split_at(&mut r.doc, TickTime(Q + Q / 2));
    assert_eq!(r.notes().len(), 3);
    r.pr.select_all(r.doc.project());
    r.pr.glue(&mut r.doc);
    assert_eq!(r.notes().len(), 2);
    // Each action is its own undo step: undo them all.
    for _ in 0..9 {
        r.doc.undo().unwrap();
    }
    assert_eq!(
        r.notes(),
        [
            NoteData::new(10, Q, 60, 0.8),
            NoteData::new(Q + 30, Q / 2, 64, 0.8)
        ]
    );
}

#[test]
fn scene_draws_notes_ruler_and_keyboard() {
    let mut r = Rig::new(vec![n(0, Q, 60), n(Q, Q, 64), n(0, Q, 10)]);
    r.pr.select_all(r.doc.project());
    let scene = r.pr.scene(r.doc.project(), &r.env());
    let notes = scene
        .prims
        .iter()
        .filter(|p| matches!(p, Prim::RoundRect { .. }))
        .count();
    // Key 10 is scrolled out of view.
    assert_eq!(notes, 2);
    let texts: Vec<&str> = scene.texts().collect();
    assert!(texts.contains(&"1") && texts.contains(&"2"), "{texts:?}");
    assert!(texts.contains(&"C4"));
    let pushes = scene
        .prims
        .iter()
        .filter(|p| matches!(p, Prim::PushClip(_)))
        .count();
    let pops = scene
        .prims
        .iter()
        .filter(|p| matches!(p, Prim::PopClip))
        .count();
    assert_eq!(pushes, pops);
}

#[test]
fn wheel_scrolls_and_zooms() {
    let mut r = Rig::new(vec![]);
    let y0 = r.pr.scroll_y;
    r.pr.wheel(Pointer::left(300.0, 300.0), 0.0, -120.0);
    assert_eq!(r.pr.scroll_y, y0 + 120.0);
    let z = r.pr.view.px_per_beat;
    let mut p = Pointer::left(300.0, 300.0);
    p.ctrl = true;
    r.pr.wheel(p, 0.0, 240.0);
    assert_eq!(r.pr.view.px_per_beat, 2.0 * z);
    p.shift = true;
    let kh = r.pr.key_height;
    r.pr.wheel(p, 0.0, 240.0);
    assert_eq!(r.pr.key_height, 2.0 * kh);
    // Scroll stays inside the keyboard.
    r.pr.wheel(Pointer::left(300.0, 300.0), 0.0, 1e9);
    assert_eq!(r.pr.scroll_y, 0.0);
}

#[test]
fn deleted_clip_closes_the_editor() {
    let mut r = Rig::new(vec![n(0, Q, 60)]);
    assert!(r.pr.has_clip(r.doc.project()));
    r.doc
        .execute(daw_model::commands::RemoveClip::new(r.clip))
        .unwrap();
    assert!(!r.pr.has_clip(r.doc.project()));
    assert!(r.pr.notes(r.doc.project()).is_empty());
    let _ = r.pr.scene(r.doc.project(), &r.env());
}

#[test]
fn glue_then_delete_removes_the_glued_notes() {
    let mut r = Rig::new(vec![n(0, Q, 60), n(Q, Q, 60), n(0, Q, 64)]);
    r.pr.select_all(r.doc.project());
    r.pr.glue(&mut r.doc);
    assert_eq!(r.notes().len(), 2);
    assert_eq!(r.pr.selection.len(), 2);
    let out = r.pr.delete_selection(&mut r.doc);
    assert!(out.error.is_none(), "{:?}", out.error);
    assert!(r.notes().is_empty());
}

#[test]
fn undo_then_delete_ignores_notes_that_are_gone() {
    let mut r = Rig::new(vec![n(0, Q, 60)]);
    r.pr.select_all(r.doc.project());
    r.pr.duplicate(&mut r.doc);
    r.doc.undo().unwrap();
    // Only the copies were selected, and they are gone.
    let out = r.pr.delete_selection(&mut r.doc);
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(r.notes().len(), 1);
    r.pr.sync(r.doc.project());
    assert!(r.pr.selection.is_empty());
}

#[test]
fn clicking_the_drawn_stem_top_keeps_the_velocity() {
    let mut r = Rig::new(vec![n(0, Q, 60)]);
    let vel = r.pr.velocity_rect();
    // Where draw.rs paints the top of a 0.8 stem.
    let top = vel.y + 4.0 + (1.0 - 0.8) * (vel.h - 4.0);
    let x = r.x(0);
    r.press(x, top);
    r.pr.release(&mut r.doc, Pointer::left(x, top));
    assert!((r.notes()[0].velocity - 0.8).abs() < 1e-6);
}

#[test]
fn opening_a_clip_shows_all_its_notes() {
    let notes = vec![n(0, Q, 48), n(Q, Q, 60), n(2 * Q, Q, 84)];
    let r = Rig::new(notes.clone());
    let mut pr = PianoRoll::new();
    // Opened before the item has a size, as the Edit page does.
    pr.open(r.doc.project(), r.clip);
    pr.fit_if_pending(r.doc.project());
    pr.resize(800.0, 400.0);
    pr.fit_if_pending(r.doc.project());
    let visible = pr.visible_keys();
    for key in [48, 60, 84] {
        assert!(visible.contains(&key), "{key} not in {visible:?}");
    }
}

#[test]
fn double_click_before_the_clip_is_refused() {
    let mut r = Rig::new(vec![]);
    // Half a beat before the clip start, in the dimmed part.
    let p = Pointer::left(r.x(-Q / 2), r.y(64));
    let out = r.pr.double_click(&mut r.doc, p);
    assert!(out.error.is_some());
    assert!(r.notes().is_empty());
    assert!(r.pr.selection.is_empty());
}

#[test]
fn a_velocity_edit_never_silences_a_note() {
    let mut r = Rig::new(vec![n(0, Q / 2, 60), n(Q, Q / 2, 62)]);
    let played = |r: &Rig| {
        let track = &r.doc.project().tracks[0];
        compile_track(track, &r.tempo, 48_000.0).len()
    };
    // A stem dragged to the very bottom of the lane, and past it.
    let vel = r.pr.velocity_rect();
    r.drag((r.x(0), vel.y + 10.0), (r.x(0), vel.bottom() + 20.0));
    assert!(r.notes()[0].velocity < 0.01);
    assert_eq!(played(&r), 2);
    r.pr.select_all(r.doc.project());
    r.pr.set_velocity(&mut r.doc, 0.0);
    assert_eq!(played(&r), 2);
    r.pr.scale_velocity(&mut r.doc, 0.0);
    assert_eq!(played(&r), 2);
    for seed in 0..50 {
        r.pr.humanize(&mut r.doc, seed);
        assert_eq!(played(&r), 2, "seed {seed}");
    }
}

#[test]
fn notes_sit_on_the_grid_at_any_scroll_and_zoom() {
    // Fully zoomed in, a tick is about two pixels wide: scrolled by a
    // fraction of a tick, notes must still start on their grid line.
    let mut r = Rig::new(vec![n(Q, Q, 60)]);
    r.pr.view.px_per_beat = 2000.0;
    r.pr.view.scroll_x = 3700.9;
    let scene = r.pr.scene(r.doc.project(), &r.env());
    let x = r.x(Q);
    let drawn = scene.prims.iter().any(|p| match p {
        Prim::RoundRect { r, .. } => (r.x - x).abs() < 1e-9 && (r.w - 2000.0).abs() < 1e-9,
        _ => false,
    });
    assert!(drawn, "no note drawn at {x}");
    // The pointer hits the note from its first pixel on, and not before.
    r.press(x + 0.1, r.y(60));
    assert_eq!(r.pr.selection.len(), 1);
    r.press(x - 0.1, r.y(60));
    assert!(r.pr.selection.is_empty());
}
