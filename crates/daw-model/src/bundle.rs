//! The project bundle on disk and the versioned JSON format.
//!
//! ```text
//! My Song/                      bundle directory
//!   My Song.kadence             project file (JSON, see docs/project-format.md)
//!   My Song.kadence.autosave    newer unsaved state, if any
//!   audio/                      recorded and imported audio
//!   backups/                    previous saves, newest 10 kept
//! ```

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;

use crate::error::FormatError;
use crate::model::Project;

/// The `format` field every project file carries.
pub const FORMAT_NAME: &str = "kadence-project";
/// The schema version this build writes. Older files are migrated on load.
/// Bump it for every added field too, or older builds drop it when they save.
pub const SCHEMA_VERSION: u64 = 4;
/// File extension of the project file (matches the MIME glob `*.kadence`).
pub const EXTENSION: &str = "kadence";
/// How many previous saves `backups/` keeps.
pub const BACKUPS_KEPT: usize = 10;

pub const AUDIO_DIR: &str = "audio";
pub const BACKUPS_DIR: &str = "backups";

/// Upgrades a project file's JSON by one schema version.
pub type Migration = fn(Value) -> Result<Value, FormatError>;

/// `MIGRATIONS[i]` turns schema `i + 1` into `i + 2`. Append one each time
/// `SCHEMA_VERSION` goes up.
pub const MIGRATIONS: &[Migration] = &[v1_unname_defaults, v2_clip_fades, v3_add_track_inputs];

/// Version 1 saved the English defaults "Untitled" and "Master" as names.
/// Version 2 leaves them empty and the UI words them in the user's language.
fn v1_unname_defaults(mut v: Value) -> Result<Value, FormatError> {
    fn clear(object: Option<&mut Value>, default: &str) {
        if let Some(name) = object.and_then(|o| o.get_mut("name"))
            && name == default
        {
            *name = Value::from("");
        }
    }
    clear(v.get_mut("master"), "Master");
    clear(Some(&mut v), "Untitled");
    Ok(v)
}

/// Version 3 adds `fade_in` and `fade_out` to audio clips. Both default to
/// no fade, so version 2 files load unchanged.
fn v2_clip_fades(v: Value) -> Result<Value, FormatError> {
    Ok(v)
}

/// Version 4 adds `midi_input` to tracks, which defaults to every input.
fn v3_add_track_inputs(v: Value) -> Result<Value, FormatError> {
    Ok(v)
}

#[derive(Serialize)]
struct EnvelopeOut<'a> {
    format: &'a str,
    version: u64,
    generator: String,
    project: &'a Project,
}

#[derive(Deserialize)]
struct EnvelopeIn<'a> {
    format: String,
    version: u64,
    #[serde(borrow)]
    project: &'a RawValue,
}

/// Serializes a project to the current schema.
pub fn to_json(project: &Project) -> Result<String, FormatError> {
    Ok(serde_json::to_string_pretty(&envelope(project))?)
}

/// A project file's content: [`to_json`] and the newline that ends it.
fn file_content(project: &Project) -> Result<Vec<u8>, FormatError> {
    let mut bytes = serde_json::to_vec_pretty(&envelope(project))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn envelope(project: &Project) -> EnvelopeOut<'_> {
    EnvelopeOut {
        format: FORMAT_NAME,
        version: SCHEMA_VERSION,
        generator: format!("daw-model {}", env!("CARGO_PKG_VERSION")),
        project,
    }
}

/// Parses a project file of any supported schema version, migrating it to the
/// current one, and checks that it is consistent.
pub fn from_json(json: &str) -> Result<Project, FormatError> {
    from_json_with(json, SCHEMA_VERSION, MIGRATIONS)
}

/// [`from_json`] with an explicit migration table, for testing migrations.
pub fn from_json_with(
    json: &str,
    current: u64,
    migrations: &[Migration],
) -> Result<Project, FormatError> {
    let env: EnvelopeIn<'_> = serde_json::from_str(json)?;
    if env.format != FORMAT_NAME {
        return Err(FormatError::WrongFormat(env.format));
    }
    if env.version > current {
        return Err(FormatError::TooNew {
            found: env.version,
            supported: current,
        });
    }
    let project: Project = if env.version == current {
        // The common case skips the generic `Value` tree entirely.
        serde_json::from_str(env.project.get())?
    } else {
        let mut value: Value = serde_json::from_str(env.project.get())?;
        for v in env.version..current {
            let m = v
                .checked_sub(1)
                .and_then(|i| migrations.get(i as usize))
                .ok_or(FormatError::NoMigration(v))?;
            value = m(value)?;
        }
        serde_json::from_value(value)?
    };
    project.validate()?;
    Ok(project)
}

/// A project bundle directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bundle {
    root: PathBuf,
    /// The project file's name without extension, for naming backups.
    stem: String,
    /// The project file's name as it is on disk (the extension's case may
    /// differ from `.kadence`).
    file_name: String,
}

impl Bundle {
    /// Creates a new bundle directory `dir` holding `<dir name>.kadence`.
    /// The directory may exist but must not already hold any project file.
    pub fn create(dir: impl AsRef<Path>) -> Result<Self, FormatError> {
        let root = dir.as_ref().to_path_buf();
        let stem = root
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| strip_suffix_ci(n, PROJECT_SUFFIX).unwrap_or(n).to_owned())
            .filter(|n| !n.is_empty())
            .ok_or_else(|| FormatError::Io {
                path: root.clone(),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "bundle needs a directory name",
                ),
            })?;
        let file_name = format!("{stem}.{EXTENSION}");
        let bundle = Self {
            root,
            stem,
            file_name,
        };
        // Any other project file there would make the bundle ambiguous to
        // open, and saving would delete an autosave left to recover from.
        let existing = project_files(&bundle.root)?
            .into_iter()
            .chain(autosaves(&bundle.root)?)
            .next();
        if let Some(existing) = existing {
            return Err(FormatError::Io {
                path: existing,
                source: std::io::ErrorKind::AlreadyExists.into(),
            });
        }
        for d in [&bundle.root, &bundle.audio_dir(), &bundle.backups_dir()] {
            fs::create_dir_all(d).map_err(FormatError::io(d))?;
        }
        Ok(bundle)
    }

    /// What saving as `path` means. A file dialog hands back the project
    /// file `Name.kadence`, which stands for the bundle directory `Name`
    /// around it; if that file is already there, it is the bundle it
    /// belongs to that was chosen.
    pub fn target(path: impl AsRef<Path>) -> Result<Target, FormatError> {
        let path = path.as_ref();
        if path.is_file() {
            return Self::open(path).map(Target::Existing);
        }
        let around = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| strip_suffix_ci(n, PROJECT_SUFFIX))
            .filter(|stem| !matches!(*stem, "" | "." | ".."))
            // A folder called `Name.kadence` is a bundle directory itself.
            .filter(|_| path.symlink_metadata().is_err())
            .map(|stem| path.with_file_name(stem));
        Ok(Target::New(around.unwrap_or_else(|| path.to_path_buf())))
    }

    /// Opens an existing bundle, given either its directory or its project
    /// file. Backups and autosaves are not bundles; read them with
    /// [`read_project`]. A `Name.kadence` path that doesn't exist opens the
    /// bundle directory `Name` beside it, which is where saving as
    /// `Name.kadence` puts the project.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, FormatError> {
        let path = path.as_ref();
        let invalid = |msg: &str| FormatError::io(path)(std::io::Error::other(msg.to_owned()));
        if !path.exists()
            && let Some(dir) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| strip_suffix_ci(n, PROJECT_SUFFIX))
                .filter(|stem| !matches!(*stem, "" | "." | ".."))
                .map(|stem| path.with_file_name(stem))
                .filter(|dir| dir.is_dir())
        {
            // A folder that holds no project is reported as the path asked for.
            return Self::open(dir).map_err(|e| match e {
                FormatError::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound => {
                    FormatError::io(path)(source)
                }
                e => e,
            });
        }
        if !path.is_file() {
            let mut files = project_files(path)?;
            if files.is_empty() {
                // The project file is gone but an autosave may be left to
                // recover from.
                files = autosaves(path)?;
            }
            return match files.len() {
                0 => Err(FormatError::io(path)(std::io::ErrorKind::NotFound.into())),
                1 => Self::open(files.remove(0)),
                _ => Err(invalid("bundle holds more than one project file")),
            };
        }
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| invalid("project file name is not valid UTF-8"))?;
        // An autosave path stands for the project file next to it.
        let file_name = autosave_of(name).unwrap_or(name);
        let stem = strip_suffix_ci(file_name, PROJECT_SUFFIX)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| invalid("not a .kadence project file"))?;
        let root = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        if is_backup(&root, stem)? {
            return Err(invalid(
                "this is a backup; open it with read_project and save it into a bundle",
            ));
        }
        Ok(Self {
            root,
            stem: stem.to_owned(),
            file_name: file_name.to_owned(),
        })
    }

    /// The project file's name without its extension.
    pub fn name(&self) -> &str {
        &self.stem
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn project_file(&self) -> PathBuf {
        self.root.join(&self.file_name)
    }

    pub fn autosave_file(&self) -> PathBuf {
        self.root.join(format!("{}{AUTOSAVE_EXT}", self.file_name))
    }

    pub fn audio_dir(&self) -> PathBuf {
        self.root.join(AUDIO_DIR)
    }

    pub fn backups_dir(&self) -> PathBuf {
        self.root.join(BACKUPS_DIR)
    }

    /// Makes sure `dir`, a folder of this bundle, is there. The bundle's
    /// own folder is only ever made by [`Bundle::create`]: if it was moved,
    /// renamed or deleted since, writing into a new one at the old place
    /// would split the project from its audio, so that is an error naming
    /// the folder.
    fn ensure_dir(&self, dir: &Path) -> Result<(), FormatError> {
        if !self.root.is_dir() {
            return Err(FormatError::io(&self.root)(
                std::io::ErrorKind::NotFound.into(),
            ));
        }
        fs::create_dir_all(dir).map_err(FormatError::io(dir))
    }

    /// Reads the project file.
    pub fn load(&self) -> Result<Project, FormatError> {
        read_project(&self.project_file())
    }

    /// Writes the project file atomically, keeping the version it replaces
    /// in `backups/`. Saving what the file already holds changes nothing, so
    /// the backups stay distinct earlier versions. Removes any autosave,
    /// since it is now older.
    ///
    /// `link` says what happens if the project file is a symlink. Pass
    /// [`Link::Follow`] only when `project` was loaded through it with
    /// [`Bundle::load`].
    pub fn save(&self, project: &Project, link: Link) -> Result<(), FormatError> {
        self.ensure_dir(&self.root)?;
        let file = self.project_file();
        let content = file_content(project)?;
        let previous = match link {
            Link::Follow => fs::metadata(&file),
            // What a link points to is not a previous version of this project.
            Link::Replace => fs::symlink_metadata(&file),
        };
        if !previous.is_ok_and(|m| m.is_file()) {
            write_atomic(&file, &file, link, &content)?;
        } else if fs::read(&file).is_ok_and(|saved| saved == content) {
            // Already saved.
        } else {
            let backups = self.backup(&file)?;
            if let Err(e) = write_atomic(&file, &file, link, &content) {
                // Nothing was replaced, so there is no earlier version to keep.
                if let Some((_, unused)) = backups.last() {
                    let _ = fs::remove_file(unused);
                }
                return Err(e);
            }
            // Only this project's backups rotate; ones left by a project
            // file of another name (before a rename) are kept. One that
            // cannot be removed is left: the project is saved.
            let excess = backups.len().saturating_sub(BACKUPS_KEPT);
            for (_, old) in &backups[..excess] {
                let _ = fs::remove_file(old);
            }
        }
        // The project is saved even if the autosave cannot be removed, which
        // only means an offer to recover it later.
        let _ = self.discard_autosave();
        Ok(())
    }

    /// Copies `file` into `backups/` under the next name. Returns this
    /// project's backups, oldest first, the new one last.
    fn backup(&self, file: &Path) -> Result<Vec<(u64, PathBuf)>, FormatError> {
        self.ensure_dir(&self.backups_dir())?;
        let mut backups = self.own_backups()?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        // Always after the newest backup, even when two saves share a
        // millisecond or the clock was set back.
        let mut stamp = backups
            .last()
            .map_or(now, |&(newest, _)| now.max(newest + 1))
            .min(MAX_STAMP);
        let dest = loop {
            let dest = self
                .backups_dir()
                .join(format!("{}-{stamp:016}.{EXTENSION}", self.stem));
            match copy_new(file, &dest) {
                // Taken by something the listing leaves out, such as a
                // dangling link: never written through, the next name is used.
                Err(FormatError::Io { source, .. })
                    if source.kind() == std::io::ErrorKind::AlreadyExists =>
                {
                    // No later name exists. Saving without a backup is not
                    // an option, so the save fails and names the file.
                    if stamp == MAX_STAMP {
                        return Err(FormatError::Io { path: dest, source });
                    }
                    stamp += 1;
                }
                result => break result.map(|()| dest)?,
            }
        };
        backups.push((stamp, dest));
        Ok(backups)
    }

    /// This project's `<stem>-<timestamp>` backups, oldest first.
    fn own_backups(&self) -> Result<Vec<(u64, PathBuf)>, FormatError> {
        let mut v: Vec<(u64, PathBuf)> = project_files(&self.backups_dir())?
            .into_iter()
            .filter_map(|p| Some((self.backup_stamp(&p)?, p)))
            .collect();
        v.sort();
        Ok(v)
    }

    /// The timestamp in a backup's name, if it is one of this project's.
    fn backup_stamp(&self, path: &Path) -> Option<u64> {
        let name = path.file_name()?.to_str()?;
        let ts = strip_suffix_ci(name, PROJECT_SUFFIX)?
            .strip_prefix(self.stem.as_str())?
            .strip_prefix('-')?;
        (ts.len() == 16 && ts.bytes().all(|b| b.is_ascii_digit()))
            .then(|| ts.parse().ok())
            .flatten()
    }

    /// Lists backups, oldest first. Backups left by a project file of another
    /// name (before a rename) come before this project's own.
    pub fn backups(&self) -> Result<Vec<PathBuf>, FormatError> {
        let mut others: Vec<PathBuf> = project_files(&self.backups_dir())?
            .into_iter()
            .filter(|p| self.backup_stamp(p).is_none())
            .collect();
        others.sort();
        others.extend(self.own_backups()?.into_iter().map(|(_, p)| p));
        Ok(others)
    }

    /// Writes the autosave file atomically. The project file is untouched.
    pub fn write_autosave(&self, project: &Project) -> Result<(), FormatError> {
        self.ensure_dir(&self.root)?;
        // The autosave holds the same data, so it gets the same permissions.
        write_atomic(
            &self.autosave_file(),
            &self.project_file(),
            Link::Replace,
            &file_content(project)?,
        )
    }

    /// Whether an autosave exists. Saving deletes it, so this means unsaved
    /// changes from a session that ended without saving. Offer to recover it.
    pub fn has_autosave(&self) -> bool {
        self.autosave_file().is_file()
    }

    pub fn load_autosave(&self) -> Result<Project, FormatError> {
        read_project(&self.autosave_file())
    }

    /// Deletes the autosave, if there is one.
    pub fn discard_autosave(&self) -> Result<(), FormatError> {
        match fs::remove_file(self.autosave_file()) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(FormatError::io(self.autosave_file())(e))
            }
            _ => Ok(()),
        }
    }

    /// Copies an audio file into `audio/` under a name that doesn't clash, and
    /// returns its path relative to the bundle root, for
    /// [`crate::AudioClip::source`]. A failed copy leaves nothing behind.
    pub fn import_audio(&self, src: impl AsRef<Path>) -> Result<PathBuf, FormatError> {
        let src = src.as_ref();
        let dir = self.audio_dir();
        self.ensure_dir(&dir)?;
        let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("audio");
        let ext = src.extension().and_then(|s| s.to_str());
        for n in 0usize.. {
            let base = if n == 0 {
                stem.to_owned()
            } else {
                format!("{stem}-{n}")
            };
            let file = match ext {
                Some(e) => format!("{base}.{e}"),
                None => base,
            };
            match copy_new(src, &dir.join(&file)) {
                Err(FormatError::Io { source, .. })
                    if source.kind() == std::io::ErrorKind::AlreadyExists => {}
                // Always `/`, so bundles open the same on every platform.
                result => return result.map(|()| PathBuf::from(format!("{AUDIO_DIR}/{file}"))),
            }
        }
        unreachable!("some name is free")
    }

    /// Resolves an [`crate::AudioClip::source`] against this bundle.
    pub fn resolve(&self, source: &Path) -> PathBuf {
        self.root.join(source)
    }
}

/// What [`Bundle::target`] makes of the path chosen to save as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A bundle to create in this directory.
    New(PathBuf),
    /// The bundle the chosen project file belongs to.
    Existing(Bundle),
}

const PROJECT_SUFFIX: &str = ".kadence";
/// What an autosave's name adds to its project file's.
const AUTOSAVE_EXT: &str = ".autosave";

/// The project file name that the autosave called `name` belongs to.
fn autosave_of(name: &str) -> Option<&str> {
    strip_suffix_ci(name, AUTOSAVE_EXT).filter(|n| has_suffix_ci(n, PROJECT_SUFFIX))
}

fn has_suffix_ci(name: &str, suffix: &str) -> bool {
    strip_suffix_ci(name, suffix).is_some()
}

/// `name` without `suffix`, ignoring ASCII case.
fn strip_suffix_ci<'a>(name: &'a str, suffix: &str) -> Option<&'a str> {
    let split = name.len().checked_sub(suffix.len())?;
    let (head, tail) = (name.get(..split)?, name.get(split..)?);
    tail.eq_ignore_ascii_case(suffix).then_some(head)
}

/// The project files directly inside `dir`.
fn project_files(dir: &Path) -> Result<Vec<PathBuf>, FormatError> {
    files_named(dir, |n| has_suffix_ci(n, PROJECT_SUFFIX))
}

/// The autosaves directly inside `dir`.
fn autosaves(dir: &Path) -> Result<Vec<PathBuf>, FormatError> {
    files_named(dir, |n| autosave_of(n).is_some())
}

/// The files directly inside `dir` whose name `wanted` accepts.
fn files_named(dir: &Path, wanted: impl Fn(&str) -> bool) -> Result<Vec<PathBuf>, FormatError> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).map_err(FormatError::io(dir))? {
        let p = entry.map_err(FormatError::io(dir))?.path();
        let matches = p.file_name().and_then(|n| n.to_str()).is_some_and(&wanted);
        if matches && p.is_file() {
            files.push(p);
        }
    }
    Ok(files)
}

/// Whether a project file in `dir` named `stem` is a backup: it sits in a
/// `backups/` folder and either has the `<name>-<16-digit timestamp>` name
/// [`Bundle::save`] gives backups or the folder belongs to a bundle.
fn is_backup(dir: &Path, stem: &str) -> Result<bool, FormatError> {
    if dir.file_name().is_none_or(|n| n != BACKUPS_DIR) {
        return Ok(false);
    }
    let timestamped = stem
        .rsplit_once('-')
        .is_some_and(|(_, ts)| ts.len() == 16 && ts.bytes().all(|b| b.is_ascii_digit()));
    if timestamped {
        return Ok(true);
    }
    match dir.parent() {
        Some(parent) => Ok(!project_files(parent)?.is_empty()),
        None => Ok(false),
    }
}

/// Reads any project file on its own, such as a backup or an autosave.
pub fn read_project(path: &Path) -> Result<Project, FormatError> {
    let json = fs::read_to_string(path).map_err(FormatError::io(path))?;
    from_json(&json)
}

/// Writes a project file on its own, atomically, such as the autosave of a
/// project that has no bundle yet. Read it back with [`read_project`].
pub fn write_project(path: &Path, project: &Project) -> Result<(), FormatError> {
    write_atomic(path, path, Link::Replace, &file_content(project)?)
}

/// The largest time stamp a backup's 16-digit name holds.
const MAX_STAMP: u64 = 9_999_999_999_999_999;

/// Copies the file `src` to `dest`, which must not exist yet: a link put at
/// `dest` beforehand is refused, not written through. A failed copy leaves
/// nothing at `dest`.
pub(crate) fn copy_new(src: &Path, dest: &Path) -> Result<(), FormatError> {
    let mut from = File::open(src).map_err(FormatError::io(src))?;
    let meta = from.metadata().map_err(FormatError::io(src))?;
    let mut to = File::create_new(dest).map_err(FormatError::io(dest))?;
    let copied = (|| {
        #[cfg(unix)]
        to.set_permissions(meta.permissions())
            .map_err(FormatError::io(dest))?;
        std::io::copy(&mut from, &mut to).map_err(FormatError::io(src))?;
        Ok(())
    })();
    if copied.is_err() {
        let _ = fs::remove_file(dest);
    }
    copied
}

/// What a write does with a symlink at the path it writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    /// Write the file the link points to and keep the link. Only for a file
    /// whose contents were read through the same link, which makes it the
    /// project file of a user who keeps it elsewhere and links it into the
    /// bundle.
    Follow,
    /// Replace the link with the new file. For files only Kadence creates,
    /// where a link can only have come with a bundle from somewhere else and
    /// would direct the write to a file of its maker's choosing.
    Replace,
}

/// Replaces the file at `path` with `content` through
/// [`daw_core::atomic_file`], so a crash never leaves a half-written project
/// and nothing put at a temporary name beforehand receives the write. The
/// new file keeps the permissions of `path`, or of `like` if `path` doesn't
/// exist yet.
fn write_atomic(path: &Path, like: &Path, link: Link, content: &[u8]) -> Result<(), FormatError> {
    let path = match fs::symlink_metadata(path) {
        // A dangling link is replaced either way.
        Ok(m) if link == Link::Follow && m.file_type().is_symlink() => {
            fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
        }
        _ => path.to_path_buf(),
    };
    let perms = fs::symlink_metadata(&path)
        .ok()
        .filter(|m| m.is_file())
        .or_else(|| fs::metadata(like).ok())
        .map(|m| m.permissions());
    daw_core::atomic_file::write(&path, perms, content).map_err(FormatError::io(path))
}
