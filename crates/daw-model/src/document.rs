//! An open project: the model, its history and where it is saved.

use std::path::Path;

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
    pub fn open(path: impl AsRef<Path>) -> Result<Self, FormatError> {
        let bundle = Bundle::open(path)?;
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
        let changes = self.history.execute(&mut self.project, command)?;
        self.touch();
        Ok(changes)
    }

    pub fn undo(&mut self) -> Result<Option<ChangeSet>, ModelError> {
        let changes = self.history.undo(&mut self.project)?;
        if changes.is_some() {
            self.touch();
        }
        Ok(changes)
    }

    pub fn redo(&mut self) -> Result<Option<ChangeSet>, ModelError> {
        let changes = self.history.redo(&mut self.project)?;
        if changes.is_some() {
            self.touch();
        }
        Ok(changes)
    }

    /// Ends the current gesture so the next edit gets its own undo step.
    pub fn seal(&mut self) {
        self.history.seal();
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
        if let Some(old) = &self.bundle {
            copy_dir_files(&old.audio_dir(), &new.audio_dir())?;
        }
        new.save(&self.project)?;
        self.bundle = Some(new);
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
    /// autosave. The app calls this on a timer. Returns whether it wrote one.
    /// A document with no bundle yet has nowhere to autosave and returns
    /// `false`.
    pub fn autosave(&mut self) -> Result<bool, FormatError> {
        let Some(bundle) = &self.bundle else {
            return Ok(false);
        };
        if !self.is_modified() || self.revision == self.autosaved_revision {
            return Ok(false);
        }
        bundle.write_autosave(&self.project)?;
        self.autosaved_revision = self.revision;
        Ok(true)
    }
}

fn copy_dir_files(from: &Path, to: &Path) -> Result<(), FormatError> {
    if !from.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(to).map_err(FormatError::io(to))?;
    for entry in std::fs::read_dir(from).map_err(FormatError::io(from))? {
        let path = entry.map_err(FormatError::io(from))?.path();
        if path.is_file() {
            let dest = to.join(path.file_name().expect("files have names"));
            std::fs::copy(&path, &dest).map_err(FormatError::io(&dest))?;
        }
    }
    Ok(())
}
