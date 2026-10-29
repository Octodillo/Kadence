//! The `Command` trait every edit goes through.

use std::any::Any;
use std::fmt::Debug;

use crate::change::ChangeSet;
use crate::error::ModelError;
use crate::model::Project;

/// One undoable edit.
///
/// `apply` and `revert` must be exact inverses: after `apply` then `revert`
/// the project compares equal to what it was before, and the same holds for
/// `revert` then `apply`. A command that returns an error must leave the
/// project unchanged.
///
/// Commands hold everything they need to redo themselves (including the IDs
/// of objects they create), so they can be applied, reverted and re-applied
/// any number of times.
pub trait Command: Any + Send + Debug {
    /// Short human-readable name for the Edit menu, like "Rename Track".
    fn label(&self) -> String;

    fn apply(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError>;

    fn revert(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError>;

    /// Absorbs `next`, which was applied right after `self`, so that both
    /// undo as one step (for example, the many small moves of a fader drag).
    /// Return `true` if merged; `next` is then dropped. Both commands are in
    /// their applied state when this is called.
    fn merge(&mut self, next: &dyn Command) -> bool {
        let _ = next;
        false
    }
}

impl dyn Command {
    /// The concrete command, if it is a `T`.
    pub fn downcast_ref<T: Command>(&self) -> Option<&T> {
        (self as &dyn Any).downcast_ref()
    }
}
