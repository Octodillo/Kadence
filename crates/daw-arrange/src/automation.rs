//! Automation lanes: their value ranges, the targets a lane can have, and
//! the arrange view's lane rows under its tracks. Point editing and drawing
//! are the [`lane_editor`](crate::lane_editor)'s, shared with other views.

use std::sync::Arc;

use daw_core::event::ParamId;
use daw_core::time::TickTime;
use daw_model::commands::{AddLane, LaneField, RemoveLane, SetLane};
use daw_model::{
    AutomationLane, AutomationPoint, AutomationTarget, Device, Document, LaneId, Project, Track,
    TrackId, TrackKind,
};

use crate::lane_editor::{LaneFrame, LaneGrid, lane_of};
use crate::scene::Rect;
use crate::{Arrange, Button, Hit, Outcome, Pointer};

/// Levels at the bottom of a dB lane are stored as this (what the mixer
/// stores for a fader pulled all the way down).
pub const SILENCE_DB: f64 = -144.0;
/// The range a level lane shows, in dB. Ramps from below the floor play
/// from it (see [`AutomationTarget::ramp_floor`]).
pub const LEVEL_FLOOR_DB: f64 = daw_model::LEVEL_FLOOR_DB;
pub const LEVEL_MAX_DB: f64 = 6.0;

/// How a lane maps its target's values to heights.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum LaneScale {
    /// A level in dB from `floor` to `max`, linear in dB; the bottom is
    /// silence ([`SILENCE_DB`]).
    Db { floor: f64, max: f64 },
    /// Plain values from `min` to `max`. `log` spaces them logarithmically
    /// (`min` must be positive); `steps` rounds to whole numbers (choices,
    /// toggles).
    Range {
        min: f64,
        max: f64,
        log: bool,
        steps: bool,
    },
}

impl LaneScale {
    pub const LEVEL: Self = Self::Db {
        floor: LEVEL_FLOOR_DB,
        max: LEVEL_MAX_DB,
    };
    pub const PAN: Self = Self::Range {
        min: -1.0,
        max: 1.0,
        log: false,
        steps: false,
    };
    pub const UNIT: Self = Self::Range {
        min: 0.0,
        max: 1.0,
        log: false,
        steps: false,
    };

    /// Whether heights are proportional to values (in dB for levels), so
    /// a straight ramp is a straight line.
    pub fn is_linear(self) -> bool {
        !matches!(self, Self::Range { log: true, .. })
    }

    /// `v` as a height from 0 (bottom) to 1 (top).
    pub fn to_norm(self, v: f64) -> f64 {
        let n = match self {
            Self::Db { floor, max } => {
                if v <= floor {
                    0.0
                } else {
                    (v - floor) / (max - floor)
                }
            }
            Self::Range { min, max, log, .. } => {
                if log && min > 0.0 && max > min {
                    (v.max(min) / min).ln() / (max / min).ln()
                } else if max > min {
                    (v - min) / (max - min)
                } else {
                    0.0
                }
            }
        };
        if n.is_finite() {
            n.clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// The value at height `n` (0 to 1).
    pub fn from_norm(self, n: f64) -> f64 {
        let n = n.clamp(0.0, 1.0);
        match self {
            Self::Db { floor, max } => {
                if n <= 0.0 {
                    SILENCE_DB
                } else {
                    // Whole hundredths of a dB, as the mixer shows them.
                    ((floor + n * (max - floor)) * 100.0).round() / 100.0
                }
            }
            Self::Range {
                min,
                max,
                log,
                steps,
            } => {
                let v = if log && min > 0.0 && max > min {
                    min * (max / min).powf(n)
                } else {
                    min + n * (max - min)
                };
                if steps { v.round() } else { v }
            }
        }
    }
}

/// What a front end knows about device parameters: which ones can be
/// automated and their ranges. Built-in devices and plugins describe their
/// parameters differently, so the arrange view asks through this.
pub trait ParamCatalog: Send + Sync {
    /// The parameters of `device` that can be automated, in display order.
    fn automatable(&self, device: &Device) -> Vec<ParamId>;
    /// The range of one parameter, if `device` has it.
    fn scale(&self, device: &Device, param: ParamId) -> Option<LaneScale>;
    /// The value a parameter has when the device doesn't store one.
    fn default_value(&self, device: &Device, param: ParamId) -> Option<f64>;
}

/// Shapes the context menu gives a segment.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PointShape {
    Linear,
    /// Hold, then jump at the next point.
    Step,
    /// Starts slowly and speeds up toward the next value.
    SlowStart,
    /// Moves quickly at first and settles into the next value.
    FastStart,
}

impl PointShape {
    const BEND: f64 = 1.5;

    /// The shape of the segment after `p`.
    pub fn of(p: &AutomationPoint) -> Self {
        if p.step {
            Self::Step
        } else if p.curve < 0.0 {
            Self::SlowStart
        } else if p.curve > 0.0 {
            Self::FastStart
        } else {
            Self::Linear
        }
    }

    pub(crate) fn apply(self, p: &mut AutomationPoint) {
        (p.step, p.curve) = match self {
            Self::Linear => (false, 0.0),
            Self::Step => (true, 0.0),
            Self::SlowStart => (false, -Self::BEND),
            Self::FastStart => (false, Self::BEND),
        };
    }
}

/// A target a new lane could automate on a track.
pub fn lane_targets(
    track: &Track,
    params: Option<&Arc<dyn ParamCatalog>>,
) -> Vec<AutomationTarget> {
    let mut out = vec![AutomationTarget::Volume];
    if track.kind != TrackKind::Master {
        out.push(AutomationTarget::Pan);
    }
    out.extend(
        track
            .sends
            .iter()
            .map(|s| AutomationTarget::SendLevel { send: s.id }),
    );
    if let Some(cat) = params {
        for d in &track.devices {
            out.extend(
                cat.automatable(d)
                    .into_iter()
                    .map(|param| AutomationTarget::DeviceParam {
                        device: d.id,
                        param,
                    }),
            );
        }
    }
    out.retain(|t| track.automation.iter().all(|l| l.target != *t));
    out
}

/// How `target` on `track` maps to heights; device parameters by `params`.
pub fn lane_scale(
    params: Option<&Arc<dyn ParamCatalog>>,
    track: &Track,
    target: AutomationTarget,
) -> LaneScale {
    match target {
        AutomationTarget::Volume | AutomationTarget::SendLevel { .. } => LaneScale::LEVEL,
        AutomationTarget::Pan => LaneScale::PAN,
        AutomationTarget::DeviceParam { device, param } => track
            .devices
            .iter()
            .find(|d| d.id == device)
            .and_then(|d| params?.scale(d, param))
            .unwrap_or(LaneScale::UNIT),
    }
}

/// The value `target` has when no lane plays: the fader, pan knob, send
/// level or device parameter as set.
pub fn static_value(
    params: Option<&Arc<dyn ParamCatalog>>,
    track: &Track,
    target: AutomationTarget,
) -> f64 {
    match target {
        AutomationTarget::Volume => track.volume_db,
        AutomationTarget::Pan => track.pan,
        AutomationTarget::SendLevel { send } => track
            .sends
            .iter()
            .find(|s| s.id == send)
            .map_or(0.0, |s| s.level_db),
        AutomationTarget::DeviceParam { device, param } => {
            let Some(d) = track.devices.iter().find(|d| d.id == device) else {
                return 0.0;
            };
            d.params
                .get(&param)
                .copied()
                .or_else(|| params?.default_value(d, param))
                .unwrap_or(0.0)
        }
    }
}

/// The arrange view's lanes: one row per lane under each expanded track
/// (see [`Arrange::expanded`]), in the model's order, edited by its
/// [`LaneEditor`](crate::lane_editor::LaneEditor).
impl Arrange {
    // -----------------------------------------------------------------------
    // Lanes: ranges, values and geometry

    /// The rectangle lane row `index` of track row `row` covers.
    pub fn lane_rect(&self, row: usize, index: usize) -> Rect {
        Rect::new(0.0, self.lane_y(row, index), self.width, self.lane_height)
    }

    /// The frame of lane row `index` of track row `row`, for `target`.
    pub(crate) fn frame_at(
        &self,
        track: &Track,
        row: usize,
        index: usize,
        target: AutomationTarget,
    ) -> LaneFrame {
        LaneFrame {
            view: self.view,
            rect: self.lane_rect(row, index),
            scale: lane_scale(self.params.as_ref(), track, target),
        }
    }

    /// The grid lane edits snap to.
    pub fn lane_grid(&self, project: &Project) -> LaneGrid {
        LaneGrid {
            snap: self.snap,
            meter: self.meter(project),
        }
    }

    /// The frame `lane` is drawn in, if it shows.
    pub fn lane_frame(&self, project: &Project, lane: LaneId) -> Option<LaneFrame> {
        let (t, l) = lane_of(project, lane)?;
        if !self.expanded.contains(&t.id) {
            return None;
        }
        let row = project.tracks.iter().position(|x| x.id == t.id)?;
        let index = t.automation.iter().position(|x| x.id == lane)?;
        Some(self.frame_at(t, row, index, l.target))
    }

    /// The automation part of [`hit`](Self::hit): `y` is below the clips of
    /// track row `row`.
    pub(crate) fn lane_hit(
        &self,
        track: &Track,
        row: usize,
        x: f64,
        y: f64,
        tick: TickTime,
    ) -> Hit {
        let index = ((y - self.lane_y(row, 0)) / self.lane_height)
            .floor()
            .max(0.0) as usize;
        let lane = track.automation.get(index);
        let point = lane.and_then(|l| self.frame_at(track, row, index, l.target).point_at(l, x, y));
        Hit::Automation {
            track: track.id,
            row,
            lane: lane.map(|l| l.id),
            tick,
            point,
        }
    }

    /// A press on a lane row (see [`press`](Self::press)).
    pub(crate) fn press_lane(&mut self, doc: &mut Document, p: Pointer, hit: Hit) -> Outcome {
        let Hit::Automation {
            track, lane, point, ..
        } = hit
        else {
            return Outcome::default();
        };
        self.selected_track = Some(track);
        self.selection.clear();
        let menu = (p.button == Button::Right).then_some(hit);
        let Some(lane) = lane else {
            self.lanes.clear_selection();
            return Outcome {
                context_menu: menu,
                ..Outcome::view()
            };
        };
        // A hit lane's track is expanded, so its frame is there.
        let project = doc.project();
        let Some(frame) = self.lane_frame(project, lane) else {
            return Outcome::view();
        };
        let grid = self.lane_grid(project);
        let o = self.lanes.press(doc, &frame, &grid, p, lane, point);
        Outcome {
            context_menu: menu,
            ..o
        }
    }

    // -----------------------------------------------------------------------
    // Commands on lanes and points

    /// Deletes the selected automation points. Unlike calling the lane
    /// editor directly, this also ends a clip drag in progress.
    pub fn delete_points(&mut self, doc: &mut Document) -> Outcome {
        self.end_drag(doc);
        self.lanes.delete_points(doc)
    }

    /// Gives the segments after the selected points `shape`. Unlike calling
    /// the lane editor directly, this also ends a clip drag in progress.
    pub fn set_point_shape(&mut self, doc: &mut Document, shape: PointShape) -> Outcome {
        self.end_drag(doc);
        self.lanes.set_point_shape(doc, shape)
    }

    /// Adds a lane for `target` on `track` and shows the track's lanes.
    /// The lane starts with no points, so the control keeps its value
    /// until one is added.
    pub fn add_lane(
        &mut self,
        doc: &mut Document,
        track: TrackId,
        target: AutomationTarget,
    ) -> Outcome {
        self.end_drag(doc);
        let lane = AutomationLane::new(target);
        let r = doc.execute(AddLane { track, lane }).map(|_| ());
        doc.seal();
        if r.is_ok() {
            self.expanded.insert(track);
            self.relayout(doc.project());
        }
        Outcome::from_result(r)
    }

    pub fn remove_lane(&mut self, doc: &mut Document, lane: LaneId) -> Outcome {
        self.end_drag(doc);
        self.lanes.forget_lanes(|l| l == lane);
        let r = doc.execute(RemoveLane::new(lane)).map(|_| ());
        doc.seal();
        self.relayout(doc.project());
        Outcome::from_result(r)
    }

    /// Turns a lane on (it plays) or off (the control keeps its own value).
    pub fn set_lane_enabled(&mut self, doc: &mut Document, lane: LaneId, on: bool) -> Outcome {
        self.end_drag(doc);
        if lane_of(doc.project(), lane).is_some_and(|(_, l)| l.enabled == on) {
            return Outcome::default();
        }
        doc.seal();
        let r = doc
            .execute(SetLane {
                id: lane,
                field: LaneField::Enabled(on),
            })
            .map(|_| ());
        doc.seal();
        Outcome::from_result(r)
    }

    /// Shows or hides a track's lanes.
    pub fn set_expanded(&mut self, project: &Project, track: TrackId, on: bool) -> Outcome {
        let changed = if on {
            self.expanded.insert(track)
        } else {
            self.expanded.remove(&track)
        };
        if !on {
            self.lanes
                .forget_lanes(|l| project.lane_location(l).is_some_and(|(t, _)| t == track));
        }
        self.relayout(project);
        self.clamp_scroll(project);
        if changed {
            Outcome::view()
        } else {
            Outcome::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_round_trip() {
        for s in [LaneScale::LEVEL, LaneScale::PAN, LaneScale::UNIT] {
            for n in [0.1, 0.25, 0.5, 0.9, 1.0] {
                let v = s.from_norm(n);
                assert!((s.to_norm(v) - n).abs() < 1e-3, "{s:?} {n}");
            }
        }
        assert_eq!(LaneScale::LEVEL.from_norm(0.0), SILENCE_DB);
        assert_eq!(LaneScale::LEVEL.to_norm(SILENCE_DB), 0.0);
        let log = LaneScale::Range {
            min: 20.0,
            max: 20000.0,
            log: true,
            steps: false,
        };
        assert!((log.from_norm(0.5) - 632.46).abs() < 0.1);
        assert_eq!(log.to_norm(0.0), 0.0);
        let steps = LaneScale::Range {
            min: 0.0,
            max: 3.0,
            log: false,
            steps: true,
        };
        assert_eq!(steps.from_norm(0.4), 1.0);
    }
}
