//! The editable project document: tracks, clips, devices and automation,
//! with undoable commands and the on-disk project bundle.
//!
//! The model is plain data. Edits go through [`Command`]s, which a
//! [`Document`] runs and records on its [`UndoStack`]. The engine never sees
//! the mutable model; it takes a [`ProjectSnapshot`], which is cheap to make
//! because tracks and clips are shared through `Arc`s and only copied when an
//! edit touches them.
//!
//! See `docs/project-format.md` for the bundle layout and JSON schema.

pub mod bundle;
pub mod change;
pub mod command;
pub mod commands;
pub mod document;
pub mod error;
pub mod files;
pub mod id;
pub mod model;
mod serde_util;
pub mod snapshot;
pub mod undo;

pub use bundle::{Bundle, FORMAT_NAME, SCHEMA_VERSION};
pub use change::{Change, ChangeSet};
pub use command::Command;
pub use document::Document;
pub use error::{FormatError, ModelError};
pub use id::{ClipId, DeviceId, LaneId, NoteUid, SendId, TrackId};
pub use model::*;
pub use snapshot::ProjectSnapshot;
pub use undo::UndoStack;
