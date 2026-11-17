//! Tests of what happens to unsaved work when the project is saved, closed
//! or replaced.

use std::time::Duration;

use daw_model::commands::AddTrack;
use daw_model::{Track, TrackKind};

use super::*;
use crate::engine::StubEngine;

fn state() -> AppState {
    AppState::with_engine(Box::new(StubEngine::default()))
}

/// Adds an armed audio track.
fn arm_audio_track(s: &mut AppState) -> TrackId {
    let mut track = Track::new(TrackKind::Audio, "Guitar");
    track.armed = true;
    let id = track.id;
    s.doc.execute(AddTrack::new(0, track)).unwrap();
    s.sync();
    id
}

/// Starts a take on the armed tracks and lets it run for a moment.
fn record_a_moment(s: &mut AppState) {
    s.start_recording().unwrap();
    assert!(s.is_recording(), "{:?}", s.status);
    std::thread::sleep(Duration::from_millis(150));
}

fn clips(s: &AppState, track: TrackId) -> usize {
    s.doc.project().track(track).unwrap().clips.len()
}

#[test]
fn ending_a_take_makes_it_an_unsaved_edit() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = state();
    let track = arm_audio_track(&mut s);
    s.save_as(&dir.path().join("Song")).unwrap();
    record_a_moment(&mut s);
    // Nothing in the document says so yet: whoever closes or replaces the
    // project ends the take first.
    assert!(!s.doc.is_modified());

    s.stop_recording();
    assert_eq!(clips(&s, track), 1);
    assert!(s.doc.is_modified());
}

#[test]
fn saving_leaves_a_take_running() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = state();
    let track = arm_audio_track(&mut s);
    s.save_as(&dir.path().join("Song")).unwrap();
    record_a_moment(&mut s);

    s.save().unwrap();
    assert!(s.is_recording() && !s.doc.is_modified());
    s.stop_recording();
    assert_eq!(clips(&s, track), 1);
    assert!(s.doc.is_modified());
}

#[test]
fn saving_as_ends_the_take_and_saves_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = state();
    let track = arm_audio_track(&mut s);
    s.save_as(&dir.path().join("Song")).unwrap();
    record_a_moment(&mut s);

    let copy = dir.path().join("Copy");
    s.save_as(&copy).unwrap();
    assert!(!s.is_recording() && !s.doc.is_modified());
    let saved = Document::open(&copy).unwrap();
    let clip = &saved.project().track(track).unwrap().clips[0];
    let daw_model::ClipContent::Audio(audio) = &clip.content else {
        panic!("the take is an audio clip");
    };
    assert!(saved.bundle().unwrap().resolve(&audio.source).is_file());
}
