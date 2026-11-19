//! An open project: the model, its history and where it is saved.

use std::path::Path;

use crate::bundle::{Bundle, Link, Target};
use crate::carry;
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
    /// What saving does with a symlinked project file: followed only when
    /// this document was loaded through it, so a link in a bundle whose
    /// project file was never read cannot choose the file a save replaces.
    project_link: Link,
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
            project_link: Link::Replace,
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

    /// Opens a bundle's project file and deletes any autosave. One that
    /// cannot be deleted (the bundle is read-only) stays, and is offered
    /// again at the next open.
    pub fn open_discarding_autosave(path: impl AsRef<Path>) -> Result<Self, FormatError> {
        let bundle = Bundle::open(path)?;
        // Load first: if the project file is missing or broken, the autosave
        // may be the only copy of the work.
        let doc = Self::load(bundle)?;
        let _ = doc.bundle.as_ref().expect("just loaded").discard_autosave();
        Ok(doc)
    }

    fn load(bundle: Bundle) -> Result<Self, FormatError> {
        let project = bundle.load()?;
        Ok(Self {
            bundle: Some(bundle),
            project_link: Link::Follow,
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
            // Not autosaved by this document yet: the next autosave writes,
            // so the work survives even if the file it came from is removed.
            autosaved_revision: u64::MAX,
            ..Self::new(project)
        })
    }

    /// A document for a project that was never saved, recovered from an
    /// autosave kept outside any bundle. It starts out modified, since the
    /// recovered state isn't saved.
    pub fn recovered(project: Project) -> Self {
        Self {
            saved_at: None,
            autosaved_revision: u64::MAX,
            ..Self::new(project)
        }
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

    /// Takes back the step at history position `step` and forgets it, if it
    /// is still the last one. See [`UndoStack::abandon`].
    pub fn abandon(&mut self, step: u64) -> Result<Option<ChangeSet>, ModelError> {
        let result = self.history.abandon(&mut self.project, step);
        self.after_step(result)
    }

    /// Stores a plugin's own state in its device without an undo step: the
    /// plugin changed it, not an edit, so undo neither reverts it nor loses
    /// its redo history to it. Does not mark the document modified.
    pub fn store_plugin_state(
        &mut self,
        device: crate::DeviceId,
        state: Option<crate::StateBlob>,
    ) -> Result<(), ModelError> {
        let d = self.project.device_mut(device)?;
        if d.state != state {
            d.state = state;
            self.touch();
        }
        Ok(())
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
        bundle.save(&self.project, self.project_link)?;
        self.mark_saved();
        Ok(())
    }

    /// Saves into a new bundle and makes it the document's bundle. `path`
    /// is the bundle directory, or the project file `Name.kadence` for the
    /// directory `Name` around it (see [`Bundle::target`]).
    ///
    /// The old bundle's `audio/` folder is copied across, with any other
    /// file in the old bundle that a clip plays. Choosing the document's own
    /// bundle, by its folder or a file in it, is a plain save; another
    /// project's file is refused, since its bundle is not this one's to
    /// write into.
    ///
    /// Devices' files outside `audio/` are left behind: use
    /// [`save_as_with`](Self::save_as_with) for a project whose devices
    /// name files.
    pub fn save_as(&mut self, path: impl AsRef<Path>) -> Result<(), FormatError> {
        self.save_as_with(path, &crate::files::ClipsOnly)
    }

    /// [`save_as`](Self::save_as), also carrying the other files in the
    /// old bundle that devices play, as `state` reads them from their saved
    /// state.
    pub fn save_as_with(
        &mut self,
        path: impl AsRef<Path>,
        state: &dyn crate::files::StateFiles,
    ) -> Result<(), FormatError> {
        let path = path.as_ref();
        let own = |root: &Path| {
            let real = |p: &Path| p.canonicalize().ok();
            self.bundle
                .as_ref()
                .is_some_and(|b| real(root).is_some_and(|r| Some(r) == real(b.root())))
        };
        // Its own project file is the bundle it has, even when that file is
        // not there (the document was recovered from the autosave alone).
        let own_file = self.bundle.as_ref().is_some_and(|b| {
            path.file_name() == b.project_file().file_name() && path.parent().is_some_and(own)
        });
        let dir = match Bundle::target(path)? {
            _ if own_file => return self.save(),
            Target::New(dir) if own(&dir) => return self.save(),
            Target::Existing(chosen) if own(chosen.root()) => return self.save(),
            Target::New(dir) => dir,
            Target::Existing(chosen) => {
                return Err(FormatError::io(chosen.project_file())(
                    std::io::ErrorKind::AlreadyExists.into(),
                ));
            }
        };
        let new = Bundle::create(dir)?;
        let carried = match &self.bundle {
            Some(old) => carry::audio(&self.project, old, &new, state)?,
            None => carry::Made::default(),
        };
        if let Err(e) = new.save(&self.project, Link::Replace) {
            // Leave the target as it was, so the user can retry there.
            carried.remove();
            return Err(e);
        }
        self.project_link = Link::Replace;
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
    /// autosave, and removes it if the document is back to its saved state.
    /// The app calls this on a timer. Returns whether it wrote one. A
    /// document with no bundle yet has nowhere to autosave and returns
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
