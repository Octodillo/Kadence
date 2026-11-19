//! What Save As brings from the old bundle into the new one.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::bundle::{Bundle, copy_new};
use crate::error::FormatError;
use crate::files::{StateFiles, audio_files, relative_inside, remove_copies};
use crate::model::Project;

/// The files and folders a carry made, so a failed Save As can take them
/// back and a retry into the same folder isn't refused over a half-finished
/// copy.
#[derive(Debug, Default)]
pub(crate) struct Made {
    files: Vec<PathBuf>,
    /// Outermost first.
    dirs: Vec<PathBuf>,
}

impl Made {
    /// Removes what was made. Each file was checked not to exist before,
    /// and only folders that are empty again go, so nothing of the user's
    /// is lost.
    pub(crate) fn remove(&self) {
        remove_copies(&self.files);
        for d in self.dirs.iter().rev() {
            let _ = fs::remove_dir(d);
        }
    }
}

/// How one file comes across.
enum Carry {
    /// Copy the file's content.
    Copy,
    /// Make a link to this file.
    Link(PathBuf),
}

/// One file to bring across: where it is, and its path relative to both
/// bundles.
struct Entry {
    from: PathBuf,
    rel: PathBuf,
    carry: Carry,
}

/// Brings the audio of `old` into `new`: everything in its `audio/` folder,
/// sub-folders included, and any other file in the bundle that a clip of
/// `project` or a device (read with `state`) plays, except the project files and backups Kadence keeps there. Each file keeps its place, so the clips' sources hold.
/// Refuses before copying anything if a file is already at one of those
/// places, since clips may use it. On failure nothing it made is left.
///
/// A bundle from someone else can hold a link to any file on this machine,
/// and the copy may be shared back, so only what is really in `old` is
/// copied. A link to a file elsewhere is made again as a link to that file;
/// a link to another file in `old` is copied as that file, since its content
/// is in the bundle anyway. Folders reached through a link are not walked,
/// and a clip's file reached through one comes across as a link.
///
/// If the old bundle's folder is gone (moved or renamed while open) and
/// clips play files in it, that is an error naming the folder: the new
/// bundle would be saved without its audio.
pub(crate) fn audio(
    project: &Project,
    old: &Bundle,
    new: &Bundle,
    state: &dyn StateFiles,
) -> Result<Made, FormatError> {
    let played = audio_files(project, Some(old), state);
    let root = match old.root().canonicalize() {
        Ok(root) => root,
        Err(e) if played.iter().any(|f| f.source.is_relative()) => {
            return Err(FormatError::io(old.root())(e));
        }
        Err(_) => return Ok(Made::default()),
    };
    let mut entries = Vec::new();
    walk(
        &root,
        &old.audio_dir(),
        Path::new(crate::bundle::AUDIO_DIR),
        &mut entries,
    )?;
    let mut planned: HashSet<PathBuf> = entries.iter().map(|e| e.rel.clone()).collect();
    for file in played {
        if let Some(rel) = relative_inside(old.root(), &file.path)
            && file.source.is_relative()
            // A project from someone else can name these as a clip's source.
            && !Bundle::is_own_file(&rel)
            && !planned.contains(&rel)
            && let Some(carry) = carry(&root, &file.path)
        {
            planned.insert(rel.clone());
            entries.push(Entry {
                from: file.path,
                rel,
                carry,
            });
        }
    }
    for e in &entries {
        let dest = new.root().join(&e.rel);
        if dest.symlink_metadata().is_ok() {
            return Err(FormatError::io(dest)(
                std::io::ErrorKind::AlreadyExists.into(),
            ));
        }
    }
    let mut made = Made::default();
    for e in entries {
        if let Err(err) = bring(&e, new.root(), &mut made) {
            made.remove();
            return Err(err);
        }
    }
    Ok(made)
}

/// Lists the files under `dir`, whose path in the bundle is `rel`. A `dir`
/// that is a link, or not a folder, lists nothing.
fn walk(root: &Path, dir: &Path, rel: &Path, out: &mut Vec<Entry>) -> Result<(), FormatError> {
    if !dir.symlink_metadata().is_ok_and(|m| m.is_dir()) {
        return Ok(());
    }
    for entry in fs::read_dir(dir).map_err(FormatError::io(dir))? {
        let entry = entry.map_err(FormatError::io(dir))?;
        let from = entry.path();
        let rel = rel.join(entry.file_name());
        // Of the entry itself: a link is not followed.
        let kind = entry.file_type().map_err(FormatError::io(&from))?;
        if kind.is_dir() {
            walk(root, &from, &rel, out)?;
        } else if let Some(carry) = carry(root, &from) {
            out.push(Entry { from, rel, carry });
        }
    }
    Ok(())
}

/// How to bring the file at `path` across, given the old bundle's real
/// location `root`: `None` to leave it out, when it does not lead to a file.
fn carry(root: &Path, path: &Path) -> Option<Carry> {
    let target = path.canonicalize().ok().filter(|t| t.is_file())?;
    Some(if target.starts_with(root) {
        Carry::Copy
    } else {
        Carry::Link(target)
    })
}

/// Brings one file into the bundle at `root`, making the folders it needs.
fn bring(entry: &Entry, root: &Path, made: &mut Made) -> Result<(), FormatError> {
    let dest = root.join(&entry.rel);
    let mut dir = root.to_path_buf();
    for part in entry.rel.parent().into_iter().flat_map(Path::iter) {
        dir.push(part);
        match fs::create_dir(&dir) {
            Ok(()) => made.dirs.push(dir.clone()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(FormatError::io(dir)(e)),
        }
    }
    match &entry.carry {
        // Makes the file anew and leaves nothing of a failed copy.
        Carry::Copy => copy_new(&entry.from, &dest)?,
        Carry::Link(target) => link(target, &dest).map_err(FormatError::io(&dest))?,
    }
    made.files.push(dest);
    Ok(())
}

#[cfg(unix)]
fn link(target: &Path, dest: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, dest)
}

/// Where links can't be made, the file is refused rather than copied.
#[cfg(not(unix))]
fn link(_target: &Path, _dest: &Path) -> std::io::Result<()> {
    Err(std::io::ErrorKind::Unsupported.into())
}
