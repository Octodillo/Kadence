//! What pointer gestures on a timeline share: when a press becomes a drag,
//! and the undo step of a drag in progress.

use daw_model::{Command, Document, ModelError};

/// A pointer must move this far before a press becomes a drag.
pub(crate) const DRAG_THRESHOLD: f64 = 3.0;

/// The undo step of a drag in progress.
///
/// A drag is one undo step however many pointer moves it takes. Each move
/// [rewinds](Self::rewind) the project to how it was at the press, works the
/// whole edit out again from there and [runs](Self::run) it as the drag's
/// step. So the step is always one command applied to the state its undo
/// restores, a move back to where the drag began leaves no step at all, and
/// cancelling is a rewind that leaves nothing to redo.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct DragStep(Option<u64>);

impl DragStep {
    /// Takes back what the drag has done so far, and returns whether that
    /// was anything.
    ///
    /// If something else has edited the document since (a take ending, say),
    /// what the drag did stays as a step of its own and this returns `false`.
    pub fn rewind(&mut self, doc: &mut Document) -> Result<bool, ModelError> {
        match self.0.take() {
            Some(step) => Ok(doc.abandon(step)?.is_some()),
            None => Ok(false),
        }
    }

    /// Runs `edit` as the drag's step. Call it after [`rewind`](Self::rewind).
    pub fn run(&mut self, doc: &mut Document, edit: Box<dyn Command>) -> Result<(), ModelError> {
        // On its own, so rewinding takes back this edit and nothing else.
        doc.seal();
        doc.execute_boxed(edit)?;
        self.0 = Some(doc.history().position());
        Ok(())
    }

    /// Whether the drag has a step: the document differs from the press.
    pub fn edited(&self) -> bool {
        self.0.is_some()
    }
}
