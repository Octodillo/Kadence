use std::path::PathBuf;

use crate::id::{ClipId, DeviceId, LaneId, NoteUid, SendId, TrackId};

/// A command could not be applied. The model is left unchanged.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ModelError {
    #[error("track {0} not found")]
    TrackNotFound(TrackId),
    #[error("clip {0} not found")]
    ClipNotFound(ClipId),
    #[error("note {0} not found")]
    NoteNotFound(NoteUid),
    #[error("device {0} not found")]
    DeviceNotFound(DeviceId),
    #[error("send {0} not found")]
    SendNotFound(SendId),
    #[error("automation lane {0} not found")]
    LaneNotFound(LaneId),
    #[error("invalid edit: {0}")]
    Invalid(String),
    /// A command failed and so did undoing its partial work. The project is
    /// still valid but holds part of the edit, and the undo history has been
    /// cleared because it no longer matches. There is no change set for the
    /// partial edit, so refresh everything that shows the project. This is a
    /// bug in a command.
    #[error("edit partly applied and history cleared: {0}")]
    Inconsistent(String),
}

/// Saving or loading a project failed.
#[derive(Debug, thiserror::Error)]
pub enum FormatError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("malformed project file: {0}")]
    Json(#[from] serde_json::Error),
    #[error("not a project file (format is {0:?})")]
    WrongFormat(String),
    #[error(
        "project was saved by a newer version (schema {found}, this build reads up to {supported})"
    )]
    TooNew { found: u64, supported: u64 },
    #[error("no migration from schema version {0}")]
    NoMigration(u64),
    #[error("project is inconsistent: {0}")]
    Model(#[from] ModelError),
    #[error("document has no bundle; save it somewhere first")]
    NoBundle,
    #[error("{0} holds unsaved changes from an earlier session; recover or discard them")]
    AutosavePending(PathBuf),
}

impl FormatError {
    pub(crate) fn io(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> Self {
        let path = path.into();
        move |source| FormatError::Io { path, source }
    }
}
