//! The rows an expanded track shows under it in the arrange view.
//!
//! [`track_rows`] is the one place that decides which rows a track has and
//! in what order. [`Arrange::relayout`](crate::Arrange::relayout) keeps its
//! answer for every track for geometry, hit tests and drawing, and a front
//! end's track headers ask it for theirs, so all agree on what row `n` is.

use daw_model::{AutomationLane, AutomationTarget, LaneId, Track};

/// What a row under a track is for.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum RowKind {
    /// The one row of a track without lanes, which offers to add one.
    Placeholder,
    /// One control of the track: its fader, pan, a send's level or a device
    /// parameter.
    Control(AutomationTarget),
}

/// One row under a track.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct LaneRow {
    pub kind: RowKind,
    /// The lane that automates the row's control, drawn in the row.
    pub lane: Option<LaneId>,
}

impl LaneRow {
    /// The control the row is for.
    pub fn target(&self) -> Option<AutomationTarget> {
        match self.kind {
            RowKind::Placeholder => None,
            RowKind::Control(t) => Some(t),
        }
    }

    /// The row's lane in `track`.
    pub fn lane_in<'a>(&self, track: &'a Track) -> Option<&'a AutomationLane> {
        let id = self.lane?;
        track.automation.iter().find(|l| l.id == id)
    }
}

/// The rows `track` shows while expanded: its lanes in the model's order,
/// or a placeholder when it has none.
pub fn track_rows(track: &Track) -> Vec<LaneRow> {
    if track.automation.is_empty() {
        return vec![LaneRow {
            kind: RowKind::Placeholder,
            lane: None,
        }];
    }
    track
        .automation
        .iter()
        .map(|l| LaneRow {
            kind: RowKind::Control(l.target),
            lane: Some(l.id),
        })
        .collect()
}
