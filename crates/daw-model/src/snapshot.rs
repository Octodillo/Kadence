//! Read-only snapshots for the engine and other threads.

use std::ops::Deref;
use std::sync::Arc;

use crate::model::Project;

/// An immutable copy of the project at one revision.
///
/// Making one is cheap: it copies the track list's `Arc`s, not the tracks.
/// Edits after that copy only the tracks and clips they touch. Snapshots are
/// `Send + Sync`, so the engine can compile one on a worker thread.
#[derive(Clone, Debug)]
pub struct ProjectSnapshot {
    revision: u64,
    project: Arc<Project>,
}

impl ProjectSnapshot {
    pub(crate) fn new(revision: u64, project: Project) -> Self {
        Self {
            revision,
            project: Arc::new(project),
        }
    }

    /// Increases with every change to the document, including undo and redo.
    /// Equal revisions from the same document mean equal contents.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn project(&self) -> &Project {
        &self.project
    }
}

impl Deref for ProjectSnapshot {
    type Target = Project;

    fn deref(&self) -> &Project {
        &self.project
    }
}
