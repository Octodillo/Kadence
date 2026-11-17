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

    let collected = files::collect_files(doc.project(), doc.bundle().unwrap(), |_| true)
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
        files::collect_files(doc.project(), doc.bundle().unwrap(), |_| true)
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
        let err = files::collect_files(doc.project(), doc.bundle().unwrap(), |_| true);
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

#[test]
fn collect_leaves_out_what_is_not_audio() {
    let fx = fixture();
    let kick = fx.outside.join("kick.wav");
    let secret = fx.outside.join("id_ed25519");
    write(&kick, b"kick");
    write(&secret, b"secret");
    let mut doc = saved(&fx, project_with(&[kick, secret.clone()]));
    let is_audio = |p: &Path| p.extension().is_some();
    let collected = files::collect_files(doc.project(), doc.bundle().unwrap(), is_audio)
        .unwrap()
        .unwrap();
    assert_eq!(collected.copies, [fx.root.join("audio/kick.wav")]);
    doc.execute(collected.command).unwrap();
    // The clip still names the file; nothing of it is in the bundle.
    assert_eq!(sources(doc.project())[1], secret);
    assert!(!fx.root.join("audio/id_ed25519").exists());
    assert!(
        files::collect_files(doc.project(), doc.bundle().unwrap(), is_audio)
            .unwrap()
            .is_none()
    );
}

#[cfg(unix)]
#[test]
fn save_as_copies_only_what_is_really_in_the_audio_folder() {
    use std::os::unix::fs::symlink;
    let fx = fixture();
    let secret = fx.outside.join("id_ed25519");
    write(&secret, b"secret");
    let mut doc = saved(&fx, project_with(&["audio/tone.wav".into()]));
    let audio = fx.root.join("audio");
    write(&audio.join("real.wav"), b"real");
    symlink(&secret, audio.join("tone.wav")).unwrap();
    symlink("../../Samples/id_ed25519", audio.join("near.wav")).unwrap();
    symlink("real.wav", audio.join("alias.wav")).unwrap();
    symlink("nowhere.wav", audio.join("dangling.wav")).unwrap();
    symlink(&fx.outside, audio.join("folder")).unwrap();

    let new = fx.outside.join("deeper/Copy");
    doc.save_as(&new).unwrap();
    let new_audio = new.join("audio");
    assert_eq!(fs::read(new_audio.join("real.wav")).unwrap(), b"real");
    for name in ["tone.wav", "near.wav"] {
        let copy = new_audio.join(name);
        // Still a link to the same file, not a copy of what is in it.
        assert!(copy.symlink_metadata().unwrap().is_symlink(), "{name}");
        assert_eq!(fs::read(&copy).unwrap(), b"secret", "{name}");
    }
    // A link to a file of the folder is that file: it must not go on
    // pointing into the old bundle.
    let alias = new_audio.join("alias.wav");
    assert!(alias.symlink_metadata().unwrap().is_file());
    assert_eq!(fs::read(alias).unwrap(), b"real");
    for name in ["dangling.wav", "folder"] {
        assert!(new_audio.join(name).symlink_metadata().is_err(), "{name}");
    }
}

#[cfg(unix)]
#[test]
fn save_as_copies_nothing_through_a_linked_audio_folder() {
    let fx = fixture();
    write(&fx.outside.join("id_ed25519"), b"secret");
    write(&fx.outside.join("unused.wav"), b"unused");
    let mut doc = saved(&fx, project_with(&["audio/id_ed25519".into()]));
    fs::remove_dir(fx.root.join("audio")).unwrap();
    std::os::unix::fs::symlink(&fx.outside, fx.root.join("audio")).unwrap();
    let new = fx._tmp.path().join("Copy");
    doc.save_as(&new).unwrap();
    // The folder is not walked, and the file a clip plays through it comes
    // across as a link to it, as a linked file would.
    assert!(new.join("audio/unused.wav").symlink_metadata().is_err());
    let played = new.join("audio/id_ed25519");
    assert!(played.symlink_metadata().unwrap().is_symlink());
    assert_eq!(fs::read(played).unwrap(), b"secret");
}

#[test]
fn save_as_brings_every_file_of_the_bundle_that_plays() {
    let fx = fixture();
    let sources: Vec<PathBuf> = [
        "audio/take.wav",
        "audio/drums/kick.wav",
        "samples/one/snare.wav",
        "samples/../audio/drums/kick.wav",
    ]
    .map(PathBuf::from)
    .into();
    let outside = fx.outside.join("far.wav");
    write(&outside, b"far");
    let mut all = sources.clone();
    all.push(outside.clone());
    all.push("../Samples/far.wav".into());
    let mut doc = saved(&fx, project_with(&all));
    let content = |s: &Path| s.file_name().unwrap().as_encoded_bytes().to_vec();
    for s in &sources {
        write(&fx.root.join(s), &content(s));
    }
    // In audio/ but played by nothing, and elsewhere and played by nothing.
    write(&fx.root.join("audio/old/unused.wav"), b"unused");
    write(&fx.root.join("notes/lyrics.txt"), b"la");

    let new = fx._tmp.path().join("Song v2");
    doc.save_as(&new).unwrap();
    assert_eq!(doc.bundle().unwrap().root(), new);
    assert!(files::missing_audio(doc.project(), doc.bundle()).is_empty());
    for s in &sources[..3] {
        let copy = new.join(s);
        assert!(copy.symlink_metadata().unwrap().is_file(), "{s:?}");
        assert_eq!(fs::read(copy).unwrap(), content(s));
    }
    assert_eq!(
        fs::read(new.join("audio/old/unused.wav")).unwrap(),
        b"unused"
    );
    assert!(!new.join("notes").exists());
    assert!(!new.join("far.wav").exists());
}

#[test]
fn failed_save_as_takes_back_the_folders_it_made() {
    let fx = fixture();
    let mut doc = saved(
        &fx,
        project_with(&["audio/drums/kick.wav".into(), "samples/snare.wav".into()]),
    );
    write(&fx.root.join("audio/drums/kick.wav"), b"kick");
    write(&fx.root.join("samples/snare.wav"), b"snare");

    // A file of that name is already where one would go: refused before
    // anything is copied.
    let taken = fx._tmp.path().join("Taken");
    write(&taken.join("samples/snare.wav"), b"theirs");
    assert!(doc.save_as(&taken).is_err());
    assert_eq!(
        fs::read(taken.join("samples/snare.wav")).unwrap(),
        b"theirs"
    );
    assert!(!taken.join("audio/drums").exists());

    // The copy fails part-way: a folder cannot be made where a file is.
    let blocked = fx._tmp.path().join("Blocked");
    write(&blocked.join("samples"), b"a file");
    assert!(doc.save_as(&blocked).is_err());
    assert!(!blocked.join("audio/drums").exists(), "left half a copy");
    assert_eq!(fs::read(blocked.join("samples")).unwrap(), b"a file");
    assert_eq!(doc.bundle().unwrap().root(), fx.root);
    assert!(!blocked.join("Blocked.kadence").exists());
}
