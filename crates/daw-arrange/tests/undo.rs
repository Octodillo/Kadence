//! A drag is one undo step that restores the project exactly, a drag that
//! changes nothing is no step, and a cancelled one leaves nothing behind.

use daw_arrange::commands::TrimClip;
use daw_arrange::{Arrange, Env, Pointer, Snap};
use daw_core::time::{PPQ, SampleTime, SegmentTempoMap, TickTime};
use daw_model::commands::{AddClip, AddTrack, SetTrack, TrackField};
use daw_model::{
    AudioClip, Clip, ClipContent, ClipId, Document, Project, Track, TrackId, TrackKind,
};

const BAR: i64 = 4 * PPQ;

struct Rig {
    doc: Document,
    arrange: Arrange,
    tempo: SegmentTempoMap,
    tracks: Vec<TrackId>,
}

impl Rig {
    /// Two audio tracks; 32 px per beat, 64 px tracks under a 24 px ruler.
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
        arrange.resize(doc.project(), 4000.0, 400.0);
        Self {
            doc,
            arrange,
            tempo: SegmentTempoMap::new(120.0),
            tracks,
        }
    }

    fn clip(&mut self, row: usize, start: i64, len: i64) -> ClipId {
        let c = Clip::new(
            "a",
            TickTime(start),
            TickTime(len),
            ClipContent::Audio(AudioClip::new("missing.wav", SampleTime(0))),
        );
        let id = c.id;
        self.doc.execute(AddClip::new(self.tracks[row], c)).unwrap();
        self.doc.seal();
        id
    }

    fn x(&self, tick: i64) -> f64 {
        self.arrange.view.tick_to_x(TickTime(tick))
    }

    fn y(&self, row: usize) -> f64 {
        self.arrange.row_y(row) + self.arrange.track_height / 2.0
    }

    fn press(&mut self, x: f64, y: f64) {
        self.arrange.press(&mut self.doc, Pointer::left(x, y));
    }

    fn motion(&mut self, x: f64, y: f64) {
        let env = Env {
            tempo: &self.tempo,
            sample_rate: 48_000.0,
            root: None,
            playhead: TickTime(0),
            loop_range: None,
        };
        self.arrange
            .motion(&mut self.doc, &env, Pointer::left(x, y));
    }

    fn release(&mut self, x: f64, y: f64) {
        self.arrange.release(&mut self.doc, Pointer::left(x, y));
    }

    fn start(&self, id: ClipId) -> i64 {
        self.doc.project().clip(id).unwrap().start.0
    }

    /// The ids of a track's clips, in list order (later ones are on top).
    fn order(&self, row: usize) -> Vec<ClipId> {
        self.doc.project().tracks[row]
            .clips
            .iter()
            .map(|c| c.id)
            .collect()
    }

    /// Three undoable renames, undone again: three steps to redo.
    fn leave_three_redo_steps(&mut self) {
        for n in ["x", "y", "z"] {
            self.doc
                .execute(SetTrack::new(self.tracks[0], TrackField::Name(n.into())))
                .unwrap();
            self.doc.seal();
        }
        for _ in 0..3 {
            self.doc.undo().unwrap();
        }
        assert_eq!(self.doc.history().redo_len(), 3);
    }
}

/// Issue #204 item 1.
#[test]
fn a_slip_on_a_clip_body_is_not_an_edit() {
    let mut r = Rig::new();
    let id = r.clip(0, BAR, BAR);
    r.leave_three_redo_steps();
    let steps = r.doc.history().undo_len();
    let position = r.doc.history().position();
    let revision = r.doc.revision();

    // 4 px is past the drag threshold but snaps back to where the clip is.
    let (x, y) = (r.x(BAR + BAR / 2), r.y(0));
    r.press(x, y);
    r.motion(x + 4.0, y);
    r.release(x + 4.0, y);

    assert_eq!(r.start(id), BAR);
    assert_eq!(r.doc.history().undo_len(), steps);
    assert_eq!(r.doc.history().redo_len(), 3, "redo history was wiped");
    assert_eq!(r.doc.history().position(), position, "marked modified");
    assert_eq!(r.doc.revision(), revision, "the engine was resynced");
}

/// Issue #204 item 1, for the edges.
#[test]
fn a_slip_on_a_clip_edge_is_not_an_edit() {
    let mut r = Rig::new();
    let id = r.clip(0, BAR, BAR);
    r.leave_three_redo_steps();
    let steps = r.doc.history().undo_len();
    for edge in [BAR, 2 * BAR] {
        let inward = if edge == BAR { 1.0 } else { -1.0 };
        let (x, y) = (r.x(edge) + inward, r.y(0));
        r.press(x, y);
        r.motion(x + 4.0, y);
        r.release(x + 4.0, y);
    }
    let c = r.doc.project().clip(id).unwrap();
    assert_eq!((c.start.0, c.length.0), (BAR, BAR));
    assert_eq!(r.doc.history().undo_len(), steps);
    assert_eq!(r.doc.history().redo_len(), 3);
}

#[test]
fn dragging_away_and_back_leaves_no_step() {
    let mut r = Rig::new();
    let id = r.clip(0, BAR, BAR);
    let steps = r.doc.history().undo_len();
    let position = r.doc.history().position();
    let (x, y) = (r.x(BAR + BAR / 2), r.y(0));
    r.press(x, y);
    r.motion(x + 128.0, y);
    assert_eq!(r.start(id), 2 * BAR);
    assert_eq!(r.doc.history().undo_len(), steps + 1);
    r.motion(x + 1.0, y);
    assert_eq!(r.start(id), BAR);
    r.release(x + 1.0, y);
    assert_eq!(r.doc.history().undo_len(), steps);
    assert_eq!(r.doc.history().position(), position);
}

/// Issue #204 item 8.
#[test]
fn a_cancelled_drag_cannot_be_redone() {
    let mut r = Rig::new();
    let id = r.clip(0, BAR, BAR);
    let steps = r.doc.history().undo_len();
    let position = r.doc.history().position();
    let (x, y) = (r.x(BAR + BAR / 2), r.y(0));
    r.press(x, y);
    r.motion(x + 128.0, y);
    r.motion(x + 256.0, y);
    assert_eq!(r.start(id), 3 * BAR);
    let o = r.arrange.cancel(&mut r.doc);
    assert!(o.document && o.error.is_none());
    assert_eq!(r.start(id), BAR);
    assert!(!r.doc.history().can_redo(), "Redo re-applies the drag");
    assert_eq!(r.doc.history().undo_len(), steps);
    assert_eq!(r.doc.history().position(), position);
}

/// Something else edits the document while a button is held (a take ends):
/// the drag neither takes that edit back nor is lost itself.
#[test]
fn a_drag_leaves_edits_made_under_it_alone() {
    let mut r = Rig::new();
    let id = r.clip(0, BAR, BAR);
    let (x, y) = (r.x(BAR + BAR / 2), r.y(0));
    r.press(x, y);
    r.motion(x + 128.0, y);
    r.doc
        .execute(SetTrack::new(r.tracks[1], TrackField::Name("take".into())))
        .unwrap();
    r.motion(x + 256.0, y);
    r.arrange.cancel(&mut r.doc);
    assert_eq!(r.doc.project().tracks[1].name, "take");
    // The first part of the drag is a step of its own, below the rename.
    assert_eq!(r.start(id), 2 * BAR);
    r.doc.undo().unwrap();
    assert_eq!(r.doc.project().tracks[1].name, "T1");
    r.doc.undo().unwrap();
    assert_eq!(r.start(id), BAR);
}

/// Issue #204 item 7: which of two clips with the same start is on top is
/// part of the project, and undo restores it.
#[test]
fn undoing_a_move_restores_the_order_of_clips_that_start_together() {
    let mut r = Rig::new();
    // B lies over the first bar of A.
    let a = r.clip(0, BAR, 2 * BAR);
    let b = r.clip(0, BAR, BAR);
    let before = r.doc.project().clone();
    assert_eq!(r.order(0), [a, b]);

    // Grab A where only it reaches and move it a bar, in a few moves.
    let (x, y) = (r.x(2 * BAR + BAR / 2), r.y(0));
    r.press(x, y);
    for dx in [40.0, 90.0, 128.0] {
        r.motion(x + dx, y);
    }
    r.release(x + 128.0, y);
    assert_eq!(r.start(a), 2 * BAR);
    assert_eq!(r.order(0), [b, a]);

    r.doc.undo().unwrap();
    assert_eq!(r.order(0), [a, b], "A is now painted over B");
    assert_eq!(*r.doc.project(), before);
}

#[test]
fn undoing_a_trim_restores_the_order_of_clips_that_start_together() {
    let mut doc = Document::new(Project::new("t"));
    let t = Track::new(TrackKind::Audio, "T");
    let tid = t.id;
    doc.execute(AddTrack::new(0, t)).unwrap();
    let mk = || {
        Clip::new(
            "a",
            TickTime(BAR),
            TickTime(BAR),
            ClipContent::Audio(AudioClip::new("x.wav", SampleTime(0))),
        )
    };
    let (a, b) = (mk(), mk());
    let (ia, ib) = (a.id, b.id);
    doc.execute(AddClip::new(tid, a)).unwrap();
    doc.execute(AddClip::new(tid, b)).unwrap();
    doc.seal();
    let before = doc.project().clone();
    let order = |d: &Document| -> Vec<ClipId> {
        d.project().tracks[0].clips.iter().map(|c| c.id).collect()
    };

    // The end: the start stays, so A stays below B.
    doc.execute(TrimClip::new(ia, TickTime(BAR), TickTime(BAR / 2), None))
        .unwrap();
    doc.seal();
    assert_eq!(order(&doc), [ia, ib]);
    doc.undo().unwrap();
    assert_eq!(*doc.project(), before);

    // The start: A moves (on top of what starts where it lands) and back.
    doc.execute(TrimClip::new(
        ia,
        TickTime(BAR + PPQ),
        TickTime(BAR - PPQ),
        Some(SampleTime(100)),
    ))
    .unwrap();
    doc.seal();
    assert_eq!(order(&doc), [ib, ia]);
    doc.undo().unwrap();
    assert_eq!(*doc.project(), before);
    doc.redo().unwrap();
    let c = doc.project().clip(ia).unwrap();
    assert_eq!((c.start.0, c.length.0), (BAR + PPQ, BAR - PPQ));
}

/// Drags of several clips over several pointer moves, between tracks and
/// onto each other, undo to exactly the project before, order included.
#[test]
fn multi_clip_drags_undo_exactly() {
    let mut seed: u64 = 12345;
    let mut rnd = |n: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n
    };
    let mut edited = 0;
    for iter in 0..500 {
        let mut r = Rig::new();
        r.arrange.snap = Snap::Off;
        // Clips on few distinct starts, so many start together.
        let n = 2 + rnd(5) as usize;
        let ids: Vec<ClipId> = (0..n)
            .map(|_| r.clip(rnd(2) as usize, (2 + rnd(4) as i64) * BAR, BAR))
            .collect();
        let before = r.doc.project().clone();
        let steps = r.doc.history().undo_len();

        r.arrange.selection = ids.iter().copied().filter(|_| rnd(2) == 0).collect();
        let Some(&lead) = r.arrange.selection.iter().next() else {
            continue;
        };
        let row = (0..2).find(|&i| r.order(i).contains(&lead)).unwrap();
        // Shift keeps the selection when the press lands on a clip lying
        // over the lead; the press may grab that one instead, which is as
        // good a drag.
        let (x, y) = (r.x(r.start(lead) + BAR / 2), r.y(row));
        r.arrange.press(
            &mut r.doc,
            Pointer {
                shift: true,
                ..Pointer::left(x, y)
            },
        );
        let (mut tx, mut ty) = (x, y);
        for _ in 0..1 + rnd(4) {
            tx = x + rnd(600) as f64 - 300.0;
            ty = r.y(rnd(2) as usize);
            r.motion(tx, ty);
        }
        r.release(tx, ty);

        let len = r.doc.history().undo_len();
        assert!(len == steps || len == steps + 1, "iter {iter}: {len}");
        if len == steps {
            assert_eq!(*r.doc.project(), before, "iter {iter}: no step, but moved");
            continue;
        }
        edited += 1;
        let after = r.doc.project().clone();
        assert_ne!(after, before, "iter {iter}: a step that does nothing");
        r.doc.undo().unwrap();
        assert_eq!(*r.doc.project(), before, "iter {iter}: undo");
        r.doc.redo().unwrap();
        assert_eq!(*r.doc.project(), after, "iter {iter}: redo");
    }
    assert!(edited > 200, "only {edited} drags edited anything");
}
