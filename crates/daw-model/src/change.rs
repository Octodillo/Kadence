//! Descriptions of what a command changed, so the UI can refresh only what's
//! affected and the engine knows whether to recompile its graph.

use crate::id::{ClipId, DeviceId, LaneId, SendId, TrackId};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Change {
    /// Project name or sample rate.
    Project,
    Tempo,
    /// Tracks were added, removed or reordered.
    TrackList,
    /// A track's own properties (name, fader, routing).
    Track(TrackId),
    /// Clips were added to or removed from a track.
    ClipList(TrackId),
    /// A clip's properties or contents, including its notes.
    Clip(ClipId),
    /// Devices were added, removed or reordered on a track.
    DeviceList(TrackId),
    Device(DeviceId),
    /// Sends were added to or removed from a track.
    SendList(TrackId),
    Send(SendId),
    /// Lanes were added to or removed from a track.
    LaneList(TrackId),
    Lane(LaneId),
}

impl Change {
    /// Whether the engine's processing graph (nodes or connections) may need
    /// recompiling, as opposed to a value it can update in place.
    pub fn affects_graph(&self) -> bool {
        matches!(
            self,
            Change::TrackList | Change::Track(_) | Change::DeviceList(_) | Change::SendList(_)
        )
    }
}

/// Everything one command (or one undo/redo step) changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChangeSet {
    changes: Vec<Change>,
}

impl ChangeSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn one(c: Change) -> Self {
        Self { changes: vec![c] }
    }

    /// Adds a change unless it's already listed.
    pub fn push(&mut self, c: Change) {
        if !self.changes.contains(&c) {
            self.changes.push(c);
        }
    }

    pub fn extend(&mut self, other: ChangeSet) {
        for c in other.changes {
            self.push(c);
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &Change> {
        self.changes.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    pub fn len(&self) -> usize {
        self.changes.len()
    }

    pub fn contains(&self, c: &Change) -> bool {
        self.changes.contains(c)
    }

    pub fn affects_graph(&self) -> bool {
        self.changes.iter().any(Change::affects_graph)
    }
}

impl FromIterator<Change> for ChangeSet {
    fn from_iter<I: IntoIterator<Item = Change>>(iter: I) -> Self {
        let mut s = Self::new();
        for c in iter {
            s.push(c);
        }
        s
    }
}
