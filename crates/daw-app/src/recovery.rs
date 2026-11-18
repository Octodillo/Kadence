//! Getting unsaved work back after Kadence quit without saving.
//!
//! A project with a bundle autosaves into it (see `daw_model::Document`).
//! A project that was never saved has no bundle, so it autosaves into a
//! scratch folder of its own under the user's data folder, one per running
//! Kadence. At startup, autosaves that no running Kadence owns are offered
//! for recovery.
//!
//! Audio recorded into a never-saved project goes into the scratch folder
//! too, under `audio/` as in a bundle, and its clips name it relative to the
//! folder. So the takes are recovered, discarded or removed together with
//! the autosave, and saving the project only has to bring them along.
//!
//! Ownership is an advisory lock (`flock`) on the folder: on the scratch
//! folder for as long as Kadence runs, and on the bundle folder of the open
//! project. The kernel drops it when the process ends, crash or not, so a
//! held lock means "in use", never "left over".

use std::fs::{self, File};
use std::io;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use daw_model::{Bundle, FormatError, Project, bundle};

/// File name of a never-saved project's autosave in its scratch folder.
const SCRATCH_FILE: &str = "Untitled.kadence.autosave";

/// The folder scratch folders go in: `recovery` in Kadence's data folder
/// (see [`crate::paths::data_dir`]).
pub fn default_base() -> Option<PathBuf> {
    crate::paths::data_dir().map(|data| data.join("recovery"))
}

/// Where the audio recorded into the project kept in `root` goes: a
/// bundle's `audio/` folder, and the same in a scratch folder.
pub fn audio_dir(root: &Path) -> PathBuf {
    root.join(bundle::AUDIO_DIR)
}

/// Brings the audio recorded into the unsaved project kept in `root` into
/// the `audio/` folder of the bundle at `bundle_root`, under the same names,
/// so the clips' relative paths stay right. Returns the files it made. All
/// or nothing: it refuses before making any if a name is taken, and removes
/// what it made if one fails.
pub fn bring_audio(root: &Path, bundle_root: &Path) -> Result<Vec<PathBuf>, FormatError> {
    let from = audio_dir(root);
    let to = audio_dir(bundle_root);
    let names: Vec<_> = match fs::read_dir(&from) {
        Ok(entries) => entries
            .flatten()
            .filter(|e| e.path().is_file())
            .map(|e| e.file_name())
            .collect(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(FormatError::Io {
                path: from,
                source: e,
            });
        }
    };
    if let Some(taken) = names.iter().map(|n| to.join(n)).find(|p| p.exists()) {
        return Err(FormatError::Io {
            path: taken,
            source: io::ErrorKind::AlreadyExists.into(),
        });
    }
    fs::create_dir_all(&to).map_err(|e| FormatError::Io {
        path: to.clone(),
        source: e,
    })?;
    let mut made = Vec::with_capacity(names.len());
    for name in &names {
        let (src, dst) = (from.join(name), to.join(name));
        // A link costs nothing where both folders are on one disk; the
        // scratch folder's name for the file is removed after the save.
        let brought = fs::hard_link(&src, &dst).or_else(|_| fs::copy(&src, &dst).map(drop));
        if let Err(e) = brought {
            remove_files(&made);
            // A copy that failed half way.
            let _ = fs::remove_file(&dst);
            return Err(FormatError::Io {
                path: src,
                source: e,
            });
        }
        made.push(dst);
    }
    Ok(made)
}

/// Removes files [`bring_audio`] made.
fn remove_files(files: &[PathBuf]) {
    for f in files {
        let _ = fs::remove_file(f);
    }
}

/// Takes back what [`bring_audio`] put into `bundle_root`, after the save
/// it was for failed: the files, and the `audio/` folder if that leaves it
/// empty.
pub fn take_back_audio(files: &[PathBuf], bundle_root: &Path) {
    remove_files(files);
    // Only an empty folder goes.
    let _ = fs::remove_dir(audio_dir(bundle_root));
}

/// A folder of this process's own in the temporary folder, made by whoever
/// first writes into it and removed when dropped. It stands in for the
/// scratch folder while there is none (before the session starts, as in
/// tests and the self test), so an unsaved project's takes have a place.
#[derive(Debug)]
pub struct TempFolder(PathBuf);

impl TempFolder {
    pub fn new() -> Self {
        let id = uuid::Uuid::new_v4().simple().to_string();
        Self(std::env::temp_dir().join(format!("kadence-{}-{}", std::process::id(), &id[..8])))
    }

    pub fn dir(&self) -> &Path {
        &self.0
    }
}

impl Default for TempFolder {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TempFolder {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// An exclusive advisory lock on a folder, held until dropped.
#[derive(Debug)]
pub struct DirLock {
    _file: File,
    dir: PathBuf,
}

impl DirLock {
    /// Locks `dir`. `Ok(None)` when someone else (another Kadence, or
    /// another lock in this one) holds it.
    pub fn try_lock(dir: &Path) -> io::Result<Option<Self>> {
        let file = File::open(dir)?;
        // SAFETY: flock on a descriptor we own; it has no memory effects.
        let r = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if r == 0 {
            return Ok(Some(Self {
                _file: file,
                dir: dir.to_path_buf(),
            }));
        }
        let e = io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::EWOULDBLOCK) {
            Ok(None)
        } else {
            Err(e)
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// Whether someone holds a [`DirLock`] on `dir`. A folder that can't be
/// locked at all (a filesystem without locks) counts as free, so recovery
/// still works there.
pub fn is_locked(dir: &Path) -> bool {
    matches!(DirLock::try_lock(dir), Ok(None))
}

/// This Kadence's scratch folder, locked for as long as it lives.
#[derive(Debug)]
pub struct Scratch {
    lock: DirLock,
}

impl Scratch {
    /// Makes a new scratch folder under `base` and locks it.
    pub fn create(base: &Path) -> io::Result<Self> {
        fs::create_dir_all(base)?;
        let stamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let id = uuid::Uuid::new_v4().simple().to_string();
        let name = format!("{stamp}-{}", &id[..8]);
        // Made under a hidden name and renamed once locked, so a Kadence
        // starting at the same time never sees it unlocked and cleans it up.
        let tmp = base.join(format!(".{name}"));
        fs::create_dir(&tmp)?;
        let Some(lock) = DirLock::try_lock(&tmp)? else {
            return Err(io::ErrorKind::WouldBlock.into());
        };
        let dir = base.join(name);
        fs::rename(&tmp, &dir)?;
        Ok(Self {
            lock: DirLock { dir, ..lock },
        })
    }

    /// Takes over the folder of a crashed Kadence's scratch autosave, to
    /// keep autosaving recovered work there. `Ok(None)` when someone else
    /// holds it.
    pub fn adopt(dir: &Path) -> io::Result<Option<Self>> {
        Ok(DirLock::try_lock(dir)?.map(|lock| Self { lock }))
    }

    pub fn dir(&self) -> &Path {
        self.lock.dir()
    }

    pub fn file(&self) -> PathBuf {
        self.dir().join(SCRATCH_FILE)
    }

    pub fn write(&self, project: &Project) -> Result<(), FormatError> {
        bundle::write_project(&self.file(), project)
    }

    /// Deletes the autosave, if there is one.
    pub fn discard(&self) -> io::Result<()> {
        match fs::remove_file(self.file()) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Deletes the folder, at a clean exit.
    pub fn remove(self) {
        let dir = self.dir().to_path_buf();
        drop(self);
        let _ = fs::remove_dir_all(dir);
    }
}

/// Unsaved work that can be recovered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// The autosave file.
    pub autosave: PathBuf,
    /// The project's bundle; `None` for a project that was never saved.
    pub bundle: Option<PathBuf>,
    /// When it was last autosaved.
    pub modified: Option<SystemTime>,
}

impl Candidate {
    /// The folder a running Kadence would hold a lock on while it has this
    /// work open: the bundle, or the scratch folder.
    fn folder(&self) -> Option<&Path> {
        self.bundle.as_deref().or_else(|| self.autosave.parent())
    }

    /// The folder, if a running Kadence has this work open: it was recovered
    /// there since it was offered here.
    pub fn in_use(&self) -> Option<PathBuf> {
        self.folder()
            .filter(|dir| is_locked(dir))
            .map(Path::to_path_buf)
    }

    /// The bundle's name; empty for a project that was never saved.
    pub fn name(&self) -> String {
        self.bundle
            .as_deref()
            .and_then(|b| Bundle::open(b).ok())
            .map(|b| b.name().to_owned())
            .unwrap_or_default()
    }

    /// Whether the work is no longer there: recovered and then saved or
    /// dropped by another Kadence since it was offered here.
    pub fn is_gone(&self) -> bool {
        !self.autosave.exists()
    }

    /// Deletes the autosave, and an orphaned scratch folder (with the audio
    /// recorded into its project) with it. Fine if it is gone already.
    pub fn discard(&self) -> io::Result<()> {
        let removed = match (&self.bundle, self.autosave.parent()) {
            (Some(_), _) => fs::remove_file(&self.autosave),
            (None, Some(dir)) => fs::remove_dir_all(dir),
            (None, None) => Ok(()),
        };
        match removed {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

/// Moves the takes out of an ownerless scratch folder that holds no
/// project to recover them into, to a folder named after it in `recordings`
/// next to `base`: audio nobody was asked about is not deleted. Returns
/// whether the scratch folder holds nothing worth keeping any more.
fn rescue_takes(scratch: &Path, base: &Path) -> bool {
    let takes = audio_dir(scratch);
    let has_takes = fs::read_dir(&takes).is_ok_and(|mut entries| entries.next().is_some());
    if !has_takes {
        return true;
    }
    let Some(name) = scratch.file_name() else {
        return false;
    };
    let rescued = base.with_file_name("recordings");
    fs::create_dir_all(&rescued).is_ok() && fs::rename(&takes, rescued.join(name)).is_ok()
}

/// Autosaves nobody owns: from scratch folders under `base` and from the
/// `recent` bundles. Newest first. Scratch folders a crash left without an
/// autosave are removed on the way, after their takes are moved to safety
/// (see [`rescue_takes`]).
pub fn find(base: Option<&Path>, recent: &[PathBuf]) -> Vec<Candidate> {
    let modified = |p: &Path| fs::metadata(p).and_then(|m| m.modified()).ok();
    let mut found = Vec::new();
    if let Some(base) = base
        && let Ok(entries) = fs::read_dir(base)
    {
        for dir in entries.flatten().map(|e| e.path()) {
            let hidden = dir
                .file_name()
                .is_some_and(|n| n.as_encoded_bytes().starts_with(b"."));
            if hidden || !dir.is_dir() {
                continue;
            }
            // Held while we look, so a Kadence starting now can't take it.
            let Ok(Some(lock)) = DirLock::try_lock(&dir) else {
                continue;
            };
            let file = dir.join(SCRATCH_FILE);
            if file.is_file() {
                found.push(Candidate {
                    modified: modified(&file),
                    autosave: file,
                    bundle: None,
                });
            } else if rescue_takes(&dir, base) {
                drop(lock);
                let _ = fs::remove_dir_all(&dir);
            }
        }
    }
    for root in recent {
        let Ok(b) = Bundle::open(root) else {
            continue;
        };
        if b.has_autosave() && !is_locked(b.root()) {
            let file = b.autosave_file();
            found.push(Candidate {
                modified: modified(&file),
                autosave: file,
                bundle: Some(b.root().to_path_buf()),
            });
        }
    }
    found.sort_by_key(|c| std::cmp::Reverse(c.modified));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> Project {
        Project::new("Song")
    }

    #[test]
    fn a_held_lock_blocks_others_until_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let lock = DirLock::try_lock(tmp.path()).unwrap().unwrap();
        assert!(is_locked(tmp.path()));
        assert!(DirLock::try_lock(tmp.path()).unwrap().is_none());
        drop(lock);
        assert!(!is_locked(tmp.path()));
    }

    #[test]
    fn a_running_scratch_folder_is_not_offered() {
        let tmp = tempfile::tempdir().unwrap();
        let scratch = Scratch::create(tmp.path()).unwrap();
        scratch.write(&project()).unwrap();
        assert!(find(Some(tmp.path()), &[]).is_empty());
        // Dropped without removing: as after a crash.
        let file = scratch.file();
        drop(scratch);
        let found = find(Some(tmp.path()), &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].autosave, file);
        assert_eq!(found[0].bundle, None);
        assert_eq!(found[0].name(), "");
        assert_eq!(
            bundle::read_project(&found[0].autosave).unwrap().name,
            "Song"
        );
        found[0].discard().unwrap();
        assert!(find(Some(tmp.path()), &[]).is_empty());
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 0);
    }

    #[test]
    fn adopting_a_crashed_folder_keeps_autosaving_there() {
        let tmp = tempfile::tempdir().unwrap();
        let crashed = Scratch::create(tmp.path()).unwrap();
        crashed.write(&project()).unwrap();
        let dir = crashed.dir().to_path_buf();
        assert!(Scratch::adopt(&dir).unwrap().is_none(), "still held");
        drop(crashed);
        let adopted = Scratch::adopt(&dir).unwrap().unwrap();
        assert_eq!(adopted.file(), dir.join(SCRATCH_FILE));
        assert!(find(Some(tmp.path()), &[]).is_empty(), "held again");
        adopted.remove();
        assert!(!dir.exists());
    }

    #[test]
    fn empty_scratch_folders_are_cleaned_up() {
        let tmp = tempfile::tempdir().unwrap();
        let left = Scratch::create(tmp.path()).unwrap();
        let dir = left.dir().to_path_buf();
        drop(left);
        let running = Scratch::create(tmp.path()).unwrap();
        assert!(find(Some(tmp.path()), &[]).is_empty());
        assert!(!dir.exists(), "orphaned empty folder removed");
        assert!(running.dir().exists(), "a running one is kept");
        running.remove();
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 0);
    }

    #[test]
    fn discarded_scratch_autosave_is_gone() {
        let tmp = tempfile::tempdir().unwrap();
        let scratch = Scratch::create(tmp.path()).unwrap();
        scratch.write(&project()).unwrap();
        scratch.discard().unwrap();
        scratch.discard().unwrap();
        assert!(!scratch.file().exists());
    }

    #[test]
    fn recent_bundles_with_an_autosave_are_offered_unless_open() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("Song");
        let mut doc = daw_model::Document::new(project());
        doc.save_as(&root).unwrap();
        let b = doc.bundle().unwrap().clone();
        let recent = vec![root.clone(), tmp.path().join("Gone")];
        assert!(find(None, &recent).is_empty(), "no autosave yet");
        b.write_autosave(&project()).unwrap();
        let found = find(None, &recent);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].bundle.as_deref(), Some(root.as_path()));
        assert_eq!(found[0].name(), "Song");
        // Open in a running Kadence: not offered.
        let lock = DirLock::try_lock(&root).unwrap().unwrap();
        assert!(find(None, &recent).is_empty());
        drop(lock);
        found[0].discard().unwrap();
        assert!(!b.has_autosave());
        assert!(b.project_file().is_file(), "discarding keeps the project");
    }

    #[test]
    fn newest_first() {
        let tmp = tempfile::tempdir().unwrap();
        let older = Scratch::create(tmp.path()).unwrap();
        older.write(&project()).unwrap();
        let old_file = older.file();
        let t = SystemTime::now() - std::time::Duration::from_secs(60);
        File::options()
            .write(true)
            .open(&old_file)
            .unwrap()
            .set_modified(t)
            .unwrap();
        let newer = Scratch::create(tmp.path()).unwrap();
        newer.write(&project()).unwrap();
        let new_file = newer.file();
        drop(older);
        drop(newer);
        let found = find(Some(tmp.path()), &[]);
        assert_eq!(
            found.iter().map(|c| &c.autosave).collect::<Vec<_>>(),
            vec![&new_file, &old_file]
        );
    }

    #[test]
    fn work_another_kadence_holds_is_in_use() {
        let tmp = tempfile::tempdir().unwrap();
        let scratch = Scratch::create(tmp.path()).unwrap();
        scratch.write(&project()).unwrap();
        let candidate = Candidate {
            autosave: scratch.file(),
            bundle: None,
            modified: None,
        };
        assert_eq!(candidate.in_use().as_deref(), Some(scratch.dir()));
        drop(scratch);
        assert_eq!(candidate.in_use(), None);
    }

    #[test]
    fn takes_without_a_project_are_moved_to_safety_not_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("recovery");
        // A crash during the first take: audio, but no autosave yet.
        let scratch = Scratch::create(&base).unwrap();
        let name = scratch.dir().file_name().unwrap().to_owned();
        let takes = audio_dir(scratch.dir());
        fs::create_dir_all(&takes).unwrap();
        fs::write(takes.join("Guitar.wav"), b"take").unwrap();
        drop(scratch);

        assert!(find(Some(&base), &[]).is_empty());
        assert_eq!(fs::read_dir(&base).unwrap().count(), 0);
        let rescued = tmp.path().join("recordings").join(name).join("Guitar.wav");
        assert_eq!(fs::read(rescued).unwrap(), b"take");
    }

    #[test]
    fn bringing_audio_is_all_or_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let (from, to) = (tmp.path().join("scratch"), tmp.path().join("Song"));
        // Nothing recorded: nothing to bring.
        assert!(bring_audio(&from, &to).unwrap().is_empty());

        fs::create_dir_all(audio_dir(&from)).unwrap();
        for name in ["a.wav", "b.wav"] {
            fs::write(audio_dir(&from).join(name), name).unwrap();
        }
        let made = bring_audio(&from, &to).unwrap();
        assert_eq!(made.len(), 2);
        assert_eq!(fs::read(audio_dir(&to).join("b.wav")).unwrap(), b"b.wav");

        // A name already taken there: refused, and nothing else is added.
        fs::write(audio_dir(&from).join("c.wav"), "c").unwrap();
        assert!(bring_audio(&from, &to).is_err());
        assert!(!audio_dir(&to).join("c.wav").exists());
        assert_eq!(fs::read(audio_dir(&to).join("a.wav")).unwrap(), b"a.wav");
    }
}
