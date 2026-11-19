//! The audio files a project plays: which are missing, which live outside
//! the bundle, and the edits that relink or collect them.
//!
//! Clips name their files in the model. Devices can name files too, inside
//! their saved state (a sampler's samples), which only the device's own
//! crate can read: callers pass a [`StateFiles`] that does.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use crate::bundle::Bundle;
use crate::commands::{Batch, ClipField, DeviceField, SetClip, SetDevice};
use crate::error::FormatError;
use crate::id::{ClipId, DeviceId};
use crate::model::{ClipContent, Device, Project, StateBlob};

/// Reads and rewrites the files named in devices' saved state, which the
/// model cannot read itself.
pub trait StateFiles {
    /// The files `device`'s saved state plays, as saved, each once. Empty
    /// for a device that names none, or whose state can't be read.
    fn files(&self, device: &Device) -> Vec<PathBuf>;

    /// `device`'s saved state with each file `to` gives a new name for
    /// renamed. `None` when nothing changes.
    fn relinked(&self, device: &Device, to: &dyn Fn(&Path) -> Option<PathBuf>)
    -> Option<StateBlob>;
}

/// A [`StateFiles`] that finds no files in any state: only clips' files
/// count.
pub struct ClipsOnly;

impl StateFiles for ClipsOnly {
    fn files(&self, _: &Device) -> Vec<PathBuf> {
        Vec::new()
    }

    fn relinked(&self, _: &Device, _: &dyn Fn(&Path) -> Option<PathBuf>) -> Option<StateBlob> {
        None
    }
}

/// One file that the project plays, as stored in an audio clip's
/// [`source`](crate::AudioClip::source) or a device's saved state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioFile {
    /// As stored in the clips and devices.
    pub source: PathBuf,
    /// Where it is on disk: resolved against the bundle. A relative source
    /// with no bundle stays relative.
    pub path: PathBuf,
    /// The clips that play it, in project order.
    pub clips: Vec<ClipId>,
    /// The devices whose saved state plays it (samplers), in project order.
    pub devices: Vec<DeviceId>,
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

/// Every file audio clips and devices (read with `state`) play, each once:
/// clips' files in the order clips first use them, then the files only
/// devices play.
pub fn audio_files(
    project: &Project,
    bundle: Option<&Bundle>,
    state: &dyn StateFiles,
) -> Vec<AudioFile> {
    let mut files: Vec<AudioFile> = Vec::new();
    let mut index: HashMap<PathBuf, usize> = HashMap::new();
    let mut entry = |source: &Path| -> usize {
        *index.entry(source.to_path_buf()).or_insert_with(|| {
            files.push(AudioFile {
                source: source.to_path_buf(),
                path: match bundle {
                    Some(b) => b.resolve(source),
                    None => source.to_path_buf(),
                },
                clips: Vec::new(),
                devices: Vec::new(),
            });
            files.len() - 1
        })
    };
    let mut clip_uses = Vec::new();
    let mut device_uses = Vec::new();
    for track in project.all_tracks() {
        for clip in &track.clips {
            if let ClipContent::Audio(a) = &clip.content {
                clip_uses.push((entry(&a.source), clip.id));
            }
        }
    }
    for track in project.all_tracks() {
        for device in &track.devices {
            for source in state.files(device) {
                device_uses.push((entry(&source), device.id));
            }
        }
    }
    for (i, clip) in clip_uses {
        files[i].clips.push(clip);
    }
    for (i, device) in device_uses {
        files[i].devices.push(device);
    }
    files
}

/// The files the project plays that are not on disk.
pub fn missing_audio(
    project: &Project,
    bundle: Option<&Bundle>,
    state: &dyn StateFiles,
) -> Vec<AudioFile> {
    audio_files(project, bundle, state)
        .into_iter()
        .filter(|f| !f.exists())
        .collect()
}

/// The files outside the bundle that are on disk, so
/// [`collect_files`] can copy them in.
pub fn external_audio(
    project: &Project,
    bundle: &Bundle,
    state: &dyn StateFiles,
) -> Vec<AudioFile> {
    audio_files(project, Some(bundle), state)
        .into_iter()
        .filter(|f| f.is_external(Some(bundle)) && f.exists())
        .collect()
}

/// `path` relative to `root` if it is inside it, written with `/`. Compares
/// the paths as written, after removing `.` and `..`, so it never touches
/// the disk.
pub(crate) fn relative_inside(root: &Path, path: &Path) -> Option<PathBuf> {
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

/// The edit that points every clip and device (read with `state`) playing
/// one of the `from` sources at its new file. `None` when nothing changes.
/// Undoes as one step called `label`.
pub fn relink(
    project: &Project,
    state: &dyn StateFiles,
    changes: &[(PathBuf, PathBuf)],
    label: &str,
) -> Option<Batch> {
    let to: HashMap<&Path, &Path> = changes
        .iter()
        .filter(|(from, to)| from != to)
        .map(|(f, t)| (f.as_path(), t.as_path()))
        .collect();
    let new_name = |source: &Path| to.get(source).map(|p| p.to_path_buf());
    let mut commands: Vec<Box<dyn crate::Command>> = Vec::new();
    for track in project.all_tracks() {
        for clip in &track.clips {
            if let ClipContent::Audio(a) = &clip.content
                && let Some(new) = new_name(&a.source)
            {
                commands.push(Box::new(SetClip::new(clip.id, ClipField::AudioSource(new))));
            }
        }
        for device in &track.devices {
            if let Some(blob) = state.relinked(device, &new_name) {
                commands.push(Box::new(SetDevice {
                    id: device.id,
                    field: DeviceField::State(Some(blob)),
                }));
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
    /// Points the clips and devices at the copies. Undoes as one "Collect Audio Files" step.
    pub command: Batch,
    /// The copies in the bundle's `audio/` folder. If running `command`
    /// fails, remove them with [`remove_copies`].
    pub copies: Vec<PathBuf>,
}

/// Removes files that [`collect_files`] or a Save As made. Each was a new
/// file, so nothing of the user's is lost.
pub fn remove_copies(copies: &[PathBuf]) {
    for c in copies {
        let _ = std::fs::remove_file(c);
    }
}

/// Copies every external file clips and devices play into the bundle's
/// `audio/` folder and returns the edit that points them at the copies. `None` when
/// there is nothing to copy. All or nothing: if one copy fails, the ones
/// made so far are removed again. Missing files are skipped; relink them
/// first.
///
/// So are files `is_audio` turns down. A project from someone else can name
/// any file on this machine as a clip's source, and the bundle it would be
/// copied into may be shared back: only what opens as audio is copied.
pub fn collect_files(
    project: &Project,
    bundle: &Bundle,
    state: &dyn StateFiles,
    is_audio: impl Fn(&Path) -> bool,
) -> Result<Option<Collected>, FormatError> {
    let mut external = external_audio(project, bundle, state);
    external.retain(|f| is_audio(&f.path));
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
    let Some(command) = relink(project, state, &changes, "Collect Audio Files") else {
        // Only when `state` lists files it then cannot rename.
        remove_copies(&copies);
        return Ok(None);
    };
    Ok(Some(Collected { command, copies }))
}
