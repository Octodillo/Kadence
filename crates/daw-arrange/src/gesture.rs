//! What pointer gestures on a timeline share: when a press becomes a drag,
//! and the undo step of a drag in progress.

use daw_model::{Command, Document, ModelError, Project};

use crate::Outcome;

/// A pointer must move this far before a press becomes a drag.
pub const DRAG_THRESHOLD: f64 = 3.0;

/// The undo step of a drag in progress.
///
/// A drag is one undo step however many pointer moves it takes. Each move
/// that changes anything takes the step back, so the project is as it was at
/// the press, and runs the whole edit again from there as the new step
/// ([`replace`](Self::replace)). So the step is always one command applied
/// to the state its undo restores, a move back to where the drag began
/// leaves no step at all, and cancelling leaves nothing to redo.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DragStep(Option<u64>);

impl DragStep {
    /// One pointer move of the drag. `edit` gives the command that takes
    /// the project it is handed to where the pointer now puts things, or
    /// `None` when they are there already. It must work that out from what
    /// the drag noted at the press, not from where the drag has got to, as
    /// it is asked about the project both as it is and as it was then.
    pub fn replace(
        &mut self,
        doc: &mut Document,
        mut edit: impl FnMut(&Project) -> Option<Box<dyn Command>>,
    ) -> Outcome {
        // Nothing to do for a move that changes nothing (most do: the
        // pointer moves by pixels, the edit by grid steps).
        if edit(doc.project()).is_none() {
            return Outcome::default();
        }
        let rewound = match self.rewind(doc) {
            Ok(rewound) => rewound,
            Err(e) => return Outcome::from_result(Err(e)),
        };
        // `None` now means the pointer is back where the drag began.
        let run = match edit(doc.project()) {
            Some(command) => self.run(doc, command),
            None => Ok(()),
        };
        let mut o = Outcome::from_result(run);
        // A refused edit still leaves the document rewound.
        o.document |= rewound;
        o
    }

    /// Takes back what the drag did (Escape).
    pub fn cancel(mut self, doc: &mut Document) -> Outcome {
        match self.rewind(doc) {
            Ok(false) => Outcome::view(),
            Ok(true) => Outcome::document(),
            Err(e) => Outcome::from_result(Err(e)),
        }
    }

    /// Runs `edit` as the drag's step, which must have none yet: for a
    /// press that edits by itself.
    pub fn run(&mut self, doc: &mut Document, edit: Box<dyn Command>) -> Result<(), ModelError> {
        // On its own, so rewinding takes back this edit and nothing else.
        doc.seal();
        doc.execute_boxed(edit)?;
        self.0 = Some(doc.history().position());
        Ok(())
    }

    /// Takes back what the drag has done so far, and returns whether that
    /// was anything.
    ///
    /// If something else has edited the document since (a take ending, say),
    /// what the drag did stays as a step of its own and this returns `false`.
    fn rewind(&mut self, doc: &mut Document) -> Result<bool, ModelError> {
        match self.0.take() {
            Some(step) => Ok(doc.abandon(step)?.is_some()),
            None => Ok(false),
        }
    }
}
