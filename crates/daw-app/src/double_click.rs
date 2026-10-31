//! Taking back a double-click's clicks. A slider moves to where it is
//! clicked, so both clicks of a double-click that resets it may have edited
//! it: the reset first undoes those edits, so it leaves one undo step.

use std::time::{Duration, Instant};

use daw_model::{Document, ModelError};

/// How soon after its first click a reset counts as the same double-click.
pub const DOUBLE_CLICK: Duration = Duration::from_millis(600);

/// Undo steps that clicks on control `key` made just now, with nothing else
/// edited since.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Clicks<K> {
    key: K,
    first: Instant,
    steps: usize,
    revision: u64,
}

impl<K: Copy + PartialEq> Clicks<K> {
    /// `clicks` after an edit of `key`: `before` is the document's revision
    /// before it and `undo_len` the history's length then.
    pub fn after_edit(
        clicks: Option<Self>,
        key: K,
        before: u64,
        undo_len: usize,
        doc: &Document,
    ) -> Option<Self> {
        let revision = doc.revision();
        if revision == before {
            return clicks;
        }
        let steps = doc.history().undo_len().saturating_sub(undo_len);
        match clicks {
            Some(c) if c.key == key && c.revision == before && c.first.elapsed() < DOUBLE_CLICK => {
                Some(Self {
                    steps: c.steps + steps,
                    revision,
                    ..c
                })
            }
            // Merged into an older step, which undo would take back too.
            _ if steps == 0 => None,
            _ => Some(Self {
                key,
                first: Instant::now(),
                steps,
                revision,
            }),
        }
    }

    /// Undoes the edits of `clicks` if they were on `key`, just now, and
    /// nothing else changed since.
    pub fn take_back(clicks: Option<Self>, key: K, doc: &mut Document) -> Result<(), ModelError> {
        if let Some(c) = clicks
            && c.key == key
            && c.revision == doc.revision()
            && c.first.elapsed() < DOUBLE_CLICK
        {
            for _ in 0..c.steps {
                doc.undo()?;
            }
        }
        Ok(())
    }
}
