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
        }
    }

    /// Applies a command and records it.
    pub fn execute(
        &mut self,
        project: &mut Project,
        mut command: Box<dyn Command>,
    ) -> Result<ChangeSet, ModelError> {
        let changes = command.apply(project)?;
        self.undone.clear();
        let merged = !self.sealed
            && self
                .done
                .back_mut()
                .is_some_and(|top| top.command.merge(&*command));
        if !merged {
            let seq = self.next_seq;
            self.next_seq += 1;
            self.done.push_back(Entry { seq, command });
            if self.done.len() > self.limit {
                self.done.pop_front();
            }
        }
        self.sealed = false;
        Ok(changes)
    }

    /// Reverts the last command. Returns `None` if there is nothing to undo.
    pub fn undo(&mut self, project: &mut Project) -> Result<Option<ChangeSet>, ModelError> {
        let Some(mut entry) = self.done.pop_back() else {
            return Ok(None);
        };
        self.sealed = true;
        match entry.command.revert(project) {
            Ok(changes) => {
                self.undone.push(entry);
                Ok(Some(changes))
            }
            Err(e) => {
                self.done.push_back(entry);
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
                Err(e)
            }
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

    /// Identifies the current point in history; 0 when nothing is done.
    /// Two equal positions mean the project is in the same state.
    pub fn position(&self) -> u64 {
        self.done.back().map_or(0, |e| e.seq)
    }

    /// Forgets all history.
    pub fn clear(&mut self) {
        self.done.clear();
        self.undone.clear();
        self.sealed = true;
    }
}
