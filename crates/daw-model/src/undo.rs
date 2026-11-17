//! The undo history.

use std::collections::VecDeque;

use crate::change::ChangeSet;
use crate::command::Command;
use crate::error::ModelError;
use crate::model::Project;

#[derive(Debug)]
struct Entry {
    /// Unique per entry, so a position in history can be recognised later
    /// (for example, the position the project was saved at).
    seq: u64,
    command: Box<dyn Command>,
}

/// What running a step cost the history, which comes back if the step is
/// [abandoned](UndoStack::abandon).
#[derive(Debug)]
struct Displaced {
    /// The step that displaced it.
    by: u64,
    /// The redo steps it cleared.
    redo: Vec<Entry>,
    /// The oldest undo step, dropped for the limit, and the base position
    /// before that.
    oldest: Option<(Entry, u64)>,
}

/// Undo and redo stacks with merging of consecutive edits.
///
/// A new command merges into the previous one if [`Command::merge`] accepts
/// it and the stack isn't sealed. Call [`UndoStack::seal`] when a gesture ends
/// (mouse release, focus change) so the next drag starts a new undo step.
/// Undo, redo and saving seal the stack too.
#[derive(Debug)]
pub struct UndoStack {
    done: VecDeque<Entry>,
    undone: Vec<Entry>,
    limit: usize,
    sealed: bool,
    next_seq: u64,
    /// Position when `done` is empty. Nonzero once the oldest steps have been
    /// dropped (by the limit or `clear`), because the project then no longer
    /// matches the state before them.
    base_seq: u64,
    /// What the last new step displaced, until another step replaces it.
    displaced: Option<Displaced>,
}

impl Default for UndoStack {
    fn default() -> Self {
        Self::new(1000)
    }
}

impl UndoStack {
    /// A stack that keeps at most `limit` undo steps.
    pub fn new(limit: usize) -> Self {
        Self {
            done: VecDeque::new(),
            undone: Vec::new(),
            limit: limit.max(1),
            sealed: true,
            next_seq: 1,
            base_seq: 0,
            displaced: None,
        }
    }

    /// Applies a command and records it.
    pub fn execute(
        &mut self,
        project: &mut Project,
        mut command: Box<dyn Command>,
    ) -> Result<ChangeSet, ModelError> {
        let changes = command.apply(project).inspect_err(|e| self.on_error(e))?;
        let merged = !self.sealed
            && self
                .done
                .back_mut()
                .is_some_and(|top| top.command.merge(&*command));
        if merged {
            self.undone.clear();
        } else {
            let seq = self.next_seq;
            self.next_seq += 1;
            self.done.push_back(Entry { seq, command });
            let mut displaced = Displaced {
                by: seq,
                redo: std::mem::take(&mut self.undone),
                oldest: None,
            };
            if self.done.len() > self.limit
                && let Some(dropped) = self.done.pop_front()
            {
                let base = std::mem::replace(&mut self.base_seq, dropped.seq);
                displaced.oldest = Some((dropped, base));
            }
            self.displaced = Some(displaced);
        }
        self.sealed = false;
        Ok(changes)
    }

    /// Reverts the last command. Returns `None` if there is nothing to undo.
    pub fn undo(&mut self, project: &mut Project) -> Result<Option<ChangeSet>, ModelError> {
        let Some((entry, changes)) = self.revert_last(project)? else {
            return Ok(None);
        };
        self.undone.push(entry);
        Ok(Some(changes))
    }

    /// Reverts the last command and forgets it, as if it had never run, when
    /// it is the step at `position`: for a gesture the user cancelled, or one
    /// that replaces its step as it goes. Unlike [`undo`](Self::undo) it
    /// leaves the step nothing to redo, and it brings back the redo steps
    /// (and the oldest undo step, at the limit) the command displaced when
    /// it ran. Returns `None`, changing nothing, when history has moved on
    /// from `position`.
    pub fn abandon(
        &mut self,
        project: &mut Project,
        position: u64,
    ) -> Result<Option<ChangeSet>, ModelError> {
        if self.done.back().is_none_or(|e| e.seq != position) {
            return Ok(None);
        }
        let Some((_, changes)) = self.revert_last(project)? else {
            return Ok(None);
        };
        if let Some(d) = self.displaced.take_if(|d| d.by == position) {
            self.undone = d.redo;
            if let Some((oldest, base)) = d.oldest {
                self.done.push_front(oldest);
                self.base_seq = base;
            }
        }
        Ok(Some(changes))
    }

    /// Takes the last command off the history and reverts it. A command
    /// that fails to revert stays.
    fn revert_last(
        &mut self,
        project: &mut Project,
    ) -> Result<Option<(Entry, ChangeSet)>, ModelError> {
        let Some(mut entry) = self.done.pop_back() else {
            return Ok(None);
        };
        self.sealed = true;
        match entry.command.revert(project) {
            Ok(changes) => Ok(Some((entry, changes))),
            Err(e) => {
                self.done.push_back(entry);
                self.on_error(&e);
                Err(e)
            }
        }
    }

    /// Re-applies the last undone command. Returns `None` if there is nothing
    /// to redo.
    pub fn redo(&mut self, project: &mut Project) -> Result<Option<ChangeSet>, ModelError> {
        let Some(mut entry) = self.undone.pop() else {
            return Ok(None);
        };
        self.sealed = true;
        match entry.command.apply(project) {
            Ok(changes) => {
                self.done.push_back(entry);
                Ok(Some(changes))
            }
            Err(e) => {
                self.undone.push(entry);
                self.on_error(&e);
                Err(e)
            }
        }
    }

    /// After a command left the project partly edited, no step in the history
    /// matches the project any more: drop it all and move to a fresh
    /// position, so the document reads as modified.
    fn on_error(&mut self, e: &ModelError) {
        if matches!(e, ModelError::Inconsistent(_)) {
            self.clear();
            self.base_seq = self.next_seq;
            self.next_seq += 1;
        }
    }

    /// Stops the next command from merging into the last one.
    pub fn seal(&mut self) {
        self.sealed = true;
    }

    pub fn can_undo(&self) -> bool {
        !self.done.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.undone.is_empty()
    }

    /// Label of the step Undo would revert, for "Undo Move Clip".
    pub fn undo_label(&self) -> Option<String> {
        self.done.back().map(|e| e.command.label())
    }

    pub fn redo_label(&self) -> Option<String> {
        self.undone.last().map(|e| e.command.label())
    }

    pub fn undo_len(&self) -> usize {
        self.done.len()
    }

    pub fn redo_len(&self) -> usize {
        self.undone.len()
    }

    /// Identifies the current point in history; 0 at the very start.
    /// Two equal positions mean the project is in the same state.
    pub fn position(&self) -> u64 {
        self.done.back().map_or(self.base_seq, |e| e.seq)
    }

    /// Forgets all history.
    pub fn clear(&mut self) {
        self.base_seq = self.position();
        self.done.clear();
        self.undone.clear();
        self.displaced = None;
        self.sealed = true;
    }
}
