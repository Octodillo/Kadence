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
use std::io::{BufWriter, Write};
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
pub const SCHEMA_VERSION: u64 = 1;
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
pub const MIGRATIONS: &[Migration] = &[];

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
        // Any other project file there would make the bundle ambiguous to open.
        if let Some(existing) = project_files(&bundle.root)?.into_iter().next() {
            return Err(FormatError::Io {
                path: existing,
                source: std::io::ErrorKind::AlreadyExists.into(),
            });
        }
        bundle.make_dirs()?;
        Ok(bundle)
    }

    /// Opens an existing bundle, given either its directory or its project
    /// file. Backups and autosaves are not bundles; read them with
    /// [`read_project`].
    pub fn open(path: impl AsRef<Path>) -> Result<Self, FormatError> {
        let path = path.as_ref();
        let invalid = |msg: &str| FormatError::io(path)(std::io::Error::other(msg.to_owned()));
        if !path.is_file() {
            let mut files = project_files(path)?;
            if files.is_empty() {
                // The project file is gone but an autosave may be left to
                // recover from.
                files = files_with_suffix(path, AUTOSAVE_SUFFIX)?;
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
        let file_name = strip_suffix_ci(name, ".autosave")
            .filter(|n| has_suffix_ci(n, PROJECT_SUFFIX))
            .unwrap_or(name);
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

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn project_file(&self) -> PathBuf {
        self.root.join(&self.file_name)
    }

    pub fn autosave_file(&self) -> PathBuf {
        self.root.join(format!("{}.autosave", self.file_name))
    }

    pub fn audio_dir(&self) -> PathBuf {
        self.root.join(AUDIO_DIR)
    }

    pub fn backups_dir(&self) -> PathBuf {
        self.root.join(BACKUPS_DIR)
    }

    fn make_dirs(&self) -> Result<(), FormatError> {
        for d in [self.root.clone(), self.audio_dir(), self.backups_dir()] {
            fs::create_dir_all(&d).map_err(FormatError::io(d))?;
        }
        Ok(())
    }

    /// Reads the project file.
    pub fn load(&self) -> Result<Project, FormatError> {
        read_project(&self.project_file())
    }

    /// Writes the project file atomically, first copying the previous version
    /// into `backups/`. Removes any autosave, since it is now older.
    pub fn save(&self, project: &Project) -> Result<(), FormatError> {
        self.make_dirs()?;
        let file = self.project_file();
        if file.exists() {
            self.backup(&file)?;
        }
        write_atomic(&file, project)?;
        self.discard_autosave()
    }

    fn backup(&self, file: &Path) -> Result<(), FormatError> {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        // Zero-padded so that name order is time order.
        let dest = self
            .backups_dir()
            .join(format!("{}-{millis:016}.{EXTENSION}", self.stem));
        fs::copy(file, &dest).map_err(FormatError::io(&dest))?;
        let mut backups: Vec<PathBuf> = fs::read_dir(self.backups_dir())
            .map_err(FormatError::io(self.backups_dir()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case(EXTENSION))
            })
            .collect();
        backups.sort();
        let excess = backups.len().saturating_sub(BACKUPS_KEPT);
        for old in &backups[..excess] {
            fs::remove_file(old).map_err(FormatError::io(old))?;
        }
        Ok(())
    }

    /// Lists backups, oldest first.
    pub fn backups(&self) -> Result<Vec<PathBuf>, FormatError> {
        let dir = self.backups_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut v: Vec<PathBuf> = fs::read_dir(&dir)
            .map_err(FormatError::io(&dir))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case(EXTENSION))
            })
            .collect();
        v.sort();
        Ok(v)
    }

    /// Writes the autosave file atomically. The project file is untouched.
    pub fn write_autosave(&self, project: &Project) -> Result<(), FormatError> {
        self.make_dirs()?;
        write_atomic(&self.autosave_file(), project)
    }

    /// Whether an autosave exists that is newer than the project file, which
    /// means the app quit without saving. Offer to recover it.
    /// Whether an autosave exists. Saving deletes it, so this means unsaved
    /// changes from a session that ended without saving.
    pub fn has_autosave(&self) -> bool {
        self.autosave_file().is_file()
    }

    pub fn has_newer_autosave(&self) -> bool {
        let modified = |p: PathBuf| fs::metadata(p).and_then(|m| m.modified()).ok();
        match (
            modified(self.autosave_file()),
            modified(self.project_file()),
        ) {
            (Some(a), Some(p)) => a >= p,
            (Some(_), None) => true,
            _ => false,
        }
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
    /// [`crate::AudioClip::source`].
    pub fn import_audio(&self, src: impl AsRef<Path>) -> Result<PathBuf, FormatError> {
        let src = src.as_ref();
        let dir = self.audio_dir();
        fs::create_dir_all(&dir).map_err(FormatError::io(&dir))?;
        let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("audio");
        let ext = src.extension().and_then(|s| s.to_str());
        let name = |n: usize| {
            let base = if n == 0 {
                stem.to_owned()
            } else {
                format!("{stem}-{n}")
            };
            match ext {
                Some(e) => format!("{base}.{e}"),
                None => base,
            }
        };
        let file = (0..)
            .map(name)
            .find(|n| !dir.join(n).exists())
            .expect("some name is free");
        fs::copy(src, dir.join(&file)).map_err(FormatError::io(src))?;
        // Always `/`, so bundles open the same on every platform.
        Ok(PathBuf::from(format!("{AUDIO_DIR}/{file}")))
    }

    /// Resolves an [`crate::AudioClip::source`] against this bundle.
    pub fn resolve(&self, source: &Path) -> PathBuf {
        self.root.join(source)
    }
}

const PROJECT_SUFFIX: &str = ".kadence";
const AUTOSAVE_SUFFIX: &str = ".kadence.autosave";

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
    files_with_suffix(dir, PROJECT_SUFFIX)
}

fn files_with_suffix(dir: &Path, suffix: &str) -> Result<Vec<PathBuf>, FormatError> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).map_err(FormatError::io(dir))? {
        let p = entry.map_err(FormatError::io(dir))?.path();
        let matches = p
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| has_suffix_ci(n, suffix));
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

/// Writes to a temporary file next to `path`, syncs it, then renames it over
/// `path`, so a crash never leaves a half-written project.
fn write_atomic(path: &Path, project: &Project) -> Result<(), FormatError> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let result = write_synced(&tmp, project)
        .and_then(|()| fs::rename(&tmp, path).map_err(FormatError::io(path)));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
        return result;
    }
    if let Some(dir) = path.parent() {
        // Persist the rename itself. Not every filesystem allows this.
        let _ = File::open(dir).and_then(|d| d.sync_all());
    }
    Ok(())
}

fn write_synced(path: &Path, project: &Project) -> Result<(), FormatError> {
    let file = File::create(path).map_err(FormatError::io(path))?;
    let mut w = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut w, &envelope(project)).map_err(|e| {
        // A full disk is an I/O error, not a malformed file.
        if e.is_io() {
            FormatError::io(path)(e.into())
        } else {
            e.into()
        }
    })?;
    w.write_all(b"\n").map_err(FormatError::io(path))?;
    let file = w
        .into_inner()
        .map_err(|e| FormatError::io(path)(e.into_error()))?;
    file.sync_all().map_err(FormatError::io(path))
}
