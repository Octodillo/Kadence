//! Tests of what happens to unsaved work when the project is saved, closed
//! or replaced.

use std::time::Duration;

use daw_model::commands::AddTrack;
use daw_model::{Track, TrackKind};

use super::*;
use crate::engine::StubEngine;
use crate::settings::MemoryStore;

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

// ---------------------------------------------------------------------------
// Autosave, recovery and the audio of a project that was never saved.

/// Starts the session with its scratch folders under `base`.
fn start_session(s: &mut AppState, base: &Path) {
    s.start_session(Box::new(MemoryStore::default()), Some(base));
}

/// The one audio clip's source, as the project stores it.
fn take_source(s: &AppState, track: TrackId) -> PathBuf {
    let clip = &s.doc.project().track(track).unwrap().clips[0];
    let daw_model::ClipContent::Audio(audio) = &clip.content else {
        panic!("the take is an audio clip");
    };
    audio.source.clone()
}

/// Records a short take on a new armed track and returns the track.
fn record_a_take(s: &mut AppState) -> TrackId {
    let track = arm_audio_track(s);
    record_a_moment(s);
    s.stop_recording();
    assert_eq!(clips(s, track), 1, "{:?}", s.status);
    track
}

fn file_count(dir: &Path) -> usize {
    std::fs::read_dir(dir).map_or(0, Iterator::count)
}

#[test]
fn the_autosave_of_undone_edits_goes_when_the_project_is_left() {
    for leave in [AppState::shutdown, AppState::new_document] {
        let dir = tempfile::tempdir().unwrap();
        let mut s = state();
        s.save_as(&dir.path().join("Song")).unwrap();
        let autosave = s.doc.bundle().unwrap().autosave_file();

        s.set_bpm(90.0).unwrap();
        s.autosave();
        assert!(autosave.is_file());
        // Back at the saved state, and gone before the timer comes round.
        s.doc.undo().unwrap();
        assert!(!s.doc.is_modified());
        leave(&mut s);
        assert!(!autosave.exists());
    }
}

#[test]
fn an_unsaved_projects_take_lives_in_its_scratch_folder() {
    let base = tempfile::tempdir().unwrap();
    let mut s = state();
    start_session(&mut s, base.path());
    let track = record_a_take(&mut s);

    // Stored like a bundle's audio, relative to the folder.
    let source = take_source(&s, track);
    assert!(source.starts_with("audio"), "{source:?}");
    assert!(s.media_root().starts_with(base.path()));
    assert!(s.media_root().join(&source).is_file());
    assert!(s.missing_files().is_empty());

    // Dropped with the project.
    let audio = recovery::audio_dir(&s.media_root());
    s.new_document();
    assert!(!audio.exists());
}

#[test]
fn the_first_save_brings_the_takes_into_the_bundle() {
    let base = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut s = state();
    start_session(&mut s, base.path());
    let track = record_a_take(&mut s);
    let source = take_source(&s, track);
    let scratch_audio = recovery::audio_dir(&s.media_root());

    let song = dir.path().join("Song");
    s.save_as(&song).unwrap();
    assert!(!s.doc.is_modified());
    // The same relative path, now inside the bundle; nothing left behind.
    assert_eq!(take_source(&s, track), source);
    assert!(song.join(&source).is_file());
    assert!(!scratch_audio.exists());
    assert!(s.missing_files().is_empty());
    assert_eq!(s.external_file_count(), 0);

    // The history still works: redoing the take finds its file.
    s.doc.undo().unwrap();
    s.doc.redo().unwrap();
    assert!(s.missing_files().is_empty());

    // And the saved project plays it.
    let saved = Document::open(&song).unwrap();
    assert!(daw_model::files::missing_audio(saved.project(), saved.bundle()).is_empty());
}

#[test]
fn a_failed_first_save_leaves_the_takes_where_they_were() {
    let base = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut s = state();
    start_session(&mut s, base.path());
    let track = record_a_take(&mut s);
    let source = take_source(&s, track);

    // A folder that already has audio under the take's name.
    let song = dir.path().join("Song");
    std::fs::create_dir_all(song.join("audio")).unwrap();
    std::fs::write(song.join(&source), b"someone else's").unwrap();
    assert!(s.save_as(&song).is_err());
    assert!(s.doc.bundle().is_none());
    assert_eq!(
        std::fs::read(song.join(&source)).unwrap(),
        b"someone else's"
    );
    assert!(s.missing_files().is_empty());
}

#[test]
fn without_a_session_takes_go_to_a_temporary_folder_of_this_state() {
    let mut s = state();
    let track = record_a_take(&mut s);
    let root = s.media_root();
    assert!(root.starts_with(std::env::temp_dir()));
    assert!(root.join(take_source(&s, track)).is_file());
    drop(s);
    assert!(!root.exists());
}

#[test]
fn a_crashed_unsaved_project_is_recovered_with_its_takes() {
    let base = tempfile::tempdir().unwrap();
    let mut crashed = state();
    start_session(&mut crashed, base.path());
    // An autosave just now: the next is a while off.
    crashed.set_bpm(90.0).unwrap();
    crashed.autosave();
    let track = record_a_take(&mut crashed);
    // A take is autosaved on the next tick all the same.
    crashed.autosave();
    let folder = crashed.media_root();
    // As a crash would: nothing is cleaned up.
    drop(crashed);

    let mut s = state();
    start_session(&mut s, base.path());
    assert_eq!(s.recovery.len(), 1);
    s.recover_offered(0).unwrap();
    assert!(s.recovery.is_empty());
    assert_eq!(s.media_root(), folder);
    assert_eq!(clips(&s, track), 1);
    assert!(s.missing_files().is_empty());

    // Discarded at a clean exit, with its audio.
    s.shutdown();
    assert_eq!(file_count(base.path()), 0);
}

#[test]
fn unsaved_work_stays_on_offer_until_it_is_dealt_with() {
    let base = tempfile::tempdir().unwrap();
    let mut crashed = state();
    start_session(&mut crashed, base.path());
    crashed.set_bpm(90.0).unwrap();
    crashed.autosave();
    let folder = crashed.media_root();
    let autosave = folder.join("Untitled.kadence.autosave");
    let good = std::fs::read(&autosave).unwrap();
    drop(crashed);

    let mut s = state();
    start_session(&mut s, base.path());
    assert_eq!(s.recovery.len(), 1);

    // A recovery that fails can be tried again.
    std::fs::write(&autosave, b"not a project").unwrap();
    assert!(s.recover_offered(0).is_err());
    assert_eq!(s.recovery.len(), 1);

    // Recovered by another Kadence meanwhile: not ours to discard or open.
    std::fs::write(&autosave, good).unwrap();
    let other = recovery::Scratch::adopt(&folder).unwrap().unwrap();
    let offered = s.recovery.clone();
    assert!(matches!(s.discard_offered(0), Err(Message::AlreadyOpen(_))));
    assert!(autosave.is_file());
    assert!(s.recovery.is_empty());
    s.recovery = offered.clone();
    assert!(matches!(s.recover_offered(0), Err(Message::AlreadyOpen(_))));
    assert!(s.recovery.is_empty());

    // Saved or dropped there since: nothing to recover or discard, and no
    // entry left that can do neither.
    other.remove();
    for deal_with in [AppState::recover_offered, AppState::discard_offered] {
        s.recovery = offered.clone();
        let _ = deal_with(&mut s, 0);
        assert!(s.recovery.is_empty());
    }
}

#[test]
fn takes_from_before_the_session_come_along_into_the_scratch_folder() {
    let base = tempfile::tempdir().unwrap();
    let mut s = state();
    let track = record_a_take(&mut s);
    start_session(&mut s, base.path());
    assert!(s.media_root().starts_with(base.path()));
    assert!(s.media_root().join(take_source(&s, track)).is_file());
    assert!(s.missing_files().is_empty());
}

#[test]
fn a_saved_projects_unsaved_work_is_recovered_from_the_offer() {
    let dir = tempfile::tempdir().unwrap();
    let base = tempfile::tempdir().unwrap();
    let song = dir.path().join("Song");
    let mut crashed = state();
    crashed.save_as(&song).unwrap();
    crashed.set_bpm(90.0).unwrap();
    crashed.autosave();
    drop(crashed);

    let mut s = state();
    s.recovery = recovery::find(Some(base.path()), std::slice::from_ref(&song));
    assert_eq!(s.recovery.len(), 1);
    s.recover_offered(0).unwrap();
    assert!(s.recovery.is_empty());
    assert_eq!(s.bundle_root().as_deref(), Some(song.as_path()));
    assert!(s.doc.is_modified());
}
