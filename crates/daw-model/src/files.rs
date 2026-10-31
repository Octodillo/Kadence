//! The audio files a project plays: which are missing, which live outside
//! the bundle, and the edits that relink or collect them.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use crate::bundle::Bundle;
use crate::commands::{Batch, ClipField, SetClip};
use crate::error::FormatError;
use crate::id::ClipId;
use crate::model::{ClipContent, Project};

/// One file that audio clips play, as stored in [`crate::AudioClip::source`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioFile {
    /// As stored in the clips.
    pub source: PathBuf,
    /// Where it is on disk: resolved against the bundle. A relative source
    /// with no bundle stays relative.
    pub path: PathBuf,
    /// The clips that play it, in project order.
    pub clips: Vec<ClipId>,
}

impl AudioFile {
    /// Whether the file is there to play.
    pub fn exists(&self) -> bool {
        self.path.is_file()
    }

    /// Whether it lives outside the bundle, so moving or sharing the bundle
    /// would lose it. Without a bundle, every file does.
    pub fn is_external(&self, bundle: Option<&Bundle>) -> bool {
        bundle.is_none_or(|b| relative_inside(b.root(), &self.path).is_none())
    }
}

/// Every file audio clips play, each once, in the order clips first use it.
pub fn audio_files(project: &Project, bundle: Option<&Bundle>) -> Vec<AudioFile> {
    let mut files: Vec<AudioFile> = Vec::new();
    let mut index: HashMap<PathBuf, usize> = HashMap::new();
    for track in project.all_tracks() {
        for clip in &track.clips {
            let ClipContent::Audio(a) = &clip.content else {
                continue;
            };
            let i = *index.entry(a.source.clone()).or_insert_with(|| {
                files.push(AudioFile {
                    source: a.source.clone(),
                    path: match bundle {
                        Some(b) => b.resolve(&a.source),
                        None => a.source.clone(),
                    },
                    clips: Vec::new(),
                });
                files.len() - 1
            });
            files[i].clips.push(clip.id);
        }
    }
    files
}

/// The files clips play that are not on disk.
pub fn missing_audio(project: &Project, bundle: Option<&Bundle>) -> Vec<AudioFile> {
    audio_files(project, bundle)
        .into_iter()
        .filter(|f| !f.exists())
        .collect()
}

/// The files outside the bundle that are on disk, so
/// [`collect_files`] can copy them in.
pub fn external_audio(project: &Project, bundle: &Bundle) -> Vec<AudioFile> {
    audio_files(project, Some(bundle))
        .into_iter()
        .filter(|f| f.is_external(Some(bundle)) && f.exists())
        .collect()
}

/// `path` relative to `root` if it is inside it, written with `/`. Compares
/// the paths as written, after removing `.` and `..`, so it never touches
/// the disk.
fn relative_inside(root: &Path, path: &Path) -> Option<PathBuf> {
    let root = normalize(root);
    let path = normalize(path);
    let rest = path.strip_prefix(&root).ok()?;
    if rest.as_os_str().is_empty() {
        return None;
    }
    let parts: Vec<_> = rest.iter().map(|p| p.to_string_lossy()).collect();
    Some(PathBuf::from(parts.join("/")))
}

/// Removes `.` and folds `..` into the component before it.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(c);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// How a clip should store `file`: relative when it is inside the bundle (so
/// the bundle can move), absolute otherwise.
pub fn source_for(file: &Path, bundle: Option<&Bundle>) -> PathBuf {
    bundle
        .and_then(|b| relative_inside(b.root(), file))
        .unwrap_or_else(|| file.to_path_buf())
}

/// The edit that points every clip playing one of the `from` sources at its
/// new file. `None` when no clip changes. Undoes as one "Relink Audio" step.
pub fn relink(project: &Project, changes: &[(PathBuf, PathBuf)], label: &str) -> Option<Batch> {
    let to: HashMap<&Path, &Path> = changes
        .iter()
        .filter(|(from, to)| from != to)
        .map(|(f, t)| (f.as_path(), t.as_path()))
        .collect();
    let mut commands: Vec<Box<dyn crate::Command>> = Vec::new();
    for track in project.all_tracks() {
        for clip in &track.clips {
            if let ClipContent::Audio(a) = &clip.content
                && let Some(new) = to.get(a.source.as_path())
            {
                commands.push(Box::new(SetClip::new(
                    clip.id,
                    ClipField::AudioSource(new.to_path_buf()),
                )));
            }
        }
    }
    (!commands.is_empty()).then(|| Batch::new(label, commands))
}

/// For each missing file, a file of the same name in `folder`, if there is
/// one. Lets the user find a whole folder of moved files by locating one.
pub fn find_in_folder(missing: &[AudioFile], folder: &Path) -> Vec<(PathBuf, PathBuf)> {
    missing
        .iter()
        .filter_map(|f| {
            let candidate = folder.join(f.path.file_name()?);
            candidate.is_file().then(|| (f.source.clone(), candidate))
        })
        .collect()
}

/// The result of [`collect_files`]: the edit to run and the copies it made.
#[derive(Debug)]
pub struct Collected {
    /// Points the clips at the copies. Undoes as one "Collect Audio Files" step.
    pub command: Batch,
    /// The copies in the bundle's `audio/` folder. If running `command`
    /// fails, remove them with [`remove_copies`].
    pub copies: Vec<PathBuf>,
}

/// Removes files [`collect_files`] made. Each was a new file, so nothing of
/// the user's is lost.
pub fn remove_copies(copies: &[PathBuf]) {
    for c in copies {
        let _ = std::fs::remove_file(c);
    }
}

/// Copies every external file clips play into the bundle's `audio/` folder
/// and returns the edit that points the clips at the copies. `None` when
/// nothing is outside the bundle. All or nothing: if one copy fails, the
/// ones made so far are removed again. Missing files are skipped; relink
/// them first.
pub fn collect_files(project: &Project, bundle: &Bundle) -> Result<Option<Collected>, FormatError> {
    let external = external_audio(project, bundle);
    if external.is_empty() {
        return Ok(None);
    }
    let mut changes = Vec::with_capacity(external.len());
    let mut copies = Vec::with_capacity(external.len());
    // Two spellings of one path (a `..` in one) copy once.
    let mut copied: HashMap<PathBuf, PathBuf> = HashMap::new();
    for f in &external {
        let key = normalize(&f.path);
        if let Some(rel) = copied.get(&key) {
            changes.push((f.source.clone(), rel.clone()));
            continue;
        }
        match bundle.import_audio(&f.path) {
            Ok(rel) => {
                copies.push(bundle.resolve(&rel));
                copied.insert(key, rel.clone());
                changes.push((f.source.clone(), rel));
            }
            Err(e) => {
                remove_copies(&copies);
                return Err(e);
            }
        }
    }
    let command =
        relink(project, &changes, "Collect Audio Files").expect("external files have clips");
    Ok(Some(Collected { command, copies }))
}
