//! Missing and external audio files: detection, relink and collect.

use std::fs;
use std::path::{Path, PathBuf};

use daw_core::time::{SampleTime, TickTime};
use daw_model::files::{self, AudioFile};
use daw_model::*;

fn audio_clip(source: impl Into<PathBuf>) -> std::sync::Arc<Clip> {
    Clip::new(
        "Take",
        TickTime(0),
        TickTime(960),
        ClipContent::Audio(AudioClip::new(source, SampleTime(0))),
    )
    .into()
}

/// A project whose audio track plays each of `sources`, once per entry.
fn project_with(sources: &[PathBuf]) -> Project {
    let mut p = Project::new("Song");
    let mut t = Track::new(TrackKind::Audio, "Audio");
    for s in sources {
        t.clips.push(audio_clip(s.clone()));
    }
    p.tracks.push(t.into());
    p
}

fn sources(p: &Project) -> Vec<PathBuf> {
    p.tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .filter_map(|c| match &c.content {
            ClipContent::Audio(a) => Some(a.source.clone()),
            ClipContent::Midi(_) => None,
        })
        .collect()
}

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

/// A saved bundle `Song` in a temp dir, plus a folder outside it.
struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    outside: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Song");
    let outside = tmp.path().join("Samples");
    fs::create_dir_all(&outside).unwrap();
    Fixture {
        root,
        outside,
        _tmp: tmp,
    }
}

fn saved(fx: &Fixture, project: Project) -> Document {
    let mut doc = Document::new(project);
    doc.save_as(&fx.root).unwrap();
    doc
}

#[test]
fn lists_each_file_once_with_its_clips() {
    let fx = fixture();
    let kick = fx.outside.join("kick.wav");
    write(&kick, b"kick");
    let doc = saved(
        &fx,
        project_with(&[kick.clone(), "audio/a.wav".into(), kick.clone()]),
    );
    let all = files::audio_files(doc.project(), doc.bundle());
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].source, kick);
    assert_eq!(all[0].clips.len(), 2);
    assert_eq!(all[1].path, fx.root.join("audio/a.wav"));
}

#[test]
fn finds_missing_files_inside_and_outside_the_bundle() {
    let fx = fixture();
    let present = fx.outside.join("here.wav");
    write(&present, b"x");
    let gone = fx.outside.join("gone.wav");
    let doc = saved(
        &fx,
        project_with(&[present.clone(), gone.clone(), "audio/lost.wav".into()]),
    );
    let missing: Vec<PathBuf> = files::missing_audio(doc.project(), doc.bundle())
        .into_iter()
        .map(|f| f.source)
        .collect();
    assert_eq!(missing, vec![gone, PathBuf::from("audio/lost.wav")]);
}

#[test]
fn external_means_outside_the_bundle_root() {
    let fx = fixture();
    let doc = saved(&fx, project_with(&[]));
    let bundle = doc.bundle().unwrap();
    let file = |path: PathBuf| AudioFile {
        source: path.clone(),
        path,
        clips: Vec::new(),
    };
    assert!(file(fx.outside.join("a.wav")).is_external(Some(bundle)));
    assert!(!file(fx.root.join("audio/a.wav")).is_external(Some(bundle)));
    // An absolute path into the bundle is inside, `..` out of it is not.
    assert!(file(fx.root.join("audio/../../Samples/a.wav")).is_external(Some(bundle)));
    assert!(!file(fx.root.join("./audio/a.wav")).is_external(Some(bundle)));
    // A sibling whose name starts with the bundle's is outside.
    let sibling = fx.root.with_file_name("Song 2").join("a.wav");
    assert!(file(sibling).is_external(Some(bundle)));
    // With no bundle, every file is.
    assert!(file(fx.root.join("audio/a.wav")).is_external(None));
}

#[test]
fn source_for_is_relative_inside_the_bundle() {
    let fx = fixture();
    let doc = saved(&fx, project_with(&[]));
    let b = doc.bundle();
    assert_eq!(
        files::source_for(&fx.root.join("audio/x.wav"), b),
        PathBuf::from("audio/x.wav")
    );
    let out = fx.outside.join("x.wav");
    assert_eq!(files::source_for(&out, b), out);
    assert_eq!(files::source_for(&out, None), out);
}

#[test]
fn relink_points_every_clip_at_the_new_file_and_undoes() {
    let fx = fixture();
    let old = fx.outside.join("old/take.wav");
    let other = fx.outside.join("other.wav");
    let mut doc = saved(
        &fx,
        project_with(&[old.clone(), other.clone(), old.clone()]),
    );
    let new = fx.outside.join("new/take.wav");
    let cmd = files::relink(doc.project(), &[(old.clone(), new.clone())], "Relink Audio").unwrap();
    doc.execute(cmd).unwrap();
    assert_eq!(
        sources(doc.project()),
        vec![new.clone(), other.clone(), new]
    );
    assert_eq!(doc.history().undo_label().as_deref(), Some("Relink Audio"));
    doc.undo().unwrap();
    assert_eq!(
        sources(doc.project()),
        vec![old.clone(), other, old.clone()]
    );
    // Nothing to change: no edit.
    assert!(files::relink(doc.project(), &[(old.clone(), old)], "Relink Audio").is_none());
}

#[test]
fn relinked_project_saves_and_loads() {
    let fx = fixture();
    let old = fx.outside.join("old.wav");
    let mut doc = saved(&fx, project_with(std::slice::from_ref(&old)));
    let cmd = files::relink(
        doc.project(),
        &[(old, PathBuf::from("audio/new.wav"))],
        "Relink Audio",
    )
    .unwrap();
    doc.execute(cmd).unwrap();
    doc.save().unwrap();
    let back = Document::open(&fx.root).unwrap();
    assert_eq!(
        sources(back.project()),
        vec![PathBuf::from("audio/new.wav")]
    );
}

#[test]
fn find_in_folder_matches_by_file_name() {
    let fx = fixture();
    let moved = fx.outside.join("moved");
    write(&moved.join("a.wav"), b"a");
    write(&moved.join("c.wav"), b"c");
    let doc = saved(
        &fx,
        project_with(&[
            PathBuf::from("/gone/a.wav"),
            PathBuf::from("/gone/b.wav"),
            PathBuf::from("audio/c.wav"),
        ]),
    );
    let missing = files::missing_audio(doc.project(), doc.bundle());
    let found = files::find_in_folder(&missing, &moved);
    assert_eq!(
        found,
        vec![
            (PathBuf::from("/gone/a.wav"), moved.join("a.wav")),
            (PathBuf::from("audio/c.wav"), moved.join("c.wav")),
        ]
    );
}

#[test]
fn collect_copies_external_files_into_the_bundle() {
    let fx = fixture();
    let kick = fx.outside.join("kick.wav");
    let snare = fx.outside.join("drums/snare.wav");
    // Same name as a file already in the bundle: gets a new name.
    let clash = fx.outside.join("drums/inside.wav");
    write(&kick, b"kick");
    write(&snare, b"snare");
    write(&clash, b"clash");
    let gone = fx.outside.join("gone.wav");
    let mut doc = saved(
        &fx,
        project_with(&[
            kick.clone(),
            snare.clone(),
            "audio/inside.wav".into(),
            clash.clone(),
            gone.clone(),
            // The same file spelled another way copies once.
            fx.outside.join("drums/../kick.wav"),
        ]),
    );
    write(&fx.root.join("audio/inside.wav"), b"inside");

    let collected = files::collect_files(doc.project(), doc.bundle().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(collected.copies.len(), 3);
    doc.execute(collected.command).unwrap();
    let after = sources(doc.project());
    assert_eq!(
        after,
        vec![
            PathBuf::from("audio/kick.wav"),
            PathBuf::from("audio/snare.wav"),
            PathBuf::from("audio/inside.wav"),
            PathBuf::from("audio/inside-1.wav"),
            // Missing files stay as they are, for relinking.
            gone.clone(),
            PathBuf::from("audio/kick.wav"),
        ]
    );
    assert_eq!(fs::read(fx.root.join("audio/kick.wav")).unwrap(), b"kick");
    assert_eq!(
        fs::read(fx.root.join("audio/inside.wav")).unwrap(),
        b"inside"
    );
    assert_eq!(
        fs::read(fx.root.join("audio/inside-1.wav")).unwrap(),
        b"clash"
    );
    // The originals stay where they were.
    assert!(kick.is_file() && snare.is_file() && clash.is_file());
    assert_eq!(
        doc.history().undo_label().as_deref(),
        Some("Collect Audio Files")
    );

    // Nothing left outside.
    assert!(
        files::collect_files(doc.project(), doc.bundle().unwrap())
            .unwrap()
            .is_none()
    );

    doc.undo().unwrap();
    assert_eq!(sources(doc.project())[0], kick);
}

#[test]
fn failed_collect_removes_the_copies_it_made() {
    let fx = fixture();
    let a = fx.outside.join("a.wav");
    let b = fx.outside.join("b.wav");
    write(&a, b"a");
    write(&b, b"b");
    let doc = saved(&fx, project_with(&[a, b.clone()]));
    // Copying b fails: it is unreadable (for any user but root).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&b, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read(&b).is_ok() {
            // Running as root: permissions don't stop the read.
            return;
        }
        let err = files::collect_files(doc.project(), doc.bundle().unwrap());
        assert!(matches!(err, Err(FormatError::Io { .. })));
        let left: Vec<_> = fs::read_dir(fx.root.join("audio")).unwrap().collect();
        assert!(left.is_empty(), "copies left behind: {left:?}");
    }
}

#[test]
fn relink_refuses_an_empty_path() {
    let fx = fixture();
    let mut doc = saved(&fx, project_with(&["audio/a.wav".into()]));
    let cmd = files::relink(
        doc.project(),
        &[(PathBuf::from("audio/a.wav"), PathBuf::new())],
        "Relink Audio",
    )
    .unwrap();
    assert!(doc.execute(cmd).is_err());
    assert_eq!(sources(doc.project()), vec![PathBuf::from("audio/a.wav")]);
}

#[test]
fn recovered_document_rewrites_its_own_autosave() {
    let fx = fixture();
    let doc = saved(&fx, project_with(&[]));
    let bundle = doc.bundle().unwrap().clone();
    bundle.write_autosave(&Project::new("Unsaved")).unwrap();
    let mut back = Document::recover(&fx.root).unwrap();
    // Someone removes the autosave it came from: the next tick puts it back.
    bundle.discard_autosave().unwrap();
    assert!(back.autosave().unwrap());
    assert_eq!(bundle.load_autosave().unwrap().name, "Unsaved");

    let mut untitled = Document::recovered(Project::new("Scratch"));
    assert!(untitled.is_modified());
    // No bundle to autosave into; the app keeps that one itself.
    assert!(!untitled.autosave().unwrap());
}
