//! Files written whole: under a temporary name next to where they go, then
//! renamed into place, so a reader or a crash never meets half a file.
//!
//! Every crate that saves something (projects, exports, peak and scan
//! caches) writes through here, so they share one set of guarantees:
//!
//! - The temporary file is always created new. Something put at its name
//!   beforehand, such as a link in a folder from someone else, is never
//!   opened; the next name is tried.
//! - The data is synced before the rename, and the folder after it.
//! - A write that fails or is abandoned removes its temporary file, and
//!   leaves the file it would have replaced as it was.
//! - Temporary files a killed process left behind are removed by the next
//!   write into the same folder, once they are an hour old.
//!
//! Not for the audio thread.

use std::fs::{self, File, Permissions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Temporary files are `.kadence-<process id>-<serial>.tmp`: short and of
/// bounded length, so a long file name next to it can't push it past the
/// longest name the file system takes.
const PREFIX: &str = ".kadence-";
const SUFFIX: &str = ".tmp";

/// How old a temporary file must be before another process removes it. A
/// younger one may still be written.
const STALE_AFTER: Duration = Duration::from_secs(3600);

/// A file being written next to its target under a temporary name. Dropped
/// without [`commit`](Self::commit), the temporary file is removed.
#[derive(Debug)]
pub struct Replacement {
    target: PathBuf,
    tmp: PathBuf,
    done: bool,
}

impl Replacement {
    /// Starts a file that will replace `target`, and returns it with the
    /// handle to write it through. The folder must exist. A link at `target`
    /// is replaced, not followed: resolve it first to write what it points
    /// to.
    pub fn begin(target: &Path) -> io::Result<(Self, File)> {
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        let dir = folder(target);
        remove_stale(dir, STALE_AFTER);
        loop {
            let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
            let tmp = dir.join(format!("{PREFIX}{}-{serial}{SUFFIX}", std::process::id()));
            match File::create_new(&tmp) {
                Ok(file) => {
                    let this = Self {
                        target: target.to_owned(),
                        tmp,
                        done: false,
                    };
                    return Ok((this, file));
                }
                // Left by an earlier process with this id, or planted.
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }
    }

    /// The temporary file's path, for error messages.
    pub fn tmp(&self) -> &Path {
        &self.tmp
    }

    /// The path the file ends up at.
    pub fn target(&self) -> &Path {
        &self.target
    }

    /// Gives the temporary file `permissions`. Call it before writing data
    /// that fewer people may read than a new file allows. Where permissions
    /// are only a read-only flag, which would stop the write, it does
    /// nothing.
    pub fn restrict(&self, file: &File, permissions: Permissions) -> io::Result<()> {
        #[cfg(unix)]
        return file.set_permissions(permissions);
        #[cfg(not(unix))]
        {
            let _ = (file, permissions);
            Ok(())
        }
    }

    /// Syncs `file` (the one [`begin`](Self::begin) returned, fully written
    /// and flushed) and renames it into place.
    pub fn commit(mut self, file: File) -> io::Result<()> {
        file.sync_all()?;
        drop(file);
        fs::rename(&self.tmp, &self.target)?;
        self.done = true;
        // Persist the rename itself. Not every file system allows this.
        let _ = File::open(folder(&self.target)).and_then(|d| d.sync_all());
        Ok(())
    }
}

impl Drop for Replacement {
    fn drop(&mut self) {
        if !self.done {
            let _ = fs::remove_file(&self.tmp);
        }
    }
}

/// Writes `content` as the file at `target`, replacing what is there. With
/// `permissions`, the file has them from before any content goes in.
pub fn write(target: &Path, permissions: Option<Permissions>, content: &[u8]) -> io::Result<()> {
    let (replacement, mut file) = Replacement::begin(target)?;
    if let Some(p) = permissions {
        replacement.restrict(&file, p)?;
    }
    file.write_all(content)?;
    replacement.commit(file)
}

/// The folder `path` is in.
fn folder(path: &Path) -> &Path {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    }
}

/// The id of the process that made the temporary file called `name`, if it
/// is a name this module gives one.
fn temp_owner(name: &str) -> Option<u32> {
    let (pid, serial) = name
        .strip_prefix(PREFIX)?
        .strip_suffix(SUFFIX)?
        .split_once('-')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    (digits(pid) && digits(serial))
        .then(|| pid.parse().ok())
        .flatten()
}

/// Removes the temporary files in `dir` that other processes last wrote at
/// least `age` ago. This process's own are left: one may be a write that
/// has stood still that long (the machine slept), and each is removed when
/// its [`Replacement`] is dropped anyway.
fn remove_stale(dir: &Path, age: Duration) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        // The name is checked before any stat, so a big sample folder costs
        // one comparison per entry.
        let owner = entry.file_name().to_str().and_then(temp_owner);
        if owner.is_none_or(|pid| pid == std::process::id()) {
            continue;
        }
        let stale = entry
            .metadata()
            .ok()
            .filter(|m| m.is_file())
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|since| since >= age);
        if stale {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temps(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| temp_owner(n).is_some())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_committed_file_replaces_the_target_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a.json");
        write(&target, None, b"one").unwrap();
        write(&target, None, b"two").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"two");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn an_abandoned_file_leaves_the_target_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a.json");
        fs::write(&target, b"kept").unwrap();
        let (replacement, mut file) = Replacement::begin(&target).unwrap();
        file.write_all(b"half").unwrap();
        assert_eq!(temps(dir.path()).len(), 1);
        drop(replacement);
        assert_eq!(fs::read(&target).unwrap(), b"kept");
        assert!(temps(dir.path()).is_empty());
    }

    #[test]
    fn a_taken_temp_name_is_passed_over_not_opened() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a.json");
        // The next names this process would use, taken by someone else.
        let (probe, _) = Replacement::begin(&target).unwrap();
        let name = probe.tmp().file_name().unwrap().to_str().unwrap();
        let serial: u64 = name
            .trim_end_matches(SUFFIX)
            .rsplit_once('-')
            .unwrap()
            .1
            .parse()
            .unwrap();
        drop(probe);
        let victim = dir.path().join("victim");
        fs::write(&victim, b"theirs").unwrap();
        // Tests run in parallel and share the serial, so take a run of names.
        for n in serial + 1..serial + 200 {
            let planted = dir
                .path()
                .join(format!("{PREFIX}{}-{n}{SUFFIX}", std::process::id()));
            #[cfg(unix)]
            std::os::unix::fs::symlink(&victim, planted).unwrap();
            #[cfg(not(unix))]
            fs::write(planted, b"taken").unwrap();
        }
        write(&target, None, b"mine").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"mine");
        assert_eq!(fs::read(&victim).unwrap(), b"theirs");
    }

    #[test]
    fn only_old_temporary_files_are_swept() {
        let dir = tempfile::tempdir().unwrap();
        // Another process's: this one's id plus one is not this one's.
        let stale = dir
            .path()
            .join(format!(".kadence-{}-0.tmp", std::process::id() + 1));
        let own = format!(".kadence-{}-999999999.tmp", std::process::id());
        let other = [
            own.as_str(),
            ".kadence-4242-0.tmp.bak",
            "kadence-1-1.tmp",
            ".kadence--1.tmp",
            ".kadence-1-.tmp",
            ".kadence-x-1.tmp",
            "song.wav",
        ];
        fs::write(&stale, b"").unwrap();
        for name in other {
            fs::write(dir.path().join(name), b"").unwrap();
        }
        // Not old enough yet: it may be another process's write in progress.
        remove_stale(dir.path(), STALE_AFTER);
        assert!(stale.exists());
        remove_stale(dir.path(), Duration::ZERO);
        assert!(!stale.exists());
        for name in other {
            assert!(dir.path().join(name).exists(), "{name}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_file_has_its_permissions_before_its_content() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("private");
        write(&target, Some(Permissions::from_mode(0o600)), b"x").unwrap();
        let mode = fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
