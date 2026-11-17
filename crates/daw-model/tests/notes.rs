//! Note commands and how they share undo steps.

use daw_core::time::TickTime;
use daw_model::commands::{AddNotes, EditNotes};
use daw_model::{Clip, ClipContent, ClipId, Document, MidiClip, Note, Project, Track, TrackKind};

fn note(start: i64, length: i64, key: u8) -> Note {
    Note::new(TickTime(start), TickTime(length), key, 0.8)
}

/// A document with one MIDI clip holding `notes`.
fn doc_with_clip(notes: Vec<Note>) -> (Document, ClipId) {
    let mut project = Project::new("N");
    let mut track = Track::new(TrackKind::Instrument, "keys");
    let clip = Clip::new(
        "part",
        TickTime(0),
        TickTime(1000),
        ClipContent::Midi(MidiClip { notes }),
    );
    let id = clip.id;
    track.clips.push(clip.into());
    project.tracks.push(track.into());
    (Document::new(project), id)
}

fn notes(doc: &Document, clip: ClipId) -> Vec<Note> {
    match &doc.project().clip(clip).unwrap().content {
        ClipContent::Midi(m) => m.notes.clone(),
        ClipContent::Audio(_) => unreachable!("a MIDI clip"),
    }
}

fn resized(n: &Note, length: i64) -> Note {
    Note {
        length: TickTime(length),
        ..n.clone()
    }
}

#[test]
fn edits_of_added_notes_join_the_step_that_added_them() {
    let old = note(0, 10, 60);
    let (mut doc, clip) = doc_with_clip(vec![old.clone()]);
    let (a, b) = (note(100, 10, 64), note(100, 10, 67));
    let add = AddNotes {
        clip,
        notes: vec![a.clone(), b.clone()],
    };
    doc.execute(add).unwrap();
    // A drag of one of them, in two moves.
    for length in [50, 80] {
        let notes = vec![resized(&a, length)];
        doc.execute(EditNotes { clip, notes }).unwrap();
    }
    let after = vec![old.clone(), resized(&a, 80), b];
    assert_eq!(notes(&doc, clip), after);
    assert_eq!(doc.history().undo_len(), 1);

    doc.undo().unwrap();
    assert_eq!(notes(&doc, clip), [old]);
    doc.redo().unwrap();
    assert_eq!(notes(&doc, clip), after, "redo adds them as they ended up");
    doc.undo().unwrap();
    doc.redo().unwrap();
    assert_eq!(notes(&doc, clip), after, "and again");
}

#[test]
fn edits_that_reach_other_notes_are_their_own_step() {
    let old = note(0, 10, 60);
    let (mut doc, clip) = doc_with_clip(vec![old.clone()]);
    let a = note(100, 10, 64);
    let add = AddNotes {
        clip,
        notes: vec![a.clone()],
    };
    doc.execute(add).unwrap();
    let both = vec![resized(&a, 50), resized(&old, 50)];
    doc.execute(EditNotes { clip, notes: both }).unwrap();
    assert_eq!(doc.history().undo_len(), 2);
    doc.undo().unwrap();
    assert_eq!(notes(&doc, clip), [old.clone(), a.clone()]);

    // After the gesture ends, an edit of the added note is its own step too.
    doc.seal();
    let notes = vec![resized(&a, 50)];
    doc.execute(EditNotes { clip, notes }).unwrap();
    assert_eq!(doc.history().undo_len(), 2);
}
