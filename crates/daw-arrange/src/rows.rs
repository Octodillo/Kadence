//! The rows an expanded track shows under it in the arrange view.
//!
//! A track unfolds into its controls, the way a channel strip lists them:
//! volume, pan, the instrument, the effects and the sends. Volume and pan
//! are rows of their own; the rest are [`Group`]s that fold. An open group
//! lists everything in it (an effect is a group of its parameters), a
//! closed one only the controls a lane automates, so automation stays in
//! view while the rest is put away.
//!
//! [`track_rows`] is the one place that decides which rows a track has and
//! in what order. [`Arrange::relayout`](crate::Arrange::relayout) keeps its
//! answer for every track for geometry, hit tests and drawing, and a front
//! end's track headers ask it for theirs, so all agree on what row `n` is.

use std::collections::BTreeSet;

use daw_model::{
    AutomationLane, AutomationTarget, Device, DeviceId, LaneId, Track, TrackId, TrackKind,
};

use crate::automation::ParamCatalog;

/// A fold among a track's rows.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    /// The instrument's parameters, on instrument tracks.
    Instrument,
    /// The effects, each a [`Group::Device`].
    Effects,
    /// One effect's parameters.
    Device(DeviceId),
    /// The levels of the track's sends.
    Sends,
    /// A device's parameters past its first [`FIRST_PARAMS`]: a plugin
    /// may have thousands, too many to list at once.
    More(DeviceId),
}

/// How many of a device's parameters an open device lists before the rest
/// fold into [`Group::More`].
pub const FIRST_PARAMS: usize = 32;

/// What a row under a track is for.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum RowKind {
    /// The heading of a fold; nothing is drawn beside it.
    Group(Group),
    /// One control of the track: its fader, pan, a send's level or a device
    /// parameter.
    Control(AutomationTarget),
}

/// One row under a track.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct LaneRow {
    pub kind: RowKind,
    /// How many groups the row is inside.
    pub depth: u8,
    /// The lane that automates the row's control, drawn in the row.
    pub lane: Option<LaneId>,
    /// Whether a group row is open. Controls are never open.
    pub open: bool,
}

impl LaneRow {
    /// The control the row is for.
    pub fn target(&self) -> Option<AutomationTarget> {
        match self.kind {
            RowKind::Group(_) => None,
            RowKind::Control(t) => Some(t),
        }
    }

    /// The group the row is the heading of.
    pub fn group(&self) -> Option<Group> {
        match self.kind {
            RowKind::Group(g) => Some(g),
            RowKind::Control(_) => None,
        }
    }

    /// The row's lane in `track`.
    pub fn lane_in<'a>(&self, track: &'a Track) -> Option<&'a AutomationLane> {
        let id = self.lane?;
        track.automation.iter().find(|l| l.id == id)
    }
}

/// Which groups are open, per track. Not part of the project: like the
/// expanded tracks, it is how the view is arranged just now.
pub type OpenGroups = BTreeSet<(TrackId, Group)>;

/// Builds one track's rows.
struct Rows<'a> {
    track: &'a Track,
    params: Option<&'a dyn ParamCatalog>,
    open: &'a OpenGroups,
    out: Vec<LaneRow>,
}

impl Rows<'_> {
    fn lane(&self, target: AutomationTarget) -> Option<LaneId> {
        self.track.lane_for(target).map(|l| l.id)
    }

    fn control(&mut self, depth: u8, target: AutomationTarget) {
        self.out.push(LaneRow {
            kind: RowKind::Control(target),
            depth,
            lane: self.lane(target),
            open: false,
        });
    }

    /// Adds the heading of `group` and says whether it is open.
    fn group(&mut self, depth: u8, group: Group) -> bool {
        let open = self.open.contains(&(self.track.id, group));
        self.out.push(LaneRow {
            kind: RowKind::Group(group),
            depth,
            lane: None,
            open,
        });
        open
    }

    /// The parameters of `device`: all that can be automated when `all`,
    /// otherwise those with a lane. Past the first [`FIRST_PARAMS`] the
    /// rest fold into [`Group::More`]. A lane whose parameter the catalog
    /// doesn't list (a plugin's that is not loaded, say) always shows,
    /// after the listed ones.
    fn params(&mut self, depth: u8, device: &Device, all: bool) {
        let listed = self.params.map_or_else(Vec::new, |c| c.automatable(device));
        let target = |param| AutomationTarget::DeviceParam {
            device: device.id,
            param,
        };
        let (first, rest) = listed.split_at(listed.len().min(FIRST_PARAMS));
        self.listed(depth, first.iter().map(|&p| target(p)), all);
        if !rest.is_empty() {
            let more = all && self.group(depth, Group::More(device.id));
            // Closed (or the device itself is), the rest show only where
            // automated, where they would be without the fold.
            let depth = if all { depth + 1 } else { depth };
            self.listed(depth, rest.iter().map(|&p| target(p)), more);
        }
        let track = self.track;
        for lane in &track.automation {
            if let AutomationTarget::DeviceParam { device: d, param } = lane.target
                && d == device.id
                && !listed.contains(&param)
            {
                self.control(depth, lane.target);
            }
        }
    }

    /// Rows for `targets`: all of them when `all`, else those with a lane.
    fn listed(&mut self, depth: u8, targets: impl Iterator<Item = AutomationTarget>, all: bool) {
        for t in targets {
            if all || self.lane(t).is_some() {
                self.control(depth, t);
            }
        }
    }
}

/// The rows `track` shows while expanded. `params` lists what can be
/// automated on a device and tells the instrument from the effects; `open`
/// holds the open groups.
pub fn track_rows(
    track: &Track,
    params: Option<&dyn ParamCatalog>,
    open: &OpenGroups,
) -> Vec<LaneRow> {
    let mut r = Rows {
        track,
        params,
        open,
        out: Vec::new(),
    };
    // The master has no pan and no sends. The arrange view doesn't list
    // it today, but its rows are still right if asked.
    let master = track.kind == TrackKind::Master;
    r.control(0, AutomationTarget::Volume);
    if !master {
        r.control(0, AutomationTarget::Pan);
    }

    let instrument = (track.kind == TrackKind::Instrument)
        .then(|| track.devices.first())
        .flatten()
        .filter(|d| params.is_some_and(|c| c.is_instrument(d)));
    if track.kind == TrackKind::Instrument {
        let all = r.group(0, Group::Instrument);
        if let Some(d) = instrument {
            r.params(1, d, all);
        }
    }

    let effects = &track.devices[usize::from(instrument.is_some())..];
    let listed = r.group(0, Group::Effects);
    for d in effects {
        if listed {
            let all = r.group(1, Group::Device(d.id));
            r.params(2, d, all);
        } else {
            r.params(1, d, false);
        }
    }

    if !master {
        let all = r.group(0, Group::Sends);
        let sends = track.sends.iter();
        r.listed(
            1,
            sends.map(|s| AutomationTarget::SendLevel { send: s.id }),
            all,
        );
    }
    r.out
}
