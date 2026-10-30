//! An open project: the model, its history and where it is saved.

use std::path::{Path, PathBuf};

use crate::bundle::Bundle;
use crate::change::ChangeSet;
use crate::command::Command;
use crate::error::{FormatError, ModelError};
use crate::model::Project;
use crate::snapshot::ProjectSnapshot;
use crate::undo::UndoStack;

/// What the app holds for each open project. All edits go through
/// [`Document::execute`] so they can be undone.
#[derive(Debug)]
pub struct Document {
    project: Project,
    history: UndoStack,
    bundle: Option<Bundle>,
    /// Bumped on every change, including undo and redo.
    revision: u64,
    /// History position at the last save, or `None` if that state is gone.
    saved_at: Option<u64>,
    autosaved_revision: u64,
    snapshot: Option<ProjectSnapshot>,
}

impl Default for Document {
    fn default() -> Self {
        Self::new(Project::default())
    }
}

impl Document {
    /// A new, unsaved document.
    pub fn new(project: Project) -> Self {
        Self {
            project,
            history: UndoStack::default(),
            bundle: None,
            revision: 0,
            saved_at: Some(0),
            autosaved_revision: 0,
            snapshot: None,
        }
    }

    /// Opens a bundle (its directory or its project file).
    ///
    /// Fails with [`FormatError::AutosavePending`] if the bundle holds an
    /// autosave (saving always deletes it, so one means the app quit without
    /// saving).
    /// Ask the user, then call [`Document::recover`] or
    /// [`Document::open_discarding_autosave`]. This way the open document
    /// always owns the bundle's autosave file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, FormatError> {
        let bundle = Bundle::open(path)?;
        if bundle.has_autosave() {
            return Err(FormatError::AutosavePending(bundle.autosave_file()));
        }
        Self::load(bundle)
    }

    /// Opens a bundle's project file, deleting any autosave first.
    pub fn open_discarding_autosave(path: impl AsRef<Path>) -> Result<Self, FormatError> {
        let bundle = Bundle::open(path)?;
        // Load first: if the project file is missing or broken, the autosave
        // may be the only copy of the work.
        let doc = Self::load(bundle)?;
        doc.bundle
            .as_ref()
            .expect("just loaded")
            .discard_autosave()?;
        Ok(doc)
    }

    fn load(bundle: Bundle) -> Result<Self, FormatError> {
        let project = bundle.load()?;
        Ok(Self {
            bundle: Some(bundle),
            ..Self::new(project)
        })
    }

    /// Opens a bundle from its autosave instead of its project file. The
    /// document starts out modified, since the recovered state isn't saved.
    pub fn recover(path: impl AsRef<Path>) -> Result<Self, FormatError> {
        let bundle = Bundle::open(path)?;
        let project = bundle.load_autosave()?;
        Ok(Self {
            bundle: Some(bundle),
            saved_at: None,
            ..Self::new(project)
        })
    }

    pub fn project(&self) -> &Project {
        &self.project
    }

    pub fn history(&self) -> &UndoStack {
        &self.history
    }

    pub fn bundle(&self) -> Option<&Bundle> {
        self.bundle.as_ref()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Applies an edit and records it for undo.
    pub fn execute(&mut self, command: impl Command) -> Result<ChangeSet, ModelError> {
        self.execute_boxed(Box::new(command))
    }

    pub fn execute_boxed(&mut self, command: Box<dyn Command>) -> Result<ChangeSet, ModelError> {
        let result = self.history.execute(&mut self.project, command);
        self.after(result)
    }

    pub fn undo(&mut self) -> Result<Option<ChangeSet>, ModelError> {
        let result = self.history.undo(&mut self.project);
        self.after_step(result)
    }

    pub fn redo(&mut self) -> Result<Option<ChangeSet>, ModelError> {
        let result = self.history.redo(&mut self.project);
        self.after_step(result)
    }

    /// Ends the current gesture so the next edit gets its own undo step.
    pub fn seal(&mut self) {
        self.history.seal();
    }

    fn after(&mut self, result: Result<ChangeSet, ModelError>) -> Result<ChangeSet, ModelError> {
        // A partly applied edit changed the project even though it failed.
        if matches!(result, Ok(_) | Err(ModelError::Inconsistent(_))) {
            self.touch();
        }
        result
    }

    fn after_step(
        &mut self,
        result: Result<Option<ChangeSet>, ModelError>,
    ) -> Result<Option<ChangeSet>, ModelError> {
        if matches!(result, Ok(Some(_)) | Err(ModelError::Inconsistent(_))) {
            self.touch();
        }
        result
    }

    fn touch(&mut self) {
        self.revision += 1;
        self.snapshot = None;
    }

    /// Whether there are changes since the last save (or since creation).
    pub fn is_modified(&self) -> bool {
        self.saved_at != Some(self.history.position())
    }

    /// An immutable view of the current state for the engine. Calling this
    /// again without edits in between returns the same snapshot.
    pub fn snapshot(&mut self) -> ProjectSnapshot {
        self.snapshot
            .get_or_insert_with(|| ProjectSnapshot::new(self.revision, self.project.clone()))
            .clone()
    }

    /// Saves to the document's bundle.
    pub fn save(&mut self) -> Result<(), FormatError> {
        let bundle = self.bundle.as_ref().ok_or(FormatError::NoBundle)?;
        bundle.save(&self.project)?;
        self.mark_saved();
        Ok(())
    }

    /// Saves into a new bundle directory and makes it the document's bundle.
    /// Audio in the old bundle's `audio/` folder is copied across.
    pub fn save_as(&mut self, dir: impl AsRef<Path>) -> Result<(), FormatError> {
        let new = Bundle::create(dir)?;
        let copied = match &self.bundle {
            Some(old) => copy_dir_files(&old.audio_dir(), &new.audio_dir())?,
            None => Vec::new(),
        };
        if let Err(e) = new.save(&self.project) {
            // Leave the target as it was, so the user can retry there.
            remove_files(&copied);
            return Err(e);
        }
        if let Some(old) = self.bundle.replace(new) {
            // The unsaved edits it held now live in the new bundle. Failing
            // to delete it only means a stale recovery offer later.
            let _ = old.discard_autosave();
        }
        self.mark_saved();
        Ok(())
    }

    fn mark_saved(&mut self) {
        // Don't let the next edit merge into the saved state.
        self.history.seal();
        self.saved_at = Some(self.history.position());
        self.autosaved_revision = self.revision;
    }

    /// Writes an autosave if anything changed since the last save or
    /// autosave, and removes it if the document is back to its saved state. The app calls this on a timer. Returns whether it wrote one.
    /// A document with no bundle yet has nowhere to autosave and returns
    /// `false`.
    pub fn autosave(&mut self) -> Result<bool, FormatError> {
        let Some(bundle) = &self.bundle else {
            return Ok(false);
        };
        if !self.is_modified() {
            // Undone back to the saved state: an older autosave would offer
            // to "recover" edits the user took back.
            if self.autosaved_revision != self.revision {
                bundle.discard_autosave()?;
                self.autosaved_revision = self.revision;
            }
            return Ok(false);
        }
        if self.revision == self.autosaved_revision {
            return Ok(false);
        }
        bundle.write_autosave(&self.project)?;
        self.autosaved_revision = self.revision;
        Ok(true)
    }
}

/// Copies the files in `from` into `to`, refusing before copying anything if
/// a file of the same name is already there, since clips may use it. Returns
/// the files it made. On failure it removes them again, so a retry into the
/// same folder isn't refused over a half-finished copy.
fn copy_dir_files(from: &Path, to: &Path) -> Result<Vec<PathBuf>, FormatError> {
    if !from.is_dir() {
        return Ok(Vec::new());
    }
    std::fs::create_dir_all(to).map_err(FormatError::io(to))?;
    let mut copies = Vec::new();
    for entry in std::fs::read_dir(from).map_err(FormatError::io(from))? {
        let path = entry.map_err(FormatError::io(from))?.path();
        if path.is_file() {
            let dest = to.join(path.file_name().expect("files have names"));
            if dest.exists() {
                return Err(FormatError::io(dest)(
                    std::io::ErrorKind::AlreadyExists.into(),
                ));
            }
            copies.push((path, dest));
        }
    }
    let mut made = Vec::with_capacity(copies.len());
    for (path, dest) in copies {
        // Pushed first: a failed copy can leave part of the file behind.
        made.push(dest);
        let dest = made.last().expect("just pushed");
        if let Err(e) = std::fs::copy(&path, dest) {
            remove_files(&made);
            return Err(FormatError::io(dest)(e));
        }
    }
    Ok(made)
}

/// Removes files [`copy_dir_files`] made. Each was checked not to exist
/// before, so nothing of the user's is lost.
fn remove_files(files: &[PathBuf]) {
    for f in files {
        let _ = std::fs::remove_file(f);
    }
}
