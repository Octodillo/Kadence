//! Glue between the editing code and the project model.
//!
//! The piano roll edits copies of a clip's notes with the [`crate::edit`]
//! operations, then turns the result into one undoable command with
//! [`NoteEdit`]. Everything here runs off the audio thread.

use std::collections::HashMap;

use daw_core::time::{SegmentTempoMap, TempoMap, TickTime};
use daw_model::commands::{AddNotes, Batch, EditNotes, RemoveNotes};
use daw_model::{ClipContent, ClipId, Command, MidiClip, Note, NoteUid, Project, TempoData, Track};

use crate::edit;
use crate::grid::{Meter, MeterPoint};
use crate::note::{EditNote, NoteData};
use crate::record::TakePlan;
use crate::sequence::{ClipNotes, Sequence, compile, uid_from_u128};

impl EditNote for Note {
    fn data(&self) -> NoteData {
        NoteData {
            start: self.start,
            length: self.length,
            key: self.key,
            channel: self.channel,
            velocity: self.velocity,
        }
    }

    fn set_data(&mut self, d: NoteData) {
        self.start = d.start;
        self.length = d.length;
        self.key = d.key;
        self.channel = d.channel;
        self.velocity = d.velocity;
    }
}

/// A new model note with a fresh ID.
pub fn new_note(d: NoteData) -> Note {
    let mut n = Note::new(d.start, d.length, d.key, d.velocity);
    n.set_data(d);
    n
}

/// The player's uid for a model note.
pub fn note_uid(id: NoteUid) -> u64 {
    uid_from_u128(id.0.as_u128())
}

/// The grid's meter for the project's time signatures.
pub fn meter(tempo: &TempoData) -> Meter {
    Meter::new(tempo.meters.iter().map(|m| MeterPoint {
        tick: m.tick,
        numerator: m.numerator,
        denominator: m.denominator,
    }))
}

/// The tempo map for the project's tempo changes. Invalid points (a bpm
/// that is not positive and finite) are skipped.
pub fn tempo_map(tempo: &TempoData) -> SegmentTempoMap {
    let valid = |bpm: f64| bpm.is_finite() && bpm > 0.0;
    let first = tempo
        .tempos
        .iter()
        .find(|p| valid(p.bpm))
        .map_or(120.0, |p| p.bpm);
    let mut map = SegmentTempoMap::new(first);
    for p in tempo.tempos.iter().filter(|p| valid(p.bpm)) {
        map.set_tempo(p.tick, p.bpm, p.ramp);
    }
    map
}

/// The notes of a MIDI clip in `project`, if `clip` is one.
pub fn midi_clip(project: &Project, clip: ClipId) -> Option<&MidiClip> {
    match &project.clip(clip)?.content {
        ClipContent::Midi(m) => Some(m),
        ClipContent::Audio(_) => None,
    }
}

/// Compiles a track's unmuted MIDI clips for its player.
pub fn compile_track(track: &Track, tempo: &dyn TempoMap, sample_rate: f64) -> Sequence {
    let clips = track
        .clips
        .iter()
        .filter(|c| !c.muted)
        .filter_map(|c| match &c.content {
            ClipContent::Midi(m) => Some(ClipNotes {
                start: c.start,
                length: c.length,
                notes: m.notes.iter().map(|n| (note_uid(n.id), n.data())),
            }),
            ClipContent::Audio(_) => None,
        });
    compile(clips, tempo, sample_rate)
}

/// Collects removals, edits and additions to one clip's notes and builds
/// the single undoable command for them.
///
/// A pure edit (no notes added or removed) becomes an [`EditNotes`], which
/// the undo stack merges with the next edit of the same notes, so a drag
/// undoes in one step.
#[derive(Debug)]
pub struct NoteEdit {
    clip: ClipId,
    label: String,
    remove: Vec<NoteUid>,
    edit: Vec<Note>,
    add: Vec<Note>,
}

impl NoteEdit {
    pub fn new(clip: ClipId, label: impl Into<String>) -> Self {
        Self {
            clip,
            label: label.into(),
            remove: Vec::new(),
            edit: Vec::new(),
            add: Vec::new(),
        }
    }

    /// Records every note in `after` that differs from the note with the
    /// same ID in `before`. Notes not in `before` are ignored.
    pub fn changed(mut self, before: &[Note], after: &[Note]) -> Self {
        let old: HashMap<NoteUid, &Note> = before.iter().map(|n| (n.id, n)).collect();
        for n in after {
            if old.get(&n.id).is_some_and(|o| *o != n) {
                self.edit.retain(|e| e.id != n.id);
                self.edit.push(n.clone());
            }
        }
        self
    }

    pub fn remove_notes(mut self, ids: impl IntoIterator<Item = NoteUid>) -> Self {
        self.remove.extend(ids);
        self
    }

    /// Adds new notes, each with a fresh ID.
    pub fn add_notes(mut self, notes: impl IntoIterator<Item = NoteData>) -> Self {
        self.add.extend(notes.into_iter().map(new_note));
        self
    }

    /// The IDs the added notes will have, in order.
    pub fn added_ids(&self) -> Vec<NoteUid> {
        self.add.iter().map(|n| n.id).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.remove.is_empty() && self.edit.is_empty() && self.add.is_empty()
    }

    /// The command, or `None` when nothing changes.
    pub fn into_command(mut self) -> Option<Box<dyn Command>> {
        // An edited note that is also removed only needs removing.
        self.remove.sort();
        self.remove.dedup();
        let removed = &self.remove;
        self.edit.retain(|n| removed.binary_search(&n.id).is_err());
        let mut parts: Vec<Box<dyn Command>> = Vec::new();
        if !self.remove.is_empty() {
            parts.push(Box::new(RemoveNotes::new(self.clip, self.remove)));
        }
        if !self.edit.is_empty() {
            parts.push(Box::new(EditNotes {
                clip: self.clip,
                notes: self.edit,
            }));
        }
        if !self.add.is_empty() {
            parts.push(Box::new(AddNotes {
                clip: self.clip,
                notes: self.add,
            }));
        }
        if parts.is_empty() {
            return None;
        }
        if parts.len() == 1 && is_edit(&*parts[0]) {
            // Unwrapped, so the undo stack can merge it (its label is
            // "Edit Notes").
            return parts.pop();
        }
        Some(Box::new(Batch::new(self.label, parts)))
    }
}

fn is_edit(c: &dyn Command) -> bool {
    c.downcast_ref::<EditNotes>().is_some()
}

/// Copies of the notes in `clip` whose IDs are in `selected`, in clip
/// order.
pub fn selected_notes(clip: &MidiClip, selected: &[NoteUid]) -> Vec<Note> {
    clip.notes
        .iter()
        .filter(|n| selected.contains(&n.id))
        .cloned()
        .collect()
}

/// Runs an in-place edit (quantize, transpose, velocity and so on) on the
/// selected notes and returns the command, or `None` if nothing changed.
pub fn edit_selection(
    clip_id: ClipId,
    clip: &MidiClip,
    selected: &[NoteUid],
    label: &str,
    op: impl FnOnce(&mut [Note]),
) -> Option<Box<dyn Command>> {
    let before = selected_notes(clip, selected);
    let mut after = before.clone();
    op(&mut after);
    NoteEdit::new(clip_id, label)
        .changed(&before, &after)
        .into_command()
}

/// Deletes the selected notes.
pub fn delete_selection(clip_id: ClipId, selected: &[NoteUid]) -> Option<Box<dyn Command>> {
    NoteEdit::new(clip_id, "Delete Notes")
        .remove_notes(selected.iter().copied())
        .into_command()
}

/// Splits every selected note that `at` (clip-relative) falls inside.
pub fn split_selection(
    clip_id: ClipId,
    clip: &MidiClip,
    selected: &[NoteUid],
    at: TickTime,
) -> Option<Box<dyn Command>> {
    let before = selected_notes(clip, selected);
    let mut after = before.clone();
    let mut right = Vec::new();
    for n in &mut after {
        if let Some((l, r)) = edit::split(n.data(), at) {
            n.set_data(l);
            right.push(r);
        }
    }
    NoteEdit::new(clip_id, "Split Notes")
        .changed(&before, &after)
        .add_notes(right)
        .into_command()
}

/// Glues touching or overlapping selected notes on the same key.
pub fn glue_selection(
    clip_id: ClipId,
    clip: &MidiClip,
    selected: &[NoteUid],
    gap: i64,
) -> Option<Box<dyn Command>> {
    let before = selected_notes(clip, selected);
    let plan = edit::glue(&before, gap);
    let mut after = before.clone();
    for (i, d) in &plan.keep {
        after[*i].set_data(*d);
    }
    NoteEdit::new(clip_id, "Glue Notes")
        .changed(&before, &after)
        .remove_notes(plan.remove.iter().map(|&i| before[i].id))
        .into_command()
}

/// Cuts selected notes short where the next one on the same key starts,
/// deleting notes left with nothing to play.
pub fn trim_overlaps_selection(
    clip_id: ClipId,
    clip: &MidiClip,
    selected: &[NoteUid],
) -> Option<Box<dyn Command>> {
    let before = selected_notes(clip, selected);
    let mut after = before.clone();
    let dead = edit::trim_overlaps(&mut after);
    NoteEdit::new(clip_id, "Fix Overlaps")
        .changed(&before, &after)
        .remove_notes(dead.iter().map(|&i| before[i].id))
        .into_command()
}

/// Adds copies of the selected notes `offset` ticks later. Returns the
/// command and the copies' IDs, so the UI can select them.
pub fn duplicate_selection(
    clip_id: ClipId,
    clip: &MidiClip,
    selected: &[NoteUid],
    offset: i64,
) -> Option<(Box<dyn Command>, Vec<NoteUid>)> {
    let notes = selected_notes(clip, selected);
    let e = NoteEdit::new(clip_id, "Duplicate Notes").add_notes(edit::duplicate(&notes, offset));
    let ids = e.added_ids();
    e.into_command().map(|c| (c, ids))
}

/// Applies a recorded take to a clip, as planned by
/// [`merge_take`](crate::record::merge_take) against `existing`.
pub fn take_command(
    clip_id: ClipId,
    existing: &[Note],
    plan: TakePlan,
) -> Option<Box<dyn Command>> {
    NoteEdit::new(clip_id, "Record Notes")
        .remove_notes(plan.remove.iter().map(|&i| existing[i].id))
        .add_notes(plan.add)
        .into_command()
}

#[cfg(test)]
mod tests {
    use super::*;
    use daw_core::time::PPQ;
    use daw_model::commands::{AddClip, AddTrack};
    use daw_model::{Clip, Document, TrackKind};

    use crate::edit::{Quantize, Snap};
    use crate::grid::Grid;
    use crate::record::{TakeMode, merge_take};

    fn doc_with_clip(mut notes: Vec<Note>) -> (Document, ClipId) {
        notes.sort_by_key(|n| (n.start, n.key, n.id));
        let mut doc = Document::new(Project::new("test"));
        let track = daw_model::Track::new(TrackKind::Instrument, "keys");
        let tid = track.id;
        doc.execute(AddTrack::new(0, track)).unwrap();
        let clip = Clip::new(
            "part",
            TickTime(PPQ),
            TickTime(4 * PPQ),
            ClipContent::Midi(MidiClip { notes }),
        );
        let cid = clip.id;
        doc.execute(AddClip::new(tid, clip)).unwrap();
        (doc, cid)
    }

    fn notes(doc: &Document, clip: ClipId) -> Vec<NoteData> {
        midi_clip(doc.project(), clip)
            .unwrap()
            .notes
            .iter()
            .map(|n| n.data())
            .collect()
    }

    fn n(start: i64, len: i64, key: u8) -> Note {
        new_note(NoteData::new(start, len, key, 0.8))
    }

    #[test]
    fn quantize_is_one_undo_step() {
        let a = n(10, 100, 60);
        let b = n(250, 100, 62);
        let ids = [a.id, b.id];
        let (mut doc, cid) = doc_with_clip(vec![a, b]);
        let before = notes(&doc, cid);
        let grid = Grid::note(16);
        let meter = meter(&doc.project().tempo);
        let snap = Snap::new(&grid, &meter, TickTime(PPQ));
        let clip = midi_clip(doc.project(), cid).unwrap().clone();
        let cmd = edit_selection(cid, &clip, &ids, "Quantize", |ns| {
            edit::quantize(ns, &snap, Quantize::default())
        })
        .unwrap();
        doc.execute_boxed(cmd).unwrap();
        let starts: Vec<i64> = notes(&doc, cid).iter().map(|n| n.start.0).collect();
        assert_eq!(starts, [0, 240]);
        doc.undo().unwrap();
        assert_eq!(notes(&doc, cid), before);
        // Nothing to change: no command.
        let clip = midi_clip(doc.project(), cid).unwrap().clone();
        assert!(edit_selection(cid, &clip, &ids, "Nothing", |_| {}).is_none());
    }

    #[test]
    fn drag_edits_merge_into_one_step() {
        let a = n(0, 100, 60);
        let id = a.id;
        let (mut doc, cid) = doc_with_clip(vec![a]);
        for dt in [10, 10, 10] {
            let clip = midi_clip(doc.project(), cid).unwrap().clone();
            let cmd = edit_selection(cid, &clip, &[id], "Move", |ns| {
                edit::nudge(ns, dt);
            })
            .unwrap();
            doc.execute_boxed(cmd).unwrap();
        }
        assert_eq!(notes(&doc, cid)[0].start.0, 30);
        doc.undo().unwrap();
        assert_eq!(notes(&doc, cid)[0].start.0, 0);
    }

    #[test]
    fn split_glue_duplicate_delete() {
        let a = n(0, 200, 60);
        let id = a.id;
        let (mut doc, cid) = doc_with_clip(vec![a]);
        let clip = midi_clip(doc.project(), cid).unwrap().clone();
        doc.execute_boxed(split_selection(cid, &clip, &[id], TickTime(50)).unwrap())
            .unwrap();
        let spans: Vec<(i64, i64)> = notes(&doc, cid)
            .iter()
            .map(|n| (n.start.0, n.length.0))
            .collect();
        assert_eq!(spans, [(0, 50), (50, 150)]);

        let clip = midi_clip(doc.project(), cid).unwrap().clone();
        let all: Vec<NoteUid> = clip.notes.iter().map(|n| n.id).collect();
        doc.execute_boxed(glue_selection(cid, &clip, &all, 0).unwrap())
            .unwrap();
        let clip = midi_clip(doc.project(), cid).unwrap().clone();
        assert_eq!(clip.notes.len(), 1);
        assert_eq!((clip.notes[0].id, clip.notes[0].length.0), (id, 200));

        let (cmd, copies) = duplicate_selection(cid, &clip, &[id], 200).unwrap();
        doc.execute_boxed(cmd).unwrap();
        assert_eq!(notes(&doc, cid).len(), 2);
        assert_eq!(copies.len(), 1);
        doc.execute_boxed(delete_selection(cid, &copies).unwrap())
            .unwrap();
        assert_eq!(notes(&doc, cid).len(), 1);
        // Undo all four edits.
        for _ in 0..4 {
            doc.undo().unwrap();
        }
        assert_eq!(notes(&doc, cid), [NoteData::new(0, 200, 60, 0.8)]);
    }

    #[test]
    fn overlaps_and_takes() {
        let a = n(0, 300, 60);
        let b = n(100, 100, 60);
        let c = n(100, 50, 60);
        let ids = [a.id, b.id, c.id];
        let (mut doc, cid) = doc_with_clip(vec![a, b, c]);
        let clip = midi_clip(doc.project(), cid).unwrap().clone();
        doc.execute_boxed(trim_overlaps_selection(cid, &clip, &ids).unwrap())
            .unwrap();
        assert_eq!(notes(&doc, cid).len(), 2);
        assert!(
            notes(&doc, cid)
                .iter()
                .all(|n| n.end().0 <= 100 || n.start.0 >= 100)
        );

        let clip = midi_clip(doc.project(), cid).unwrap().clone();
        let take = vec![NoteData::new(120, 30, 64, 0.5)];
        let plan = merge_take(
            &clip.notes,
            take,
            TakeMode::Replace,
            TickTime(50)..TickTime(400),
        );
        doc.execute_boxed(take_command(cid, &clip.notes, plan).unwrap())
            .unwrap();
        let keys: Vec<u8> = notes(&doc, cid).iter().map(|n| n.key).collect();
        assert_eq!(keys, [60, 64]);
        doc.undo().unwrap();
        assert_eq!(notes(&doc, cid).len(), 2);
    }

    #[test]
    fn edit_and_remove_of_the_same_note_only_removes() {
        let a = n(0, 100, 60);
        let (mut doc, cid) = doc_with_clip(vec![a.clone()]);
        let mut moved = a.clone();
        moved.start = TickTime(10);
        let cmd = NoteEdit::new(cid, "x")
            .changed(std::slice::from_ref(&a), &[moved])
            .remove_notes([a.id])
            .into_command()
            .unwrap();
        doc.execute_boxed(cmd).unwrap();
        assert!(notes(&doc, cid).is_empty());
    }

    #[test]
    fn compile_and_tempo_from_the_model() {
        let (mut doc, cid) = doc_with_clip(vec![n(0, PPQ, 60)]);
        let mut tempo = doc.project().tempo.clone();
        tempo.tempos[0].bpm = 60.0;
        let map = tempo_map(&tempo);
        let snap = doc.snapshot();
        let track = &snap.project().tracks[0];
        let seq = compile_track(track, &map, 48_000.0);
        // The clip starts on beat 2: one second at 60 bpm.
        assert_eq!(seq.notes()[0].start.0, 48_000);
        assert_eq!(seq.notes()[0].end.0, 96_000);
        let id = midi_clip(doc.project(), cid).unwrap().notes[0].id;
        assert_eq!(seq.notes()[0].uid, note_uid(id));
        assert_eq!(meter(&tempo).bar_at(TickTime(4 * PPQ)).index, 1);
    }
}
