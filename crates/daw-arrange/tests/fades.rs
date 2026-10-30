//! Fade and gain handles on audio clips, and the clip gain and fade commands.

use daw_arrange::{Arrange, Cursor, Env, Hit, Pointer, Prim, Zone};
use daw_core::time::{PPQ, SampleTime, SegmentTempoMap, TickTime};
use daw_model::commands::{AddClip, AddTrack, ClipField, SetClip};
use daw_model::{
    AudioClip, Clip, ClipContent, ClipId, Document, Fade, FadeShape, Project, Track, TrackId,
    TrackKind,
};

const BEAT: i64 = PPQ;
const BAR: i64 = 4 * PPQ;

struct Rig {
    doc: Document,
    arrange: Arrange,
    tempo: SegmentTempoMap,
    track: TrackId,
}

impl Rig {
    /// One audio track; 32 px per beat, 64 px tracks under a 24 px ruler.
    fn new() -> Self {
        let mut doc = Document::new(Project::new("t"));
        let t = Track::new(TrackKind::Audio, "A");
        let track = t.id;
        doc.execute(AddTrack::new(0, t)).unwrap();
        doc.seal();
        let mut arrange = Arrange::new();
        arrange.resize(doc.project(), 1000.0, 400.0);
        Self {
            doc,
            arrange,
            tempo: SegmentTempoMap::new(120.0),
            track,
        }
    }

    fn clip(&mut self, start: i64, len: i64) -> ClipId {
        let c = Clip::new(
            "audio",
            TickTime(start),
            TickTime(len),
            ClipContent::Audio(AudioClip::new("missing.wav", SampleTime(0))),
        );
        let id = c.id;
        self.doc.execute(AddClip::new(self.track, c)).unwrap();
        self.doc.seal();
        id
    }

    fn audio(&self, id: ClipId) -> AudioClip {
        match &self.doc.project().clip(id).unwrap().content {
            ClipContent::Audio(a) => a.clone(),
            ClipContent::Midi(_) => panic!("not audio"),
        }
    }

    fn x(&self, tick: i64) -> f64 {
        self.arrange.view.tick_to_x(TickTime(tick))
    }

    /// Just inside the top of track 0, where the handles are.
    fn top(&self) -> f64 {
        self.arrange.row_y(0) + 4.0
    }

    fn mid(&self) -> f64 {
        self.arrange.row_y(0) + self.arrange.track_height / 2.0
    }

    fn with_env<R>(&mut self, f: impl FnOnce(&mut Arrange, &mut Document, &Env) -> R) -> R {
        let tempo = self.tempo.clone();
        let env = Env {
            tempo: &tempo,
            sample_rate: 48_000.0,
            root: None,
            playhead: TickTime(0),
            loop_range: None,
        };
        f(&mut self.arrange, &mut self.doc, &env)
    }

    /// Press at `from`, move in steps to `to`; release unless `hold`.
    fn drag(&mut self, from: (f64, f64), to: (f64, f64), shift: bool, hold: bool) {
        let mk = |x, y| Pointer {
            shift,
            ..Pointer::left(x, y)
        };
        self.arrange.press(&mut self.doc, mk(from.0, from.1));
        for i in 1..=4 {
            let f = f64::from(i) / 4.0;
            let p = mk(from.0 + (to.0 - from.0) * f, from.1 + (to.1 - from.1) * f);
            self.with_env(|a, doc, env| a.motion(doc, env, p));
        }
        if !hold {
            self.arrange.release(&mut self.doc, mk(to.0, to.1));
        }
    }

    fn zone(&self, x: f64, y: f64) -> Option<Zone> {
        match self.arrange.hit(self.doc.project(), x, y) {
            Hit::Clip { zone, .. } => Some(zone),
            _ => None,
        }
    }
}

#[test]
fn handles_sit_along_the_top_of_audio_clips() {
    let mut r = Rig::new();
    let _ = r.clip(BAR, BAR);
    let (top, mid) = (r.top(), r.mid());
    assert_eq!(r.zone(r.x(BAR) + 2.0, top), Some(Zone::FadeIn));
    assert_eq!(r.zone(r.x(2 * BAR) - 2.0, top), Some(Zone::FadeOut));
    assert_eq!(r.zone(r.x(BAR + BAR / 2), top), Some(Zone::Gain));
    // Below the handles the edges still trim.
    assert_eq!(r.zone(r.x(BAR) + 2.0, mid), Some(Zone::Start));
    assert_eq!(r.zone(r.x(2 * BAR) - 2.0, mid), Some(Zone::End));
    let p = r.doc.project();
    assert_eq!(
        r.arrange.cursor(p, r.x(BAR + BAR / 2), top),
        Cursor::ResizeVertical
    );
    assert_eq!(
        r.arrange.cursor(p, r.x(BAR) + 2.0, top),
        Cursor::ResizeHorizontal
    );
}

#[test]
fn tiny_clips_have_no_handles() {
    let mut r = Rig::new();
    let _ = r.clip(BAR, BEAT / 2); // 16 px wide
    assert_eq!(r.zone(r.x(BAR) + 1.0, r.top()), Some(Zone::Start));
}

#[test]
fn dragging_a_fade_handle_sets_the_fade_in_one_undo_step() {
    let mut r = Rig::new();
    let id = r.clip(BAR, BAR);
    let (top, from) = (r.top(), r.x(BAR) + 2.0);
    // The fade follows the pointer from wherever the handle was grabbed.
    let beat_px = r.x(BEAT) - r.x(0);
    r.drag((from, top), (from + beat_px, top), false, false);
    let a = r.audio(id);
    assert_eq!(a.fade_in.length, TickTime(BEAT));
    assert_eq!(a.fade_in.shape, FadeShape::Linear);
    assert!(r.arrange.selection.contains(&id));

    // The fade-out handle, dragged a beat and a half in from the end.
    let from = r.x(2 * BAR) - 2.0;
    r.drag((from, top), (from - 1.5 * beat_px, top), false, false);
    assert_eq!(r.audio(id).fade_out.length, TickTime(3 * BEAT / 2));

    // A fade can't run into the other one or past the clip.
    let h = r
        .arrange
        .handles(0, r.doc.project().clip(id).unwrap())
        .unwrap();
    let from = (h.fade_in.x + 2.0, top);
    r.drag(from, (r.x(3 * BAR), top), false, false);
    assert_eq!(r.audio(id).fade_in.length, TickTime(BAR - 3 * BEAT / 2));
    let h = r
        .arrange
        .handles(0, r.doc.project().clip(id).unwrap())
        .unwrap();
    r.drag((h.fade_in.x + 2.0, top), (0.0, top), false, false);
    assert!(r.audio(id).fade_in.is_none());

    // Each drag was one step.
    r.doc.undo().unwrap();
    assert_eq!(r.audio(id).fade_in.length, TickTime(BAR - 3 * BEAT / 2));
    r.doc.undo().unwrap();
    assert_eq!(r.audio(id).fade_in.length, TickTime(BEAT));
    r.doc.undo().unwrap();
    assert!(r.audio(id).fade_out.is_none());
    r.doc.undo().unwrap();
    assert!(r.audio(id).fade_in.is_none());
}

#[test]
fn dragging_the_gain_handle_changes_clip_gain() {
    let mut r = Rig::new();
    let id = r.clip(BAR, BAR);
    let (x, top) = (r.x(BAR + BAR / 2), r.top());
    // 4 px per dB.
    r.drag((x, top), (x, top - 40.0), false, true);
    assert_eq!(r.arrange.dragged_gain(r.doc.project()), Some(10.0));
    r.arrange.release(&mut r.doc, Pointer::left(x, top - 40.0));
    assert_eq!(r.audio(id).gain_db, 10.0);
    assert_eq!(r.arrange.dragged_gain(r.doc.project()), None);
    assert_eq!(
        r.doc.history().undo_label().as_deref(),
        Some("Change Clip Gain")
    );

    // Shift is ten times finer; down lowers it.
    r.drag((x, top), (x, top + 40.0), true, false);
    assert_eq!(r.audio(id).gain_db, 9.0);
    // It stops at the limits.
    r.drag((x, top), (x, top + 4000.0), false, false);
    assert_eq!(r.audio(id).gain_db, daw_arrange::MIN_CLIP_GAIN_DB);

    r.doc.undo().unwrap();
    r.doc.undo().unwrap();
    assert_eq!(r.audio(id).gain_db, 10.0);
    r.doc.undo().unwrap();
    assert_eq!(r.audio(id).gain_db, 0.0);
}

#[test]
fn escape_during_a_handle_drag_undoes_it() {
    let mut r = Rig::new();
    let id = r.clip(BAR, BAR);
    let top = r.top();
    r.drag((r.x(BAR) + 2.0, top), (r.x(BAR + BEAT), top), false, true);
    assert!(!r.audio(id).fade_in.is_none());
    r.arrange.cancel(&mut r.doc);
    assert!(r.audio(id).fade_in.is_none());
    assert!(
        !r.doc.history().can_undo()
            || r.doc.history().undo_label().as_deref() != Some("Change Fade In")
    );
}

#[test]
fn double_clicking_a_handle_resets_it() {
    let mut r = Rig::new();
    let id = r.clip(BAR, BAR);
    r.doc
        .execute(SetClip::new(id, ClipField::AudioGainDb(-3.0)))
        .unwrap();
    r.doc
        .execute(SetClip::new(
            id,
            ClipField::FadeOut(Fade::new(TickTime(BEAT), FadeShape::SCurve)),
        ))
        .unwrap();
    r.doc.seal();
    let (top, mid, x) = (r.top(), r.mid(), r.x(BAR + BAR / 2));
    let out = r.arrange.double_click(&mut r.doc, Pointer::left(x, top));
    assert!(out.document);
    assert!(out.open_clip.is_none());
    assert_eq!(r.audio(id).gain_db, 0.0);
    let h = r
        .arrange
        .handles(0, r.doc.project().clip(id).unwrap())
        .unwrap();
    r.arrange
        .double_click(&mut r.doc, Pointer::left(h.fade_out.x + 2.0, top));
    assert!(r.audio(id).fade_out.is_none());
    // Double-clicking the body still opens the clip.
    let out = r.arrange.double_click(&mut r.doc, Pointer::left(x, mid));
    assert_eq!(out.open_clip, Some(id));
}

#[test]
fn splitting_keeps_each_fade_on_its_own_end() {
    let mut r = Rig::new();
    let id = r.clip(0, BAR);
    let fade_in = Fade::new(TickTime(BEAT / 2), FadeShape::EqualPower);
    let fade_out = Fade::new(TickTime(BEAT), FadeShape::SCurve);
    r.doc
        .execute(SetClip::new(id, ClipField::FadeIn(fade_in)))
        .unwrap();
    r.doc
        .execute(SetClip::new(id, ClipField::FadeOut(fade_out)))
        .unwrap();
    r.doc.seal();
    r.arrange.selection = [id].into();
    r.with_env(|a, doc, env| a.split_at(doc, env, TickTime(2 * BEAT)));
    let left = r.audio(id);
    assert_eq!(left.fade_in, fade_in);
    assert!(left.fade_out.is_none());
    let right_id = r.doc.project().tracks[0].clips[1].id;
    let right = r.audio(right_id);
    assert!(right.fade_in.is_none());
    assert_eq!(right.fade_out, fade_out);
    r.doc.undo().unwrap();
    assert_eq!(r.audio(id).fade_out, fade_out);
}

#[test]
fn fade_shape_gain_reset_and_remove_apply_to_the_selection() {
    let mut r = Rig::new();
    let a = r.clip(0, BAR);
    let b = r.clip(BAR, BAR);
    let c = r.clip(2 * BAR, BAR);
    for id in [a, b] {
        r.doc
            .execute(SetClip::new(
                id,
                ClipField::FadeIn(Fade::new(TickTime(BEAT), FadeShape::Linear)),
            ))
            .unwrap();
        r.doc
            .execute(SetClip::new(id, ClipField::AudioGainDb(6.0)))
            .unwrap();
        r.doc.seal();
    }
    r.arrange.selection = [a, b].into();
    assert!(r.arrange.audio_selected(r.doc.project()));
    let out = r.arrange.set_fade_shape(&mut r.doc, FadeShape::SCurve);
    assert!(out.document);
    for id in [a, b] {
        let x = r.audio(id);
        assert_eq!(x.fade_in, Fade::new(TickTime(BEAT), FadeShape::SCurve));
        assert_eq!(x.fade_out.shape, FadeShape::SCurve);
    }
    assert_eq!(r.audio(c).fade_in.shape, FadeShape::Linear);
    r.arrange.reset_clip_gain(&mut r.doc);
    assert_eq!(r.audio(a).gain_db, 0.0);
    assert_eq!(r.audio(b).gain_db, 0.0);
    r.arrange.remove_fades(&mut r.doc);
    assert!(r.audio(a).fade_in.is_none());
    // Each was one undo step.
    r.doc.undo().unwrap();
    r.doc.undo().unwrap();
    assert_eq!(r.audio(b).gain_db, 6.0);
    r.doc.undo().unwrap();
    assert_eq!(r.audio(a).fade_in.shape, FadeShape::Linear);
    assert_eq!(r.audio(b).fade_in.shape, FadeShape::Linear);
}

#[test]
fn fades_and_handles_are_drawn() {
    let mut r = Rig::new();
    let id = r.clip(0, BAR);
    let count = |r: &mut Rig| {
        let p = r.doc.project().clone();
        let scene = r.with_env(|a, _, env| a.scene(&p, env));
        let lines = scene
            .prims
            .iter()
            .filter(|p| matches!(p, Prim::Line { width, .. } if *width == 1.5))
            .count();
        let handles = scene
            .prims
            .iter()
            .filter(|p| matches!(p, Prim::RoundRect { radius, .. } if *radius == 2.0))
            .count();
        (lines, handles)
    };
    assert_eq!(count(&mut r), (0, 3));
    r.doc
        .execute(SetClip::new(
            id,
            ClipField::FadeIn(Fade::new(TickTime(BEAT), FadeShape::EqualPower)),
        ))
        .unwrap();
    let (lines, handles) = count(&mut r);
    assert!(lines >= 10, "{lines} curve segments");
    assert_eq!(handles, 3);
}

#[test]
fn splitting_inside_a_fade_cuts_it_short() {
    let mut r = Rig::new();
    let id = r.clip(0, BAR);
    let fade = |len| Fade::new(TickTime(len), FadeShape::Linear);
    r.doc
        .execute(SetClip::new(id, ClipField::FadeIn(fade(2 * BEAT))))
        .unwrap();
    r.doc
        .execute(SetClip::new(id, ClipField::FadeOut(fade(2 * BEAT))))
        .unwrap();
    r.doc.seal();
    // Inside the fade-in: the left half keeps a fade no longer than itself.
    r.arrange.selection = [id].into();
    r.with_env(|a, doc, env| a.split_at(doc, env, TickTime(BEAT)));
    assert_eq!(r.audio(id).fade_in, fade(BEAT));
    assert!(r.audio(id).fade_out.is_none());
    let right = r.doc.project().tracks[0].clips[1].id;
    assert_eq!(r.audio(right).fade_out, fade(2 * BEAT));
    // Inside the fade-out: the right half's fade fits it.
    r.arrange.selection = [right].into();
    r.with_env(|a, doc, env| a.split_at(doc, env, TickTime(3 * BEAT)));
    let last = r.doc.project().tracks[0].clips[2].id;
    assert_eq!(r.audio(last).fade_out, fade(BEAT));
    assert!(r.audio(right).fade_out.is_none());
}

#[test]
fn grabbing_a_fade_handle_does_not_jump() {
    let mut r = Rig::new();
    let id = r.clip(BAR, BAR);
    let top = r.top();
    // Press in the middle of the fade-out handle and move straight down.
    let h = r
        .arrange
        .handles(0, r.doc.project().clip(id).unwrap())
        .unwrap();
    let x = h.fade_out.x + h.fade_out.w / 2.0;
    r.drag((x, top), (x, top + 5.0), false, false);
    assert!(r.audio(id).fade_out.is_none());
    // And then moving left by a beat makes a one-beat fade.
    r.drag((x, top), (x - r.x(BEAT) + r.x(0), top), false, false);
    assert_eq!(r.audio(id).fade_out.length, TickTime(BEAT));
}
