//! Drives the arrange view with pointer events and checks the document.

use std::time::Instant;

use daw_arrange::{Arrange, Button, Env, Hit, Pointer, Prim, Snap, Zone};
use daw_core::time::{PPQ, SampleTime, SegmentTempoMap, TickTime};
use daw_model::commands::{AddClip, AddTrack};
use daw_model::{
    AudioClip, Clip, ClipContent, ClipId, Document, MidiClip, Note, Project, Track, TrackId,
    TrackKind,
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
    /// Two audio tracks and an instrument track; 32 px per beat, so a bar is
    /// 128 px; 64 px tracks under a 24 px ruler.
    fn new() -> Self {
        let mut doc = Document::new(Project::new("t"));
        let mut tracks = Vec::new();
        for (i, kind) in [TrackKind::Audio, TrackKind::Audio, TrackKind::Instrument]
            .into_iter()
            .enumerate()
        {
            let t = Track::new(kind, format!("T{i}"));
            tracks.push(t.id);
            doc.execute(AddTrack::new(i, t)).unwrap();
        }
        doc.seal();
        let mut arrange = Arrange::new();
        arrange.resize(doc.project(), 1000.0, 400.0);
        Self {
            doc,
            arrange,
            tempo: SegmentTempoMap::new(120.0),
            tracks,
        }
    }

    fn audio(&mut self, row: usize, start: i64, len: i64, offset: i64) -> ClipId {
        let c = Clip::new(
            "audio",
            TickTime(start),
            TickTime(len),
            ClipContent::Audio(AudioClip {
                source: "missing.wav".into(),
                offset: SampleTime(offset),
                gain_db: 0.0,
            }),
        );
        let id = c.id;
        self.doc.execute(AddClip::new(self.tracks[row], c)).unwrap();
        self.doc.seal();
        id
    }

    fn midi(&mut self, start: i64, len: i64, notes: &[(i64, u8)]) -> ClipId {
        let mut m = MidiClip::default();
        for &(at, key) in notes {
            m.notes.push(Note::new(TickTime(at), TickTime(BEAT), key, 0.8));
        }
        let c = Clip::new("midi", TickTime(start), TickTime(len), ClipContent::Midi(m));
        let id = c.id;
        self.doc.execute(AddClip::new(self.tracks[2], c)).unwrap();
        self.doc.seal();
        id
    }

    fn x(&self, tick: i64) -> f64 {
        self.arrange.view.tick_to_x(TickTime(tick))
    }

    fn y(&self, row: usize) -> f64 {
        self.arrange.row_y(row) + self.arrange.track_height / 2.0
    }

    /// Press at `from`, move through a few points to `to`, release.
    fn drag(&mut self, from: (f64, f64), to: (f64, f64), shift: bool) {
        let tempo = self.tempo.clone();
        let env = Env {
            tempo: &tempo,
            sample_rate: 48_000.0,
            root: None,
            playhead: TickTime(0),
            loop_range: None,
        };
        let mk = |x, y| Pointer {
            shift,
            ..Pointer::left(x, y)
        };
        self.arrange.press(&mut self.doc, mk(from.0, from.1));
        for i in 1..=4 {
            let f = f64::from(i) / 4.0;
            let p = mk(from.0 + (to.0 - from.0) * f, from.1 + (to.1 - from.1) * f);
            self.arrange.motion(&mut self.doc, &env, p);
        }
        self.arrange.release(&mut self.doc, mk(to.0, to.1));
    }

    fn clip(&self, id: ClipId) -> (TrackId, Clip) {
        let p = self.doc.project();
        let (t, _) = p.clip_location(id).unwrap();
        (t, (**p.clip(id).unwrap()).clone())
    }
}

#[test]
fn hit_testing_finds_edges_and_body() {
    let mut r = Rig::new();
    let id = r.audio(0, BAR, BAR, 0);
    let p = r.doc.project();
    let y = r.y(0);
    let zone = |x| match r.arrange.hit(p, x, y) {
        Hit::Clip { clip, zone, .. } if clip == id => Some(zone),
        _ => None,
    };
    assert_eq!(zone(r.x(BAR) + 1.0), Some(Zone::Start));
    assert_eq!(zone(r.x(BAR + BAR / 2)), Some(Zone::Body));
    assert_eq!(zone(r.x(2 * BAR) - 1.0), Some(Zone::End));
    assert_eq!(zone(r.x(2 * BAR) + 1.0), None);
    assert!(matches!(r.arrange.hit(p, 10.0, 5.0), Hit::Ruler(_)));
    assert!(matches!(r.arrange.hit(p, 10.0, 399.0), Hit::Empty(_)));
}

#[test]
fn moving_snaps_and_undoes_in_one_step() {
    let mut r = Rig::new();
    let id = r.audio(0, BAR, BAR, 0);
    let before = r.doc.project().clone();
    let y = r.y(0);
    // Grab the middle and drag right by a beat and a bit.
    let from = r.x(BAR + BAR / 2);
    r.drag((from, y), (from + 32.0 + 3.0, y), false);
    let (_, c) = r.clip(id);
    // At 32 px per beat the adaptive grid is eighths; 35 px is just over a beat.
    assert_eq!(c.start, TickTime(BAR + BEAT));
    r.doc.undo().unwrap();
    assert_eq!(*r.doc.project(), before, "one undo restores the drag");
}

#[test]
fn shift_turns_snapping_off() {
    let mut r = Rig::new();
    let id = r.audio(0, BAR, BAR, 0);
    let y = r.y(0);
    let from = r.x(BAR + BAR / 2);
    r.drag((from, y), (from + 37.0, y), true);
    let (_, c) = r.clip(id);
    assert_eq!(c.start.0, BAR + (37.0 * PPQ as f64 / 32.0).round() as i64);
}

#[test]
fn clips_move_between_tracks_of_the_same_kind_only() {
    let mut r = Rig::new();
    let id = r.audio(0, 0, BAR, 0);
    let x = r.x(BAR / 2);
    let (y0, y1, y2) = (r.y(0), r.y(1), r.y(2));
    r.drag((x, y0), (x, y1), false);
    assert_eq!(r.clip(id).0, r.tracks[1]);
    // The instrument track doesn't take audio: the clip stays where it was.
    r.drag((x, y1), (x, y2), false);
    assert_eq!(r.clip(id).0, r.tracks[1]);
}

#[test]
fn trimming_audio_keeps_the_sound_in_place() {
    let mut r = Rig::new();
    let id = r.audio(0, BAR, BAR, 48_000);
    let y = r.y(0);
    // Pull the left edge right by one beat: start, length and offset move.
    r.drag((r.x(BAR) + 1.0, y), (r.x(BAR + BEAT) + 1.0, y), false);
    let (_, c) = r.clip(id);
    assert_eq!(c.start, TickTime(BAR + BEAT));
    assert_eq!(c.length, TickTime(BAR - BEAT));
    let ClipContent::Audio(a) = &c.content else {
        unreachable!()
    };
    // One beat at 120 bpm is 24000 samples.
    assert_eq!(a.offset, SampleTime(48_000 + 24_000));

    // Pull it far left: it stops where the file starts (offset 0).
    r.drag((r.x(BAR + BEAT) + 1.0, y), (r.x(0) + 1.0, y), false);
    let (_, c) = r.clip(id);
    let ClipContent::Audio(a) = &c.content else {
        unreachable!()
    };
    assert_eq!(a.offset, SampleTime(0));
    assert_eq!(c.start, TickTime(BAR - 2 * BEAT));

    // The right edge.
    let end = c.start.0 + c.length.0;
    r.drag((r.x(end) - 1.0, y), (r.x(end + BAR) - 1.0, y), false);
    assert_eq!(r.clip(id).1.length.0, c.length.0 + BAR);
}

#[test]
fn trimming_midi_never_cuts_notes() {
    let mut r = Rig::new();
    let id = r.midi(0, 2 * BAR, &[(BAR, 60), (BAR + BEAT, 64)]);
    let y = r.y(2);
    r.drag((r.x(0) + 1.0, y), (r.x(BAR + 2 * BEAT) + 1.0, y), false);
    let (_, c) = r.clip(id);
    // Stopped at the first note, which kept its place on the timeline.
    assert_eq!(c.start, TickTime(BAR));
    let ClipContent::Midi(m) = &c.content else {
        unreachable!()
    };
    assert_eq!(m.notes[0].start, TickTime(0));
    assert_eq!(m.notes[1].start, TickTime(BEAT));
    r.doc.undo().unwrap();
    let (_, c) = r.clip(id);
    let ClipContent::Midi(m) = &c.content else {
        unreachable!()
    };
    assert_eq!((c.start, m.notes[0].start), (TickTime(0), TickTime(BAR)));
}

#[test]
fn rubber_band_selects_and_delete_removes() {
    let mut r = Rig::new();
    let a = r.audio(0, 0, BAR, 0);
    let b = r.audio(1, 2 * BAR, BAR, 0);
    let c = r.audio(1, 6 * BAR, BAR, 0);
    // From an empty spot in track 0 to past `b`.
    r.drag((r.x(BAR + BEAT), r.y(0)), (r.x(3 * BAR - BEAT), r.y(1)), false);
    assert_eq!(
        r.arrange.selection,
        [b].into_iter().collect(),
        "only b intersects"
    );
    r.arrange.delete_selection(&mut r.doc);
    assert!(r.doc.project().clip(b).is_none());
    assert!(r.doc.project().clip(a).is_some() && r.doc.project().clip(c).is_some());
}

#[test]
fn split_and_duplicate() {
    let mut r = Rig::new();
    let a = r.audio(0, 0, 2 * BAR, 1000);
    let m = r.midi(0, 2 * BAR, &[(0, 60), (BAR + BEAT, 62)]);
    let before = r.doc.project().clone();
    r.arrange.selection = [a, m].into_iter().collect();
    let tempo = r.tempo.clone();
    let env = Env {
        tempo: &tempo,
        sample_rate: 48_000.0,
        root: None,
        playhead: TickTime(0),
        loop_range: None,
    };
    let out = r.arrange.split_at(&mut r.doc, &env, TickTime(BAR));
    assert!(out.document, "{out:?}");
    let p = r.doc.project();
    assert_eq!(p.tracks[0].clips.len(), 2);
    let right = &p.tracks[0].clips[1];
    assert_eq!((right.start, right.length), (TickTime(BAR), TickTime(BAR)));
    let ClipContent::Audio(au) = &right.content else {
        unreachable!()
    };
    assert_eq!(au.offset, SampleTime(1000 + 4 * 24_000));
    let midi = &p.tracks[2].clips;
    assert_eq!(midi.len(), 2);
    let notes = |c: &Clip| match &c.content {
        ClipContent::Midi(m) => m.notes.iter().map(|n| n.start.0).collect::<Vec<_>>(),
        _ => vec![],
    };
    assert_eq!(notes(&midi[0]), vec![0]);
    assert_eq!(notes(&midi[1]), vec![BEAT]);
    r.doc.undo().unwrap();
    assert_eq!(*r.doc.project(), before);

    r.arrange.selection = [a].into_iter().collect();
    r.arrange.duplicate_selection(&mut r.doc);
    let p = r.doc.project();
    assert_eq!(p.tracks[0].clips.len(), 2);
    assert_eq!(p.tracks[0].clips[1].start, TickTime(2 * BAR));
    assert!(!r.arrange.selection.contains(&a));
}

#[test]
fn double_click_on_instrument_lane_makes_a_bar_long_clip() {
    let mut r = Rig::new();
    let (x, y) = (r.x(BAR + BEAT), r.y(2));
    let out = r
        .arrange
        .double_click(&mut r.doc, Pointer::left(x, y));
    assert!(out.document);
    let c = &r.doc.project().tracks[2].clips[0];
    assert_eq!((c.start, c.length), (TickTime(BAR), TickTime(BAR)));
    // Not on an audio track.
    let y0 = r.y(0);
    assert!(!r.arrange.double_click(&mut r.doc, Pointer::left(x, y0)).document);
}

#[test]
fn ruler_locates_and_shift_drag_sets_loop() {
    let mut r = Rig::new();
    let env_tempo = r.tempo.clone();
    let env = Env {
        tempo: &env_tempo,
        sample_rate: 48_000.0,
        root: None,
        playhead: TickTime(0),
        loop_range: None,
    };
    let (x1, x3) = (r.x(BAR), r.x(3 * BAR));
    let out = r.arrange.press(&mut r.doc, Pointer::left(x1, 5.0));
    assert_eq!(out.locate, Some(TickTime(BAR)));
    r.arrange.release(&mut r.doc, Pointer::left(x1, 5.0));
    let shift = |x| Pointer {
        shift: true,
        ..Pointer::left(x, 5.0)
    };
    r.arrange.press(&mut r.doc, shift(x1 + 2.0));
    let out = r.arrange.motion(&mut r.doc, &env, shift(x3 - 2.0));
    assert_eq!(out.set_loop, Some(Some((TickTime(BAR), TickTime(3 * BAR)))));
}

#[test]
fn right_click_asks_for_a_menu() {
    let mut r = Rig::new();
    let id = r.audio(0, 0, BAR, 0);
    let p = Pointer {
        button: Button::Right,
        ..Pointer::left(r.x(BAR / 2), r.y(0))
    };
    let out = r.arrange.press(&mut r.doc, p);
    assert!(matches!(out.context_menu, Some(Hit::Clip { clip, .. }) if clip == id));
    assert!(r.arrange.selection.contains(&id));
}

#[test]
fn escape_cancels_a_drag() {
    let mut r = Rig::new();
    let id = r.audio(0, BAR, BAR, 0);
    let y = r.y(0);
    let x = r.x(BAR + BAR / 2);
    let tempo = r.tempo.clone();
    let env = Env {
        tempo: &tempo,
        sample_rate: 48_000.0,
        root: None,
        playhead: TickTime(0),
        loop_range: None,
    };
    r.arrange.press(&mut r.doc, Pointer::left(x, y));
    r.arrange.motion(&mut r.doc, &env, Pointer::left(x + 200.0, y));
    assert_ne!(r.clip(id).1.start, TickTime(BAR));
    r.arrange.cancel(&mut r.doc);
    assert_eq!(r.clip(id).1.start, TickTime(BAR));
}

#[test]
fn zoom_and_scroll_stay_in_range() {
    let mut r = Rig::new();
    let p = r.doc.project().clone();
    let ctrl = Pointer {
        ctrl: true,
        ..Pointer::left(500.0, 100.0)
    };
    let before = r.arrange.view.x_to_tick(500.0);
    r.arrange.wheel(&p, ctrl, 0.0, 240.0);
    assert_eq!(r.arrange.view.px_per_beat, 64.0);
    assert!((r.arrange.view.x_to_tick(500.0).0 - before.0).abs() <= 1);
    // Three tracks fit in the view, so there is nothing to scroll.
    r.arrange.wheel(&p, Pointer::left(0.0, 100.0), 0.0, -500.0);
    assert_eq!(r.arrange.scroll_y, 0.0);
    r.arrange.snap = Snap::Off;
    assert_eq!(r.arrange.drop_tick(&p, r.x(BEAT) + 3.0).0, BEAT + 45);
}

/// The plan's budget: 200 tracks of 50 clips must redraw fast enough to
/// scroll at 60 fps. Building the display list is the Rust share of that.
#[test]
fn big_project_scene_is_cheap() {
    let mut doc = Document::new(Project::new("big"));
    for i in 0..200 {
        let mut t = Track::new(TrackKind::Instrument, format!("Track {i}"));
        for j in 0..50 {
            let mut m = MidiClip::default();
            for k in 0..16 {
                m.notes
                    .push(Note::new(TickTime(k * BEAT / 4), TickTime(BEAT / 4), 60 + (k % 12) as u8, 0.7));
            }
            t.clips.push(std::sync::Arc::new(Clip::new(
                format!("c{j}"),
                TickTime(j * BAR),
                TickTime(BAR),
                ClipContent::Midi(m),
            )));
        }
        doc.execute(AddTrack::new(i, t)).unwrap();
    }
    let mut a = Arrange::new();
    a.resize(doc.project(), 1280.0, 800.0);
    a.scroll_y = 3000.0;
    a.view.scroll_x = 2_000.0;
    let tempo = SegmentTempoMap::new(120.0);
    let env = Env {
        tempo: &tempo,
        sample_rate: 48_000.0,
        root: None,
        playhead: TickTime(0),
        loop_range: Some((TickTime(0), TickTime(8 * BAR))),
    };
    let start = Instant::now();
    let mut prims = 0;
    for _ in 0..60 {
        a.view.scroll_x += 5.0;
        prims = a.scene(doc.project(), &env).prims.len();
    }
    let per_frame = start.elapsed() / 60;
    eprintln!("{prims} primitives, {per_frame:?} per frame");
    assert!(prims < 5_000, "{prims} primitives for one screen");
    // Generous for debug builds on slow CI; release is far below this.
    assert!(per_frame.as_millis() < 8, "{per_frame:?} per frame");
    let clips = a
        .scene(doc.project(), &env)
        .prims
        .iter()
        .filter(|p| matches!(p, Prim::RoundRect { .. }))
        .count();
    // About 12 tracks by 10 bars are on screen.
    assert!((100..=200).contains(&clips), "{clips} clips drawn");
}
