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
    save_versions(&mut doc, id, 1);
    assert!(
        std::fs::symlink_metadata(b.project_file())
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(bundle::read_project(&real).unwrap().tracks[0].name, "v0");
}
