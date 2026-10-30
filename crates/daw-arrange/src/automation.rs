//! Automation lanes under the tracks: their value ranges, hit testing and
//! point editing.
//!
//! An expanded track (see [`Arrange::expanded`]) shows one lane row per
//! automation lane, in the model's order, under its clips. A click on a lane
//! adds a point there and drags it; a point can be dragged in time (snapped
//! unless Shift is held) and value, deleted with a double click or Delete,
//! and given a shape from the context menu. Alt-dragging between two points
//! bends the segment. Every gesture is one undoable [`SetLane`] edit.

use std::collections::BTreeSet;
use std::sync::Arc;

use daw_core::event::ParamId;
use daw_core::time::TickTime;
use daw_model::commands::{AddLane, LaneField, RemoveLane, SetLane};
use daw_model::{
    AutomationLane, AutomationPoint, AutomationTarget, Device, Document, LaneId, MAX_CURVE,
    Project, Track, TrackId, TrackKind,
};

use crate::scene::Rect;
use crate::{Arrange, Button, Drag, Hit, Outcome, Pointer};

/// Levels at the bottom of a dB lane are stored as this (what the mixer
/// stores for a fader pulled all the way down).
pub const SILENCE_DB: f64 = -144.0;
/// The range a level lane shows, in dB.
pub const LEVEL_FLOOR_DB: f64 = -60.0;
pub const LEVEL_MAX_DB: f64 = 6.0;

/// Pixels a point's square is wide.
const POINT_PX: f64 = 7.0;
/// Space kept free above and below a lane's curve.
const LANE_PAD: f64 = 4.0;
/// Pixels of vertical drag that bend a segment by 1.
const CURVE_PX: f64 = 40.0;

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

    fn apply(self, p: &mut AutomationPoint) {
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

impl Arrange {
    // -----------------------------------------------------------------------
    // Lanes: ranges, values and geometry

    /// How `target` on `track` maps to heights.
    pub fn lane_scale(&self, track: &Track, target: AutomationTarget) -> LaneScale {
        match target {
            AutomationTarget::Volume | AutomationTarget::SendLevel { .. } => LaneScale::LEVEL,
            AutomationTarget::Pan => LaneScale::PAN,
            AutomationTarget::DeviceParam { device, param } => track
                .devices
                .iter()
                .find(|d| d.id == device)
                .and_then(|d| self.params.as_ref()?.scale(d, param))
                .unwrap_or(LaneScale::UNIT),
        }
    }

    /// The value `target` has when no lane plays: the fader, pan knob,
    /// send level or device parameter as set.
    pub fn static_value(&self, track: &Track, target: AutomationTarget) -> f64 {
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
                    .or_else(|| self.params.as_ref()?.default_value(d, param))
                    .unwrap_or(0.0)
            }
        }
    }

    /// The rectangle lane row `index` of track row `row` covers.
    pub fn lane_rect(&self, row: usize, index: usize) -> Rect {
        Rect::new(0.0, self.lane_y(row, index), self.width, self.lane_height)
    }

    /// The y of height `n` (0 to 1) inside a lane rectangle.
    pub(crate) fn norm_to_y(&self, r: &Rect, n: f64) -> f64 {
        let pad = LANE_PAD.min(r.h / 4.0);
        r.bottom() - pad - n * (r.h - 2.0 * pad)
    }

    fn y_to_norm(&self, r: &Rect, y: f64) -> f64 {
        let pad = LANE_PAD.min(r.h / 4.0);
        ((r.bottom() - pad - y) / (r.h - 2.0 * pad).max(1.0)).clamp(0.0, 1.0)
    }

    /// Where point `p` of a lane in `r` is drawn.
    pub fn point_pos(&self, r: &Rect, scale: LaneScale, p: &AutomationPoint) -> (f64, f64) {
        (
            self.view.tick_to_x(p.tick),
            self.norm_to_y(r, scale.to_norm(p.value)),
        )
    }

    pub(crate) fn point_px(&self) -> f64 {
        POINT_PX
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
        let Some(lane) = track.automation.get(index) else {
            return Hit::Automation {
                track: track.id,
                row,
                lane: None,
                tick,
                point: None,
            };
        };
        let r = self.lane_rect(row, index);
        let scale = self.lane_scale(track, lane.target);
        let reach = POINT_PX / 2.0 + 2.0;
        // The nearest point within reach; later points paint on top.
        let point = lane
            .points
            .iter()
            .enumerate()
            .rev()
            .map(|(i, p)| {
                let (px, py) = self.point_pos(&r, scale, p);
                (i, (px - x).abs().max((py - y).abs()))
            })
            .filter(|&(_, d)| d <= reach)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i);
        Hit::Automation {
            track: track.id,
            row,
            lane: Some(lane.id),
            tick,
            point,
        }
    }

    fn lane_of<'a>(project: &'a Project, lane: LaneId) -> Option<(&'a Track, &'a AutomationLane)> {
        let (track, index) = project.lane_location(lane)?;
        let t = project.track(track)?;
        Some((t, t.automation.get(index)?))
    }

    // -----------------------------------------------------------------------
    // Pointer input on lanes

    pub(crate) fn press_lane(&mut self, doc: &mut Document, p: Pointer, hit: Hit) -> Outcome {
        let Hit::Automation {
            track,
            row,
            lane,
            tick,
            point,
        } = hit
        else {
            return Outcome::default();
        };
        self.selected_track = Some(track);
        self.selection.clear();
        let Some(lane) = lane else {
            self.points = None;
            return Outcome {
                context_menu: (p.button == Button::Right).then_some(hit),
                ..Outcome::view()
            };
        };
        if p.button == Button::Middle {
            return Outcome::default();
        }
        // The selection belongs to one lane.
        if self.points.as_ref().is_some_and(|(l, _)| *l != lane) {
            self.points = None;
        }
        let selected = |a: &Self, i: usize| a.points.as_ref().is_some_and(|(_, s)| s.contains(&i));
        if p.button == Button::Right {
            if let Some(i) = point
                && !selected(self, i)
            {
                self.points = Some((lane, BTreeSet::from([i])));
            }
            return Outcome {
                context_menu: Some(hit),
                ..Outcome::view()
            };
        }
        let project = doc.project();
        let Some((t, l)) = Self::lane_of(project, lane) else {
            return Outcome::view();
        };
        let orig = l.points.clone();
        match point {
            Some(i) => {
                if p.ctrl {
                    let sel = &mut self.points.get_or_insert_with(|| (lane, BTreeSet::new())).1;
                    if !sel.remove(&i) {
                        sel.insert(i);
                    }
                    // Toggling isn't a drag.
                    return Outcome::view();
                }
                if !selected(self, i) {
                    let sel = &mut self.points.get_or_insert_with(|| (lane, BTreeSet::new())).1;
                    if !p.shift {
                        sel.clear();
                    }
                    sel.insert(i);
                }
                let moving = self
                    .points
                    .as_ref()
                    .map(|(_, s)| s.iter().copied().collect())
                    .unwrap_or_default();
                doc.seal();
                self.drag = Some(Drag::Points {
                    lane,
                    tick: self.view.x_to_tick(p.x),
                    y: p.y,
                    lead: i,
                    orig,
                    moving,
                    edited: false,
                });
                Outcome::view()
            }
            None if p.alt => {
                // Bend the segment under the pointer.
                let after = orig.partition_point(|q| q.tick <= tick);
                let Some(index) = after.checked_sub(1).filter(|&i| i + 1 < orig.len()) else {
                    return Outcome::view();
                };
                doc.seal();
                self.points = None;
                self.drag = Some(Drag::Curve {
                    lane,
                    index,
                    y: p.y,
                    orig,
                    edited: false,
                });
                Outcome::view()
            }
            None => {
                // Add a point here and drag it.
                let index = t.automation.iter().position(|x| x.id == lane).unwrap_or(0);
                let r = self.lane_rect(row, index);
                let scale = self.lane_scale(t, l.target);
                let at = if p.shift {
                    tick
                } else {
                    self.snap
                        .apply(tick, self.view.px_per_tick(), &self.meter(project))
                };
                let at = TickTime(at.0.max(0));
                let value = scale.from_norm(self.y_to_norm(&r, p.y));
                let mut points = orig.clone();
                let i = points.partition_point(|q| q.tick < at);
                let mut new = AutomationPoint::new(at, value);
                if let Some(q) = points.get(i).filter(|q| q.tick == at) {
                    // A point is already there: replace its value.
                    new.curve = q.curve;
                    new.step = q.step;
                    points[i] = new;
                } else {
                    // Keep the shape of the segment the point splits.
                    if let Some(prev) = i.checked_sub(1).map(|j| &points[j]) {
                        new.step = prev.step;
                    }
                    points.insert(i, new);
                }
                doc.seal();
                let r = doc
                    .execute(SetLane {
                        id: lane,
                        field: LaneField::Points(points.clone()),
                    })
                    .map(|_| ());
                if r.is_err() {
                    return Outcome::from_result(r);
                }
                self.points = Some((lane, BTreeSet::from([i])));
                self.drag = Some(Drag::Points {
                    lane,
                    tick: at,
                    y: p.y,
                    lead: i,
                    orig: points,
                    moving: vec![i],
                    edited: true,
                });
                Outcome::document()
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn drag_points(
        &mut self,
        doc: &mut Document,
        p: Pointer,
        lane: LaneId,
        tick: TickTime,
        y: f64,
        lead: usize,
        orig: &[AutomationPoint],
        moving: &[usize],
    ) -> Outcome {
        let project = doc.project();
        let Some((t, l)) = Self::lane_of(project, lane) else {
            self.end_drag(doc);
            return Outcome::default();
        };
        if moving.iter().any(|&i| i >= orig.len()) || lead >= orig.len() {
            self.end_drag(doc);
            return Outcome::default();
        }
        let (Some(row), Some(index)) = (
            project.tracks.iter().position(|x| x.id == t.id),
            t.automation.iter().position(|x| x.id == lane),
        ) else {
            return Outcome::default();
        };
        let r = self.lane_rect(row, index);
        let scale = self.lane_scale(t, l.target);

        // Time: snap the grabbed point, move the rest with it, and keep
        // every moved point between its unmoved neighbours.
        let raw = TickTime(orig[lead].tick.0 + (self.view.x_to_tick(p.x).0 - tick.0));
        let snapped = if p.shift {
            raw
        } else {
            self.snap
                .apply(raw, self.view.px_per_tick(), &self.meter(project))
        };
        let mut delta = snapped.0 - orig[lead].tick.0;
        let is_moving = |i: usize| moving.contains(&i);
        for &i in moving {
            let lo = (0..i)
                .rev()
                .find(|&j| !is_moving(j))
                .map_or(0, |j| orig[j].tick.0 + 1);
            let hi = (i + 1..orig.len())
                .find(|&j| !is_moving(j))
                .map(|j| orig[j].tick.0 - 1);
            delta = delta.max(lo - orig[i].tick.0);
            if let Some(hi) = hi {
                delta = delta.min(hi - orig[i].tick.0);
            }
        }
        // Value: by the same height for all.
        let pad = LANE_PAD.min(r.h / 4.0);
        let dn = (y - p.y) / (r.h - 2.0 * pad).max(1.0);
        let mut points = orig.to_vec();
        for &i in moving {
            let q = &mut points[i];
            q.tick = TickTime(orig[i].tick.0 + delta);
            q.value = scale.from_norm(scale.to_norm(orig[i].value) + dn);
        }
        if points == l.points {
            return Outcome::default();
        }
        let r = doc
            .execute(SetLane {
                id: lane,
                field: LaneField::Points(points),
            })
            .map(|_| ());
        Outcome::from_result(r)
    }

    pub(crate) fn drag_curve(
        &mut self,
        doc: &mut Document,
        p: Pointer,
        lane: LaneId,
        index: usize,
        y: f64,
        orig: &[AutomationPoint],
    ) -> Outcome {
        let Some((t, l)) = Self::lane_of(doc.project(), lane) else {
            self.end_drag(doc);
            return Outcome::default();
        };
        let (Some(a), Some(b)) = (orig.get(index), orig.get(index + 1)) else {
            self.end_drag(doc);
            return Outcome::default();
        };
        let scale = self.lane_scale(t, l.target);
        // Dragging up lifts the middle of the segment, whichever way it
        // goes: a rising segment rises early, a falling one falls late.
        let up = (y - p.y) / CURVE_PX;
        let rising = scale.to_norm(b.value) >= scale.to_norm(a.value);
        let bend = if rising { up } else { -up };
        let mut points = orig.to_vec();
        let q = &mut points[index];
        q.step = false;
        q.curve = ((a.curve + bend) * 10.0).round() / 10.0;
        q.curve = q.curve.clamp(-MAX_CURVE, MAX_CURVE);
        if points == l.points {
            return Outcome::default();
        }
        let r = doc
            .execute(SetLane {
                id: lane,
                field: LaneField::Points(points),
            })
            .map(|_| ());
        Outcome::from_result(r)
    }

    // -----------------------------------------------------------------------
    // Commands on lanes and points

    /// Deletes the selected automation points.
    pub fn delete_points(&mut self, doc: &mut Document) -> Outcome {
        self.end_drag(doc);
        let Some((lane, sel)) = self.points.take() else {
            return Outcome::default();
        };
        let Some((_, l)) = Self::lane_of(doc.project(), lane) else {
            return Outcome::view();
        };
        let points: Vec<AutomationPoint> = l
            .points
            .iter()
            .enumerate()
            .filter(|(i, _)| !sel.contains(i))
            .map(|(_, p)| p.clone())
            .collect();
        if points.len() == l.points.len() {
            return Outcome::view();
        }
        self.set_points(doc, lane, points)
    }

    /// Gives the segments after the selected points `shape`.
    pub fn set_point_shape(&mut self, doc: &mut Document, shape: PointShape) -> Outcome {
        self.end_drag(doc);
        let Some((lane, sel)) = self.points.clone() else {
            return Outcome::default();
        };
        let Some((_, l)) = Self::lane_of(doc.project(), lane) else {
            return Outcome::view();
        };
        let mut points = l.points.clone();
        for i in sel {
            if let Some(p) = points.get_mut(i) {
                shape.apply(p);
            }
        }
        self.set_points(doc, lane, points)
    }

    /// The shape of the segment after the first selected point, for the
    /// context menu to check.
    pub fn selected_shape(&self, project: &Project) -> Option<PointShape> {
        let (lane, sel) = self.points.as_ref()?;
        let p = Self::lane_of(project, *lane)?.1.points.get(*sel.first()?)?;
        Some(if p.step {
            PointShape::Step
        } else if p.curve < 0.0 {
            PointShape::SlowStart
        } else if p.curve > 0.0 {
            PointShape::FastStart
        } else {
            PointShape::Linear
        })
    }

    fn set_points(
        &mut self,
        doc: &mut Document,
        lane: LaneId,
        points: Vec<AutomationPoint>,
    ) -> Outcome {
        doc.seal();
        let r = doc
            .execute(SetLane {
                id: lane,
                field: LaneField::Points(points),
            })
            .map(|_| ());
        doc.seal();
        Outcome::from_result(r)
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
        if self.points.as_ref().is_some_and(|(l, _)| *l == lane) {
            self.points = None;
        }
        let r = doc.execute(RemoveLane::new(lane)).map(|_| ());
        doc.seal();
        self.relayout(doc.project());
        Outcome::from_result(r)
    }

    /// Turns a lane on (it plays) or off (the control keeps its own value).
    pub fn set_lane_enabled(&mut self, doc: &mut Document, lane: LaneId, on: bool) -> Outcome {
        self.end_drag(doc);
        if Self::lane_of(doc.project(), lane).is_some_and(|(_, l)| l.enabled == on) {
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
        if !on
            && self
                .points
                .as_ref()
                .is_some_and(|(l, _)| project.lane_location(*l).is_some_and(|(t, _)| t == track))
        {
            self.points = None;
        }
        self.relayout(project);
        self.clamp_scroll(project);
        if changed {
            Outcome::view()
        } else {
            Outcome::default()
        }
    }

    /// Whether automation points are selected (for enabling Delete).
    pub fn points_selected(&self) -> bool {
        self.points.as_ref().is_some_and(|(_, s)| !s.is_empty())
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
