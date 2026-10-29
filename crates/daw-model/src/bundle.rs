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
    /// The project file's name without extension.
    stem: String,
}

impl Bundle {
    /// Creates a new bundle directory `dir` holding `<dir name>.kadence`.
    /// The directory may exist but must not already hold a project file.
    pub fn create(dir: impl AsRef<Path>) -> Result<Self, FormatError> {
        let root = dir.as_ref().to_path_buf();
        let stem = root
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.strip_suffix(".kadence").unwrap_or(n).to_owned())
            .filter(|n| !n.is_empty())
            .ok_or_else(|| FormatError::Io {
                path: root.clone(),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "bundle needs a directory name",
                ),
            })?;
        let bundle = Self { root, stem };
        if bundle.project_file().exists() {
            return Err(FormatError::Io {
                path: bundle.project_file(),
                source: std::io::ErrorKind::AlreadyExists.into(),
            });
        }
        bundle.make_dirs()?;
        Ok(bundle)
    }

    /// Opens an existing bundle, given either its directory or its project
    /// file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, FormatError> {
        let path = path.as_ref();
        if path.is_file() {
            let root = path.parent().unwrap_or(Path::new(".")).to_path_buf();
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| FormatError::io(path)(std::io::ErrorKind::InvalidInput.into()))?;
            return Ok(Self {
                root,
                stem: stem.to_owned(),
            });
        }
        let mut found = None;
        for entry in fs::read_dir(path).map_err(FormatError::io(path))? {
            let p = entry.map_err(FormatError::io(path))?.path();
            if p.extension().is_some_and(|e| e == EXTENSION) && p.is_file() {
                if found.is_some() {
                    return Err(FormatError::io(path)(std::io::Error::other(
                        "bundle holds more than one project file",
                    )));
                }
                found = Some(p);
            }
        }
        let file =
            found.ok_or_else(|| FormatError::io(path)(std::io::ErrorKind::NotFound.into()))?;
        Self::open(file)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn project_file(&self) -> PathBuf {
        self.root.join(format!("{}.{EXTENSION}", self.stem))
    }

    pub fn autosave_file(&self) -> PathBuf {
        self.root
            .join(format!("{}.{EXTENSION}.autosave", self.stem))
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
        match fs::remove_file(self.autosave_file()) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(FormatError::io(self.autosave_file())(e))
            }
            _ => Ok(()),
        }
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
            .filter(|p| p.extension().is_some_and(|e| e == EXTENSION))
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
            .filter(|p| p.extension().is_some_and(|e| e == EXTENSION))
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

    pub fn discard_autosave(&self) -> Result<(), FormatError> {
        fs::remove_file(self.autosave_file()).map_err(FormatError::io(self.autosave_file()))
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
        Ok(Path::new(AUDIO_DIR).join(file))
    }

    /// Resolves an [`crate::AudioClip::source`] against this bundle.
    pub fn resolve(&self, source: &Path) -> PathBuf {
        self.root.join(source)
    }
}

fn read_project(path: &Path) -> Result<Project, FormatError> {
    let json = fs::read_to_string(path).map_err(FormatError::io(path))?;
    from_json(&json)
}

/// Writes to a temporary file next to `path`, syncs it, then renames it over
/// `path`, so a crash never leaves a half-written project.
fn write_atomic(path: &Path, project: &Project) -> Result<(), FormatError> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let file = File::create(&tmp).map_err(FormatError::io(&tmp))?;
    let mut w = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut w, &envelope(project))?;
    w.write_all(b"\n").map_err(FormatError::io(&tmp))?;
    let file = w
        .into_inner()
        .map_err(|e| FormatError::io(&tmp)(e.into_error()))?;
    file.sync_all().map_err(FormatError::io(&tmp))?;
    fs::rename(&tmp, path).map_err(FormatError::io(path))?;
    if let Some(dir) = path.parent() {
        // Persist the rename itself. Not every filesystem allows this.
        let _ = File::open(dir).and_then(|d| d.sync_all());
    }
    Ok(())
}
