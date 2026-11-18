//! Regression tests for issues found in code review.

use daw_model::commands::*;
use daw_model::*;

fn project_with_track() -> (Project, TrackId) {
    let mut p = Project::new("R");
    let t = Track::new(TrackKind::Audio, "A");
    let id = t.id;
    p.tracks.push(t.into());
    (p, id)
}

#[test]
fn undo_limit_keeps_position_distinct_from_start() {
    let (mut p, id) = project_with_track();
    let mut h = UndoStack::new(2);
    for i in 0..3 {
        h.execute(
            &mut p,
            Box::new(SetTrack::new(id, TrackField::Name(format!("n{i}")))),
        )
        .unwrap();
        h.seal();
    }
    while h.undo(&mut p).unwrap().is_some() {}
    // The first rename can't be undone any more, so this isn't the start.
    assert_eq!(p.tracks[0].name, "n0");
    assert_ne!(h.position(), 0);
}

#[test]
fn autosave_is_removed_after_undoing_to_saved_state() {
    let (p, id) = project_with_track();
    let dir = tempfile::tempdir().unwrap();
    let mut doc = Document::new(p);
    doc.save_as(dir.path().join("S")).unwrap();
    doc.execute(SetTrack::new(id, TrackField::Mute(true)))
        .unwrap();
    assert!(doc.autosave().unwrap());
    let bundle = doc.bundle().unwrap().clone();
    assert!(bundle.autosave_file().exists());
    doc.undo().unwrap();
    assert!(!doc.autosave().unwrap());
    assert!(!bundle.autosave_file().exists());
    assert!(!bundle.has_newer_autosave());
}

#[test]
fn save_as_drops_old_autosave() {
    let (p, id) = project_with_track();
    let dir = tempfile::tempdir().unwrap();
    let mut doc = Document::new(p);
    doc.save_as(dir.path().join("Old")).unwrap();
    doc.execute(SetTrack::new(id, TrackField::Mute(true)))
        .unwrap();
    doc.autosave().unwrap();
    let old = doc.bundle().unwrap().clone();
    doc.save_as(dir.path().join("New")).unwrap();
    assert!(!old.autosave_file().exists());
}

#[test]
fn open_rejects_backups_and_other_files() {
    let (p, id) = project_with_track();
    let dir = tempfile::tempdir().unwrap();
    let mut doc = Document::new(p);
    doc.save_as(dir.path().join("B")).unwrap();
    doc.execute(SetTrack::new(id, TrackField::Mute(true)))
        .unwrap();
    doc.save().unwrap();
    doc.execute(SetTrack::new(id, TrackField::Solo(true)))
        .unwrap();
    doc.autosave().unwrap();
    let bundle = doc.bundle().unwrap().clone();

    let backup = bundle.backups().unwrap().pop().unwrap();
    assert!(Bundle::open(&backup).is_err());
    assert!(bundle::read_project(&backup).is_ok());
    // An autosave path stands for its bundle, so recovery can start from it.
    assert_eq!(Bundle::open(bundle.autosave_file()).unwrap(), bundle);
    let other = dir.path().join("song.json");
    std::fs::write(&other, "{}").unwrap();
    assert!(Bundle::open(&other).is_err());
}

#[test]
fn create_refuses_directory_with_another_project() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("Old");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("Other.kadence"), "{}").unwrap();
    assert!(Bundle::create(&target).is_err());
}

#[test]
fn imported_audio_paths_use_forward_slashes() {
    let dir = tempfile::tempdir().unwrap();
    let b = Bundle::create(dir.path().join("I")).unwrap();
    let src = dir.path().join("snare.wav");
    std::fs::write(&src, b"RIFF").unwrap();
    assert_eq!(
        b.import_audio(&src).unwrap().to_str(),
        Some("audio/snare.wav")
    );
}

#[test]
fn save_leaves_no_temp_file() {
    let (p, _) = project_with_track();
    let dir = tempfile::tempdir().unwrap();
    let mut doc = Document::new(p);
    doc.save_as(dir.path().join("T")).unwrap();
    let names: Vec<_> = std::fs::read_dir(dir.path().join("T"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(names.iter().all(|n| !n.to_string_lossy().ends_with(".tmp")));
}

/// Applies fine but refuses to revert.
#[derive(Debug)]
struct StuckOnRevert;

impl Command for StuckOnRevert {
    fn label(&self) -> String {
        "Stuck".into()
    }
    fn apply(&mut self, _: &mut Project) -> Result<ChangeSet, ModelError> {
        Ok(ChangeSet::new())
    }
    fn revert(&mut self, _: &mut Project) -> Result<ChangeSet, ModelError> {
        Err(ModelError::Invalid("stuck".into()))
    }
}

#[test]
fn failed_batch_undo_leaves_project_unchanged() {
    let (p, id) = project_with_track();
    let mut doc = Document::new(p);
    doc.execute(Batch::new(
        "Batch",
        vec![
            Box::new(StuckOnRevert),
            Box::new(SetTrack::new(id, TrackField::Name("renamed".into()))),
        ],
    ))
    .unwrap();
    let before = doc.project().clone();
    assert!(doc.undo().is_err());
    assert_eq!(doc.project(), &before);
    // Still undoable state is consistent: a second attempt fails the same way.
    assert!(doc.undo().is_err());
    assert_eq!(doc.project(), &before);
}

#[test]
fn add_track_rejects_self_routing_and_bad_targets() {
    let (mut p, audio) = project_with_track();
    let mut t = Track::new(TrackKind::Bus, "Loop");
    t.sends.push(Send::new(t.id));
    assert!(AddTrack::new(0, t).apply(&mut p).is_err());
    let mut t = Track::new(TrackKind::Audio, "ToAudio");
    t.output = TrackOutput::Bus(audio);
    assert!(AddTrack::new(0, t).apply(&mut p).is_err());
    let dup = (*p.tracks[0]).clone();
    assert!(AddTrack::new(0, dup).apply(&mut p).is_err());
    assert_eq!(p.tracks.len(), 1);
}

#[test]
fn bundle_named_backups_reopens() {
    let (p, _) = project_with_track();
    let dir = tempfile::tempdir().unwrap();
    let mut doc = Document::new(p);
    doc.save_as(dir.path().join("backups")).unwrap();
    assert_eq!(
        Document::open(dir.path().join("backups"))
            .unwrap()
            .project(),
        doc.project()
    );
}

#[test]
fn project_file_extension_case_is_ignored() {
    let (p, _) = project_with_track();
    let dir = tempfile::tempdir().unwrap();
    let mut doc = Document::new(p);
    doc.save_as(dir.path().join("Case")).unwrap();
    let root = dir.path().join("Case");
    std::fs::rename(root.join("Case.kadence"), root.join("Case.Kadence")).unwrap();
    assert_eq!(Document::open(&root).unwrap().project(), doc.project());
    assert_eq!(
        Document::open(root.join("Case.Kadence")).unwrap().project(),
        doc.project()
    );
}

#[test]
fn recover_works_when_only_the_autosave_is_left() {
    let (p, id) = project_with_track();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Lost");
    let mut doc = Document::new(p);
    doc.save_as(&root).unwrap();
    doc.execute(SetTrack::new(id, TrackField::Mute(true)))
        .unwrap();
    doc.autosave().unwrap();
    std::fs::remove_file(root.join("Lost.kadence")).unwrap();
    assert!(Bundle::open(&root).unwrap().has_newer_autosave());
    assert_eq!(Document::recover(&root).unwrap().project(), doc.project());
    assert_eq!(
        Document::recover(root.join("Lost.kadence.autosave"))
            .unwrap()
            .project(),
        doc.project()
    );
}

#[test]
fn open_refuses_while_a_crash_autosave_is_pending() {
    let (p, id) = project_with_track();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Crash");
    let mut first = Document::new(p);
    first.save_as(&root).unwrap();
    first
        .execute(SetTrack::new(id, TrackField::Mute(true)))
        .unwrap();
    first.autosave().unwrap();
    let crashed = first.project().clone();
    drop(first); // the app crashes

    // Neither path silently loads the older project file.
    assert!(matches!(
        Document::open(&root),
        Err(FormatError::AutosavePending(_))
    ));
    let autosave = root.join("Crash.kadence.autosave");
    assert!(matches!(
        Document::open(&autosave),
        Err(FormatError::AutosavePending(_))
    ));
    assert_eq!(Document::recover(&root).unwrap().project(), &crashed);

    let fresh = Document::open_discarding_autosave(&root).unwrap();
    assert!(!fresh.project().tracks[0].mute);
    assert!(!autosave.exists());
}

#[test]
fn failed_note_edit_does_not_copy_shared_clip() {
    let mut p = Project::new("N");
    let mut t = Track::new(TrackKind::Instrument, "I");
    let mut m = MidiClip::default();
    m.notes.push(Note::new(
        daw_core::time::TickTime(0),
        daw_core::time::TickTime(10),
        60,
        0.5,
    ));
    let clip = Clip::new(
        "c",
        daw_core::time::TickTime(0),
        daw_core::time::TickTime(100),
        ClipContent::Midi(m),
    );
    let clip_id = clip.id;
    t.clips.push(clip.into());
    p.tracks.push(t.into());
    let mut doc = Document::new(p);
    let snap = doc.snapshot();
    let stale = Note::new(
        daw_core::time::TickTime(0),
        daw_core::time::TickTime(10),
        61,
        0.5,
    );
    assert!(
        doc.execute(EditNotes {
            clip: clip_id,
            notes: vec![stale],
        })
        .is_err()
    );
    assert!(std::sync::Arc::ptr_eq(
        &snap.tracks[0],
        &doc.project().tracks[0]
    ));
}

/// Fails to apply.
#[derive(Debug)]
struct Refuses;

impl Command for Refuses {
    fn label(&self) -> String {
        "Refuses".into()
    }
    fn apply(&mut self, _: &mut Project) -> Result<ChangeSet, ModelError> {
        Err(ModelError::Invalid("no".into()))
    }
    fn revert(&mut self, _: &mut Project) -> Result<ChangeSet, ModelError> {
        Ok(ChangeSet::new())
    }
}

#[test]
fn failed_rollback_clears_history_and_marks_modified() {
    let (p, id) = project_with_track();
    let mut doc = Document::new(p);
    doc.execute(SetTrack::new(id, TrackField::Mute(true)))
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    doc.save_as(dir.path().join("X")).unwrap();
    let rev = doc.revision();
    let err = doc
        .execute(Batch::new(
            "Broken",
            vec![Box::new(StuckOnRevert), Box::new(Refuses)],
        ))
        .unwrap_err();
    assert!(matches!(err, ModelError::Inconsistent(_)));
    assert!(!doc.history().can_undo());
    assert!(doc.is_modified());
    assert!(doc.revision() > rev);
    doc.project().validate().unwrap();
}

#[test]
fn discarding_never_destroys_the_only_copy() {
    let (p, id) = project_with_track();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Only");
    let mut doc = Document::new(p);
    doc.save_as(&root).unwrap();
    doc.execute(SetTrack::new(id, TrackField::Mute(true)))
        .unwrap();
    doc.autosave().unwrap();
    std::fs::remove_file(root.join("Only.kadence")).unwrap();

    // Only the autosave is left: open reports it rather than NotFound.
    assert!(matches!(
        Document::open(&root),
        Err(FormatError::AutosavePending(_))
    ));
    assert!(Document::open_discarding_autosave(&root).is_err());
    assert!(root.join("Only.kadence.autosave").exists());
    assert_eq!(Document::recover(&root).unwrap().project(), doc.project());
}

/// Saves `doc` with a new track name each time, `n` times.
fn save_versions(doc: &mut Document, id: TrackId, n: usize) {
    for i in 0..n {
        doc.execute(SetTrack::new(id, TrackField::Name(format!("v{i}"))))
            .unwrap();
        doc.save().unwrap();
    }
}

fn backup_names(bundle: &Bundle) -> Vec<String> {
    bundle
        .backups()
        .unwrap()
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn backups_survive_renaming_the_project_file() {
    let (p, id) = project_with_track();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Song");
    let mut doc = Document::new(p);
    doc.save_as(&root).unwrap();
    save_versions(&mut doc, id, bundle::BACKUPS_KEPT + 1);
    // Renamed outside the app to a name that sorts first.
    std::fs::rename(root.join("Song.kadence"), root.join("Ballad.kadence")).unwrap();
    let mut doc = Document::open(&root).unwrap();
    save_versions(&mut doc, id, 2);
    let names = backup_names(doc.bundle().unwrap());
    assert_eq!(
        names.iter().filter(|n| n.starts_with("Ballad-")).count(),
        2,
        "{names:?}"
    );
    // The backups made before the rename are neither pruned nor listed last.
    assert_eq!(
        names.iter().filter(|n| n.starts_with("Song-")).count(),
        bundle::BACKUPS_KEPT
    );
    assert!(names.last().unwrap().starts_with("Ballad-"));
}

#[test]
fn backups_survive_a_clock_set_back() {
    let (p, id) = project_with_track();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Clock");
    let mut doc = Document::new(p);
    doc.save_as(&root).unwrap();
    // Backups stamped far in the future, as if the clock was stepped back.
    let b = doc.bundle().unwrap().clone();
    for i in 0..bundle::BACKUPS_KEPT {
        let ts = 5_000_000_000_000u64 + i as u64;
        std::fs::copy(
            b.project_file(),
            b.backups_dir().join(format!("Clock-{ts:016}.kadence")),
        )
        .unwrap();
    }
    save_versions(&mut doc, id, 2);
    let newest = b.backups().unwrap().pop().unwrap();
    let last = bundle::read_project(&newest).unwrap();
    assert_eq!(last.tracks[0].name, "v0");
    assert_eq!(b.backups().unwrap().len(), bundle::BACKUPS_KEPT);
}

#[test]
fn quick_saves_never_overwrite_a_backup() {
    let (p, id) = project_with_track();
    let dir = tempfile::tempdir().unwrap();
    let mut doc = Document::new(p);
    doc.save_as(dir.path().join("Fast")).unwrap();
    // No pause between saves: several land in the same millisecond.
    save_versions(&mut doc, id, 5);
    let backups = doc.bundle().unwrap().backups().unwrap();
    assert_eq!(backups.len(), 5);
    let names: Vec<String> = backups
        .iter()
        .map(|b| bundle::read_project(b).unwrap().tracks[0].name.clone())
        .collect();
    assert_eq!(names, ["A", "v0", "v1", "v2", "v3"]);
}

#[test]
fn save_as_refuses_a_folder_holding_an_autosave() {
    let (p, _) = project_with_track();
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("Demo");
    std::fs::create_dir(&target).unwrap();
    let autosave = target.join("Demo.kadence.autosave");
    std::fs::write(&autosave, "unsaved work").unwrap();
    let mut doc = Document::new(p);
    assert!(doc.save_as(&target).is_err());
    assert_eq!(std::fs::read_to_string(&autosave).unwrap(), "unsaved work");
}

#[test]
fn save_as_never_overwrites_audio_in_the_target() {
    let (p, _) = project_with_track();
    let dir = tempfile::tempdir().unwrap();
    let mut doc = Document::new(p);
    doc.save_as(dir.path().join("Old")).unwrap();
    let src = dir.path().join("take1.wav");
    std::fs::write(&src, b"old take").unwrap();
    doc.bundle().unwrap().import_audio(&src).unwrap();
    let target = dir.path().join("New");
    std::fs::create_dir_all(target.join("audio")).unwrap();
    std::fs::write(target.join("audio/take1.wav"), b"other take").unwrap();
    assert!(doc.save_as(&target).is_err());
    assert_eq!(
        std::fs::read(target.join("audio/take1.wav")).unwrap(),
        b"other take"
    );
    // The document stays on its old bundle.
    assert!(doc.bundle().unwrap().root().ends_with("Old"));
}

#[test]
fn a_failed_save_as_can_be_retried_into_the_same_folder() {
    let (p, _) = project_with_track();
    let dir = tempfile::tempdir().unwrap();
    let mut doc = Document::new(p);
    doc.save_as(dir.path().join("Old")).unwrap();
    let src = dir.path().join("take1.wav");
    std::fs::write(&src, b"take").unwrap();
    doc.bundle().unwrap().import_audio(&src).unwrap();
    // A directory where the project file goes makes writing the project fail
    // (at the rename over it) after the audio is copied.
    let target = dir.path().join("New");
    let blocker = target.join("New.kadence");
    std::fs::create_dir_all(&blocker).unwrap();
    assert!(doc.save_as(&target).is_err());
    assert!(!target.join("audio/take1.wav").exists());
    // Nothing else is left behind either: no backup and no temporary file.
    assert_eq!(
        std::fs::read_dir(target.join("backups")).unwrap().count(),
        0
    );
    assert_eq!(std::fs::read_dir(&target).unwrap().count(), 3);
    assert!(doc.bundle().unwrap().root().ends_with("Old"));
    std::fs::remove_dir(&blocker).unwrap();
    doc.save_as(&target).unwrap();
    assert_eq!(
        std::fs::read(target.join("audio/take1.wav")).unwrap(),
        b"take"
    );
}

#[cfg(unix)]
#[test]
fn save_keeps_the_project_file_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let (p, id) = project_with_track();
    let dir = tempfile::tempdir().unwrap();
    let mut doc = Document::new(p);
    doc.save_as(dir.path().join("Private")).unwrap();
    let b = doc.bundle().unwrap().clone();
    std::fs::set_permissions(b.project_file(), std::fs::Permissions::from_mode(0o600)).unwrap();
    save_versions(&mut doc, id, 1);
    let mode = |p: std::path::PathBuf| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(b.project_file()), 0o600);
    // The autosave holds the same data, so it gets the same mode.
    doc.execute(SetTrack::new(id, TrackField::Mute(true)))
        .unwrap();
    assert!(doc.autosave().unwrap());
    assert_eq!(mode(b.autosave_file()), 0o600);
}

#[cfg(unix)]
#[test]
fn save_writes_through_a_symlinked_project_file() {
    let (p, id) = project_with_track();
    let dir = tempfile::tempdir().unwrap();
    let mut doc = Document::new(p);
    doc.save_as(dir.path().join("Linked")).unwrap();
    let b = doc.bundle().unwrap().clone();
    let real = dir.path().join("synced.kadence");
    std::fs::rename(b.project_file(), &real).unwrap();
    std::os::unix::fs::symlink(&real, b.project_file()).unwrap();
    // Read through the link, which is what makes it the user's own.
    let mut doc = Document::open(b.root()).unwrap();
    save_versions(&mut doc, id, 1);
    assert!(
        std::fs::symlink_metadata(b.project_file())
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(bundle::read_project(&real).unwrap().tracks[0].name, "v0");
}

/// Issue #204: a cancelled gesture is taken back without becoming a redo
/// step, and only while it is still the last step.
#[test]
fn abandoning_a_step_leaves_no_redo_and_restores_the_position() {
    let (p, id) = project_with_track();
    let mut doc = Document::new(p);
    let rename = |n: &str| SetTrack::new(id, TrackField::Name(n.into()));
    doc.execute(rename("kept")).unwrap();
    doc.seal();
    let kept = doc.history().position();
    assert!(doc.abandon(kept + 100).unwrap().is_none(), "not a step");

    doc.execute(rename("dragged")).unwrap();
    let step = doc.history().position();
    let revision = doc.revision();
    assert!(doc.abandon(step).unwrap().is_some());
    assert_eq!(doc.project().tracks[0].name, "kept");
    assert_eq!(doc.history().position(), kept);
    assert!(!doc.history().can_redo());
    assert_eq!(doc.history().undo_len(), 1);
    assert!(doc.revision() > revision, "the engine needs a new snapshot");

    // The next edit doesn't merge into the step before the abandoned one.
    doc.execute(rename("next")).unwrap();
    assert_eq!(doc.history().undo_len(), 2);

    // A step that is no longer the last one stays.
    let buried = doc.history().position();
    doc.seal();
    doc.execute(SetTrack::new(id, TrackField::Mute(true)))
        .unwrap();
    assert!(doc.abandon(buried).unwrap().is_none());
    assert_eq!(doc.project().tracks[0].name, "next");
    assert_eq!(doc.history().undo_len(), 3);
}

/// A step that is abandoned gives back what running it cost the history:
/// the redo steps it cleared and, at the undo limit, the oldest undo step.
#[test]
fn abandoning_a_step_restores_what_it_displaced() {
    let (mut p, id) = project_with_track();
    let mut h = UndoStack::new(3);
    let rename = |n: &str| Box::new(SetTrack::new(id, TrackField::Name(n.into())));
    for n in ["a", "b", "c", "d"] {
        h.execute(&mut p, rename(n)).unwrap();
        h.seal();
    }
    // "a" fell off the limit; undo "d" so there is something to redo.
    h.undo(&mut p).unwrap();
    let (position, len) = (h.position(), h.undo_len());
    assert_eq!((len, h.redo_len()), (2, 1));

    h.execute(&mut p, rename("dragged")).unwrap();
    assert!(!h.can_redo());
    assert!(h.abandon(&mut p, h.position()).unwrap().is_some());
    assert_eq!((h.undo_len(), h.redo_len()), (len, 1));
    assert_eq!(h.position(), position);
    h.redo(&mut p).unwrap();
    assert_eq!(p.tracks[0].name, "d");

    // Full again: the step drops "b", and abandoning it brings "b" back.
    assert_eq!(h.undo_len(), 3);
    let position = h.position();
    h.execute(&mut p, rename("dragged")).unwrap();
    assert_eq!(h.undo_len(), 3);
    assert!(h.abandon(&mut p, h.position()).unwrap().is_some());
    assert_eq!((h.undo_len(), h.position()), (3, position));
    while h.undo(&mut p).unwrap().is_some() {}
    assert_eq!(p.tracks[0].name, "a");
    assert_ne!(h.position(), 0, "the first rename is still out of reach");

    // A step that is kept keeps what it displaced gone.
    h.redo(&mut p).unwrap();
    h.execute(&mut p, rename("kept")).unwrap();
    h.seal();
    let kept = h.position();
    h.execute(&mut p, rename("next")).unwrap();
    assert!(h.abandon(&mut p, kept).unwrap().is_none());
    assert!(h.abandon(&mut p, h.position()).unwrap().is_some());
    assert_eq!(p.tracks[0].name, "kept");
    assert!(!h.can_redo());
}

/// A saved bundle in a new folder, as if received from somewhere else, and a
/// file outside it that nothing in the bundle may write to.
#[cfg(unix)]
fn received_bundle() -> (tempfile::TempDir, Document, TrackId, std::path::PathBuf) {
    let (p, id) = project_with_track();
    let dir = tempfile::tempdir().unwrap();
    let mut doc = Document::new(p);
    doc.save_as(dir.path().join("Received")).unwrap();
    let victim = dir.path().join("victim.txt");
    std::fs::write(&victim, "mine").unwrap();
    (dir, doc, id, victim)
}

#[cfg(unix)]
fn append(path: std::path::PathBuf, suffix: &str) -> std::path::PathBuf {
    let mut name = path.into_os_string();
    name.push(suffix);
    name.into()
}

#[cfg(unix)]
#[test]
fn links_at_temporary_names_are_not_written_through() {
    let (_dir, mut doc, id, victim) = received_bundle();
    let b = doc.bundle().unwrap().clone();
    for planted in [
        append(b.project_file(), ".tmp"),
        append(b.autosave_file(), ".tmp"),
    ] {
        std::os::unix::fs::symlink(&victim, planted).unwrap();
    }
    doc.execute(SetTrack::new(id, TrackField::Mute(true)))
        .unwrap();
    assert!(doc.autosave().unwrap());
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "mine");
    save_versions(&mut doc, id, 1);
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "mine");
}

#[cfg(unix)]
#[test]
fn autosave_replaces_a_link_instead_of_writing_through_it() {
    let (_dir, mut doc, id, victim) = received_bundle();
    let b = doc.bundle().unwrap().clone();
    std::os::unix::fs::symlink(&victim, b.autosave_file()).unwrap();
    doc.execute(SetTrack::new(id, TrackField::Mute(true)))
        .unwrap();
    assert!(doc.autosave().unwrap());
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "mine");
    let autosave = std::fs::symlink_metadata(b.autosave_file()).unwrap();
    assert!(autosave.file_type().is_file());
    assert!(bundle::read_project(&b.autosave_file()).unwrap().tracks[0].mute);
}

#[cfg(unix)]
#[test]
fn recovering_does_not_follow_a_linked_project_file() {
    let (_dir, mut doc, id, victim) = received_bundle();
    let b = doc.bundle().unwrap().clone();
    doc.execute(SetTrack::new(id, TrackField::Mute(true)))
        .unwrap();
    assert!(doc.autosave().unwrap());
    // The project file was never read, so nothing shows the link is the
    // user's own.
    std::fs::remove_file(b.project_file()).unwrap();
    std::os::unix::fs::symlink(&victim, b.project_file()).unwrap();
    let mut doc = Document::recover(b.root()).unwrap();
    doc.save().unwrap();
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "mine");
    assert!(b.backups().unwrap().is_empty());
    let saved = std::fs::symlink_metadata(b.project_file()).unwrap();
    assert!(saved.file_type().is_file());
    assert!(b.load().unwrap().tracks[0].mute);
}

/// Milliseconds since the Unix epoch, as backups are stamped.
#[cfg(unix)]
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[cfg(unix)]
#[test]
fn backup_does_not_create_the_target_of_a_dangling_link() {
    let (dir, mut doc, id, _victim) = received_bundle();
    let b = doc.bundle().unwrap().clone();
    let target = dir.path().join("created.txt");
    // An hour ahead, as after a clock change, so the next backup's name is
    // known.
    let ahead = now_millis() + 3_600_000;
    let name = |stamp: u64| {
        b.backups_dir()
            .join(format!("Received-{stamp:016}.kadence"))
    };
    std::fs::copy(b.project_file(), name(ahead)).unwrap();
    std::os::unix::fs::symlink(&target, name(ahead + 1)).unwrap();
    save_versions(&mut doc, id, 1);
    assert!(!target.exists());
    assert!(name(ahead + 2).is_file());
}

#[test]
fn a_backup_at_the_last_time_stamp_fails_the_save_instead_of_piling_up() {
    let (p, id) = project_with_track();
    let dir = tempfile::tempdir().unwrap();
    let mut doc = Document::new(p);
    doc.save_as(dir.path().join("Far")).unwrap();
    let b = doc.bundle().unwrap().clone();
    let last = b.backups_dir().join("Far-9999999999999999.kadence");
    std::fs::copy(b.project_file(), &last).unwrap();
    doc.execute(SetTrack::new(id, TrackField::Name("v0".into())))
        .unwrap();
    assert!(doc.save().is_err());
    assert_eq!(backup_names(&b), ["Far-9999999999999999.kadence"]);
    assert_eq!(b.load().unwrap().tracks[0].name, "A");
    std::fs::remove_file(last).unwrap();
    doc.save().unwrap();
    assert_eq!(b.load().unwrap().tracks[0].name, "v0");
}

fn audio_clip_at(start: i64, length: i64) -> Clip {
    Clip::new(
        "c",
        daw_core::time::TickTime(start),
        daw_core::time::TickTime(length),
        ClipContent::Audio(AudioClip::new("audio/a.wav", daw_core::time::SampleTime(0))),
    )
}

/// `project` as JSON with `edit` applied, loaded again.
fn reloaded(
    project: &Project,
    edit: impl Fn(&mut serde_json::Value),
) -> Result<Project, FormatError> {
    let mut json: serde_json::Value =
        serde_json::from_str(&bundle::to_json(project).unwrap()).unwrap();
    edit(&mut json);
    bundle::from_json(&json.to_string())
}

#[test]
fn a_project_file_with_times_out_of_range_does_not_load() {
    use daw_core::time::TickTime;
    use serde_json::json;
    let (mut p, id) = project_with_track();
    let mut h = UndoStack::new(8);
    h.execute(&mut p, Box::new(AddClip::new(id, audio_clip_at(0, 960))))
        .unwrap();
    let midi = Track::new(TrackKind::Instrument, "I");
    let midi_id = midi.id;
    h.execute(&mut p, Box::new(AddTrack::new(1, midi))).unwrap();
    let mut notes = MidiClip::default();
    notes
        .notes
        .push(Note::new(TickTime(0), TickTime(10), 60, 1.0));
    let clip = Clip::new("m", TickTime(0), TickTime(960), ClipContent::Midi(notes));
    h.execute(&mut p, Box::new(AddClip::new(midi_id, clip)))
        .unwrap();
    let mut lane = AutomationLane::new(AutomationTarget::Volume);
    lane.points.push(AutomationPoint::new(TickTime(0), 0.0));
    h.execute(&mut p, Box::new(AddLane { track: id, lane }))
        .unwrap();
    assert!(reloaded(&p, |_| {}).is_ok());

    let over = MAX_PROJECT_TICK.0 + 1;
    let audio = "/project/tracks/0/clips/0";
    let note = "/project/tracks/1/clips/0/content/notes/0";
    let cases = [
        (format!("{audio}/start"), json!(i64::MAX)),
        (format!("{audio}/length"), json!(i64::MAX)),
        (format!("{audio}/start"), json!(over)),
        (format!("{audio}/length"), json!(over)),
        (
            format!("{audio}/content/offset"),
            json!(MAX_AUDIO_OFFSET.0 + 1),
        ),
        (format!("{audio}/content/source"), json!("")),
        (format!("{audio}/content/fade_in/length"), json!(over)),
        (format!("{note}/start"), json!(over)),
        (format!("{note}/length"), json!(over)),
        (
            "/project/tracks/0/automation/0/points/0/tick".into(),
            json!(over),
        ),
        (
            "/project/tracks/0/automation/0/points/0/tick".into(),
            json!(i64::MIN),
        ),
        ("/project/tempo/tempos/0/bpm".into(), json!(1e-300)),
        ("/project/tempo/tempos/0/bpm".into(), json!(1e300)),
    ];
    for (path, value) in cases {
        let loaded = reloaded(&p, |json| {
            *json
                .pointer_mut(&path)
                .unwrap_or_else(|| panic!("no {path}")) = value.clone();
        });
        assert!(loaded.is_err(), "{path} = {value} loaded");
    }
    let mut far = p.clone();
    far.tempo.tempos.push(TempoPoint {
        tick: TickTime(over),
        ..far.tempo.tempos[0].clone()
    });
    assert!(far.validate().is_err());
    // The furthest values allowed are fine.
    audio_clip_at(MAX_PROJECT_TICK.0, MAX_PROJECT_TICK.0)
        .check()
        .unwrap();
}

#[test]
fn edits_hold_clips_to_what_a_project_file_may_say() {
    use daw_core::time::TickTime;
    let (mut p, id) = project_with_track();
    let mut h = UndoStack::new(8);
    let clip = audio_clip_at(0, 960);
    let cid = clip.id;
    h.execute(&mut p, Box::new(AddClip::new(id, clip))).unwrap();
    let before = p.clone();
    let far = TickTime(MAX_PROJECT_TICK.0 + 1);
    let refused: Vec<Box<dyn Command>> = vec![
        Box::new(MoveClip::new(cid, id, far)),
        Box::new(SetClip::new(cid, ClipField::Length(far))),
        Box::new(SetClip::new(cid, ClipField::Length(TickTime(0)))),
        Box::new(SetClip::new(cid, ClipField::AudioSource("".into()))),
        Box::new(SetClip::new(
            cid,
            ClipField::AudioOffset(daw_core::time::SampleTime(MAX_AUDIO_OFFSET.0 + 1)),
        )),
        Box::new(SetClip::new(
            cid,
            ClipField::FadeIn(Fade::new(far, FadeShape::default())),
        )),
    ];
    for cmd in refused {
        assert!(h.execute(&mut p, cmd).is_err());
        // A refused edit leaves the clip as it was.
        assert_eq!(p, before);
    }
    h.execute(&mut p, Box::new(MoveClip::new(cid, id, MAX_PROJECT_TICK)))
        .unwrap();
}

#[test]
fn a_bus_armed_for_recording_does_not_load() {
    let (mut p, _) = project_with_track();
    let mut bus = Track::new(TrackKind::Bus, "B");
    bus.armed = true;
    let mut h = UndoStack::new(8);
    assert!(
        h.execute(&mut p, Box::new(AddTrack::new(1, bus.clone())))
            .is_err()
    );
    p.tracks.push(bus.into());
    assert!(p.validate().is_err());
}
