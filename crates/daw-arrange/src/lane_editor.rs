//! Editing and drawing one automation lane on a timeline, for any view
//! that shows lanes (the arrange view under its tracks, the piano roll
//! under its notes).
//!
//! A view hands the editor a [`LaneFrame`] for the lane it is working on:
//! where the lane is on screen, how times map to x, and how the lane's
//! values map to heights. Presses and drags also take the [`LaneGrid`]
//! that edits snap to. The editor keeps what belongs to lanes
//! rather than to the view: the selected points and the drag in progress.
//!
//! A click on a lane adds a point there and drags it; a point can be
//! dragged in time (snapped unless Shift is held) and value, deleted with a
//! double click or Delete, and given a shape from the context menu.
//! Alt-dragging between two points bends the segment. Every gesture is one
//! undoable [`SetLane`] edit.

use std::collections::BTreeSet;

use daw_core::time::TickTime;
use daw_midi::grid::Meter;
use daw_model::commands::{LaneField, SetLane};
use daw_model::{AutomationLane, AutomationPoint, Document, LaneId, MAX_CURVE, Project, Track};

use crate::automation::{LaneScale, PointShape};
use crate::scene::{Palette, Prim, Rect, Rgba, Scene};
use crate::{Button, Cursor, Outcome, Pointer, Snap, TimelineView};

/// Pixels a point's square is wide.
pub const POINT_PX: f64 = 7.0;
/// Space kept free above and below a lane's curve.
const LANE_PAD: f64 = 4.0;
/// Pixels of vertical drag that bend a segment by 1.
const CURVE_PX: f64 = 40.0;
/// Straight pieces a curved segment is drawn with.
const CURVE_SEGMENTS: usize = 16;

/// The grid lane edits snap to. Kept apart from [`LaneFrame`] so drawing
/// and hit tests, which never snap, don't build a [`Meter`].
#[derive(Clone, Debug)]
pub struct LaneGrid {
    pub snap: Snap,
    pub meter: Meter,
}

impl LaneGrid {
    /// `tick` on the grid at `frame`'s zoom, unless `free` (Shift held).
    fn apply(&self, frame: &LaneFrame, tick: TickTime, free: bool) -> TickTime {
        if free {
            tick
        } else {
            self.snap.apply(tick, frame.view.px_per_tick(), &self.meter)
        }
    }
}

/// Where a lane is drawn and how it maps time and values there.
#[derive(Clone, Debug)]
pub struct LaneFrame {
    pub view: TimelineView,
    /// The lane's rectangle; its width is the timeline's.
    pub rect: Rect,
    pub scale: LaneScale,
}

impl LaneFrame {
    fn pad(&self) -> f64 {
        LANE_PAD.min(self.rect.h / 4.0)
    }

    /// Pixels from height 0 to height 1.
    fn span(&self) -> f64 {
        (self.rect.h - 2.0 * self.pad()).max(1.0)
    }

    /// The y of height `n` (0 to 1).
    pub fn norm_to_y(&self, n: f64) -> f64 {
        self.rect.bottom() - self.pad() - n * self.span()
    }

    /// The height (0 to 1) at `y`.
    pub fn y_to_norm(&self, y: f64) -> f64 {
        ((self.rect.bottom() - self.pad() - y) / self.span()).clamp(0.0, 1.0)
    }

    /// The y of value `v`.
    pub fn value_y(&self, v: f64) -> f64 {
        self.norm_to_y(self.scale.to_norm(v))
    }

    /// Where point `p` is drawn.
    pub fn point_pos(&self, p: &AutomationPoint) -> (f64, f64) {
        (self.view.tick_to_x(p.tick), self.value_y(p.value))
    }

    /// The point of `lane` under (`x`, `y`), if any; later points paint on
    /// top, so they win.
    pub fn point_at(&self, lane: &AutomationLane, x: f64, y: f64) -> Option<usize> {
        let reach = POINT_PX / 2.0 + 2.0;
        lane.points
            .iter()
            .enumerate()
            .rev()
            .map(|(i, p)| {
                let (px, py) = self.point_pos(p);
                (i, (px - x).abs().max((py - y).abs()))
            })
            .filter(|&(_, d)| d <= reach)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }
}

/// A lane and its track, by the lane's id.
pub fn lane_of(project: &Project, lane: LaneId) -> Option<(&Track, &AutomationLane)> {
    let (track, index) = project.lane_location(lane)?;
    let t = project.track(track)?;
    Some((t, t.automation.get(index)?))
}

#[derive(Clone, Debug)]
enum LaneDrag {
    /// Points being moved.
    Points {
        lane: LaneId,
        /// Tick and y under the pointer at the press.
        tick: TickTime,
        y: f64,
        /// The point grabbed; its tick is what snaps.
        lead: usize,
        /// The lane's points at the press, and the indices being moved.
        orig: Vec<AutomationPoint>,
        moving: Vec<usize>,
        edited: bool,
    },
    /// The bend of the segment after point `index`.
    Curve {
        lane: LaneId,
        index: usize,
        y: f64,
        orig: Vec<AutomationPoint>,
        edited: bool,
    },
}

impl LaneDrag {
    fn lane(&self) -> LaneId {
        match self {
            Self::Points { lane, .. } | Self::Curve { lane, .. } => *lane,
        }
    }

    fn edited(&mut self) -> &mut bool {
        match self {
            Self::Points { edited, .. } | Self::Curve { edited, .. } => edited,
        }
    }
}

/// The selected points and the drag on lanes, for one view.
#[derive(Clone, Debug, Default)]
pub struct LaneEditor {
    /// Selected points: a lane and indices into its points. Dropped when
    /// the lane's points change other than by this editor's own gestures
    /// (undo, a touch-write take), as the indices may then point elsewhere.
    selection: Option<(LaneId, BTreeSet<usize>)>,
    /// The selected lane's points when the selection was last set or
    /// edited here.
    seen: Vec<AutomationPoint>,
    drag: Option<LaneDrag>,
}

impl LaneEditor {
    // -----------------------------------------------------------------------
    // Selection

    /// The selection, if its lane's points are as this editor left them.
    pub fn selection(&self, project: &Project) -> Option<&(LaneId, BTreeSet<usize>)> {
        self.selection
            .as_ref()
            .filter(|(lane, _)| lane_of(project, *lane).is_some_and(|(_, l)| l.points == self.seen))
    }

    /// Whether points are selected (for enabling Delete).
    pub fn points_selected(&self, project: &Project) -> bool {
        self.selection(project).is_some_and(|(_, s)| !s.is_empty())
    }

    /// Whether point `index` of `lane` is selected, for drawing.
    /// The lane the selection is on, unchecked against the project (see
    /// [`check`](Self::check)).
    pub fn selected_lane(&self) -> Option<LaneId> {
        self.selection.as_ref().map(|(l, _)| *l)
    }

    fn is_selected(&self, lane: LaneId, index: usize) -> bool {
        self.selection
            .as_ref()
            .is_some_and(|(l, s)| *l == lane && s.contains(&index))
    }

    /// Selects only point `index` of `lane`.
    pub fn select_point(&mut self, project: &Project, lane: LaneId, index: usize) {
        self.selection = Some((lane, BTreeSet::from([index])));
        self.saw(project);
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    /// Drops the selection if it is on a lane `gone` says no longer shows.
    pub fn forget_lanes(&mut self, gone: impl Fn(LaneId) -> bool) {
        if self.selection.as_ref().is_some_and(|(l, _)| gone(*l)) {
            self.selection = None;
        }
    }

    /// Notes the selected lane's points as they are now, after a gesture
    /// here selected or edited them.
    fn saw(&mut self, project: &Project) {
        self.seen.clear();
        if let Some((_, l)) = self
            .selection
            .as_ref()
            .and_then(|(lane, _)| lane_of(project, *lane))
        {
            self.seen.extend_from_slice(&l.points);
        }
    }

    /// Drops a selection that something else (undo, a take) has made
    /// stale. Views call it whenever the project may have changed.
    pub fn check(&mut self, project: &Project) {
        if self.selection(project).is_none() {
            self.selection = None;
        }
    }

    // -----------------------------------------------------------------------
    // Pointer input

    /// The lane being dragged on, whose frame [`drag`](Self::drag) needs.
    pub fn dragging(&self) -> Option<LaneId> {
        self.drag.as_ref().map(LaneDrag::lane)
    }

    /// The pointer shape while dragging on a lane.
    pub fn cursor(&self) -> Option<Cursor> {
        self.drag.as_ref().map(|d| match d {
            LaneDrag::Points { .. } => Cursor::Move,
            LaneDrag::Curve { .. } => Cursor::ResizeVertical,
        })
    }

    /// A press on `lane`, drawn in `frame` and snapping to `grid`, over its
    /// point `point` if any.
    /// A right press selects the point for the context menu, which the view
    /// opens.
    pub fn press(
        &mut self,
        doc: &mut Document,
        frame: &LaneFrame,
        grid: &LaneGrid,
        p: Pointer,
        lane: LaneId,
        point: Option<usize>,
    ) -> Outcome {
        let o = self.press_inner(doc, frame, grid, p, lane, point);
        self.saw(doc.project());
        o
    }

    fn press_inner(
        &mut self,
        doc: &mut Document,
        frame: &LaneFrame,
        grid: &LaneGrid,
        p: Pointer,
        lane: LaneId,
        point: Option<usize>,
    ) -> Outcome {
        if p.button == Button::Middle {
            return Outcome::default();
        }
        // The selection belongs to one lane.
        if self.selection.as_ref().is_some_and(|(l, _)| *l != lane) {
            self.selection = None;
        }
        let selected =
            |e: &Self, i: usize| e.selection.as_ref().is_some_and(|(_, s)| s.contains(&i));
        if p.button == Button::Right {
            if let Some(i) = point
                && !selected(self, i)
            {
                self.selection = Some((lane, BTreeSet::from([i])));
            }
            return Outcome::view();
        }
        let Some((_, l)) = lane_of(doc.project(), lane) else {
            return Outcome::view();
        };
        let orig = l.points.clone();
        let tick = frame.view.x_to_tick(p.x);
        match point {
            Some(i) => {
                if p.ctrl {
                    let sel = &mut self
                        .selection
                        .get_or_insert_with(|| (lane, BTreeSet::new()))
                        .1;
                    if !sel.remove(&i) {
                        sel.insert(i);
                    }
                    // Toggling isn't a drag.
                    return Outcome::view();
                }
                if !selected(self, i) {
                    let sel = &mut self
                        .selection
                        .get_or_insert_with(|| (lane, BTreeSet::new()))
                        .1;
                    if !p.shift {
                        sel.clear();
                    }
                    sel.insert(i);
                }
                let moving = self
                    .selection
                    .as_ref()
                    .map(|(_, s)| s.iter().copied().collect())
                    .unwrap_or_default();
                doc.seal();
                self.drag = Some(LaneDrag::Points {
                    lane,
                    tick,
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
                self.selection = None;
                self.drag = Some(LaneDrag::Curve {
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
                let at = TickTime(grid.apply(frame, tick, p.shift).0.max(0));
                let value = frame.scale.from_norm(frame.y_to_norm(p.y));
                let mut points = orig;
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
                let r = set_lane_points(doc, lane, points.clone());
                if r.is_err() {
                    return Outcome::from_result(r);
                }
                self.selection = Some((lane, BTreeSet::from([i])));
                self.drag = Some(LaneDrag::Points {
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

    /// Moves the drag in progress to `p`. `frame` is that of the lane
    /// [`dragging`](Self::dragging) names, or `None` if it no longer shows,
    /// which ends the drag; moved points snap to `grid`.
    pub fn drag(
        &mut self,
        doc: &mut Document,
        frame: Option<&LaneFrame>,
        grid: &LaneGrid,
        p: Pointer,
    ) -> Outcome {
        let (Some(drag), Some(frame)) = (self.drag.clone(), frame) else {
            self.end_drag(doc);
            return Outcome::default();
        };
        let o = match drag {
            LaneDrag::Points {
                lane,
                tick,
                y,
                lead,
                orig,
                moving,
                ..
            } => self.drag_points(doc, frame, grid, p, lane, tick, y, lead, &orig, &moving),
            LaneDrag::Curve {
                lane,
                index,
                y,
                orig,
                ..
            } => self.drag_curve(doc, frame, p, lane, index, y, &orig),
        };
        if o.document
            && let Some(d) = &mut self.drag
        {
            *d.edited() = true;
        }
        o
    }

    #[allow(clippy::too_many_arguments)]
    fn drag_points(
        &mut self,
        doc: &mut Document,
        frame: &LaneFrame,
        grid: &LaneGrid,
        p: Pointer,
        lane: LaneId,
        tick: TickTime,
        y: f64,
        lead: usize,
        orig: &[AutomationPoint],
        moving: &[usize],
    ) -> Outcome {
        let Some((_, l)) = lane_of(doc.project(), lane) else {
            self.end_drag(doc);
            return Outcome::default();
        };
        if moving.iter().any(|&i| i >= orig.len()) || lead >= orig.len() {
            self.end_drag(doc);
            return Outcome::default();
        }
        // Time: snap the grabbed point, move the rest with it, and keep
        // every moved point between its unmoved neighbours.
        let raw = TickTime(orig[lead].tick.0 + (frame.view.x_to_tick(p.x).0 - tick.0));
        let mut delta = grid.apply(frame, raw, p.shift).0 - orig[lead].tick.0;
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
        let dn = (y - p.y) / frame.span();
        let scale = frame.scale;
        let mut points = orig.to_vec();
        for &i in moving {
            let q = &mut points[i];
            q.tick = TickTime(orig[i].tick.0 + delta);
            // Moving only in time keeps values exactly, even ones outside
            // what the lane shows.
            if dn != 0.0 {
                q.value = scale.from_norm(scale.to_norm(orig[i].value) + dn);
            }
        }
        if points == l.points {
            return Outcome::default();
        }
        let r = set_lane_points(doc, lane, points);
        self.saw(doc.project());
        Outcome::from_result(r)
    }

    #[allow(clippy::too_many_arguments)]
    fn drag_curve(
        &mut self,
        doc: &mut Document,
        frame: &LaneFrame,
        p: Pointer,
        lane: LaneId,
        index: usize,
        y: f64,
        orig: &[AutomationPoint],
    ) -> Outcome {
        let Some((_, l)) = lane_of(doc.project(), lane) else {
            self.end_drag(doc);
            return Outcome::default();
        };
        let (Some(a), Some(b)) = (orig.get(index), orig.get(index + 1)) else {
            self.end_drag(doc);
            return Outcome::default();
        };
        let scale = frame.scale;
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
        Outcome::from_result(set_lane_points(doc, lane, points))
    }

    /// Lets go of the drag, if one is on a lane.
    pub fn release(&mut self, doc: &mut Document, p: Pointer) -> Option<Outcome> {
        let drag = self.drag.take()?;
        doc.seal();
        if let LaneDrag::Points {
            lane,
            lead,
            edited: false,
            ..
        } = drag
            && !p.shift
            && !p.ctrl
        {
            // A click (no drag) on one of several selected points selects
            // only it.
            self.select_point(doc.project(), lane, lead);
        }
        Some(Outcome::view())
    }

    /// Drops the drag, undoing what it did, if one is on a lane.
    pub fn cancel(&mut self, doc: &mut Document) -> Option<Outcome> {
        let mut drag = self.drag.take()?;
        Some(if *drag.edited() {
            // The drag is one merged undo step: undo it.
            doc.seal();
            let _ = doc.undo();
            Outcome::document()
        } else {
            Outcome::view()
        })
    }

    /// Ends the drag, keeping what it did, so it doesn't merge with the
    /// next edit.
    pub fn end_drag(&mut self, doc: &mut Document) {
        if self.drag.take().is_some() {
            doc.seal();
        }
    }

    // -----------------------------------------------------------------------
    // Commands on the selected points

    /// Deletes point `index` of `lane` (a double click on it).
    pub fn delete_point(&mut self, doc: &mut Document, lane: LaneId, index: usize) -> Outcome {
        self.select_point(doc.project(), lane, index);
        self.delete_points(doc)
    }

    /// Deletes the selected points.
    pub fn delete_points(&mut self, doc: &mut Document) -> Outcome {
        self.end_drag(doc);
        self.check(doc.project());
        let Some((lane, sel)) = self.selection.take() else {
            return Outcome::default();
        };
        let Some((_, l)) = lane_of(doc.project(), lane) else {
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
        commit_points(doc, lane, points)
    }

    /// Gives the segments after the selected points `shape`.
    pub fn set_point_shape(&mut self, doc: &mut Document, shape: PointShape) -> Outcome {
        self.end_drag(doc);
        self.check(doc.project());
        let Some((lane, sel)) = self.selection.clone() else {
            return Outcome::default();
        };
        let Some((_, l)) = lane_of(doc.project(), lane) else {
            return Outcome::view();
        };
        let mut points = l.points.clone();
        for i in sel {
            if let Some(p) = points.get_mut(i) {
                shape.apply(p);
            }
        }
        let o = commit_points(doc, lane, points);
        self.saw(doc.project());
        o
    }

    /// The shape of the segment after the first selected point, for the
    /// context menu to check.
    pub fn selected_shape(&self, project: &Project) -> Option<PointShape> {
        let (lane, sel) = self.selection(project)?;
        let p = lane_of(project, *lane)?.1.points.get(*sel.first()?)?;
        Some(PointShape::of(p))
    }

    // -----------------------------------------------------------------------
    // Drawing

    /// Draws `lane` in `frame`: its curve, or as a faint line the value
    /// `resting` the control has without it, and its points. `color` is
    /// the track's.
    pub fn draw(
        &self,
        s: &mut Scene,
        frame: &LaneFrame,
        pal: &Palette,
        lane: &AutomationLane,
        resting: f64,
        color: Rgba,
    ) {
        let r = frame.rect;
        let (left, right) = (r.x, r.x + r.w);
        let ink = if lane.enabled {
            pal.background.mix(color, 0.9)
        } else {
            pal.disabled_text
        };
        s.push(Prim::PushClip(r));
        let Some(first) = lane.points.first() else {
            s.hline(
                frame.value_y(resting),
                left,
                right,
                ink.with_alpha(0.5),
                1.0,
            );
            s.pop_clip();
            return;
        };
        let line = |s: &mut Scene, x0: f64, y0: f64, x1: f64, y1: f64| {
            s.push(Prim::Line {
                x0,
                y0,
                x1,
                y1,
                color: ink,
                width: 1.5,
            });
        };
        let pos = |p| frame.point_pos(p);
        let (x0, y0) = pos(first);
        if x0 > left {
            line(s, left, y0, x0.min(right), y0);
        }
        for pair in lane.points.windows(2) {
            let (p, q) = (&pair[0], &pair[1]);
            let ((xa, ya), (xb, yb)) = (pos(p), pos(q));
            if xb < left || xa > right {
                continue;
            }
            if p.step {
                line(s, xa, ya, xb, ya);
                line(s, xb, ya, xb, yb);
            } else if (p.curve == 0.0 && frame.scale.is_linear()) || (xb - xa) < 4.0 {
                line(s, xa, ya, xb, yb);
            } else {
                let floor = lane.target.ramp_floor();
                let at = |k: usize| {
                    let x = k as f64 / CURVE_SEGMENTS as f64;
                    // The value moves along the shape as it plays; heights
                    // follow the scale.
                    (
                        xa + (xb - xa) * x,
                        frame.value_y(p.towards(q.value, x, floor)),
                    )
                };
                for k in 0..CURVE_SEGMENTS {
                    let ((x1, y1), (x2, y2)) = (at(k), at(k + 1));
                    line(s, x1, y1, x2, y2);
                }
            }
        }
        let last = lane.points.last().unwrap_or(first);
        let (xl, yl) = pos(last);
        if xl < right {
            line(s, xl.max(left), yl, right, yl);
        }
        let half = POINT_PX / 2.0;
        for (i, p) in lane.points.iter().enumerate() {
            let (x, y) = pos(p);
            if x < left - half || x > right + half {
                continue;
            }
            let selected = self.is_selected(lane.id, i);
            s.push(Prim::RoundRect {
                r: Rect::new(x - half, y - half, 2.0 * half, 2.0 * half),
                radius: 1.0,
                fill: if selected {
                    pal.highlight
                } else {
                    pal.background
                },
                stroke: if selected { pal.highlight } else { ink },
                stroke_width: 1.0,
            });
        }
        s.pop_clip();
    }
}

/// Sets `lane`'s points, merging into the undo step in progress.
fn set_lane_points(
    doc: &mut Document,
    lane: LaneId,
    points: Vec<AutomationPoint>,
) -> Result<(), daw_model::ModelError> {
    doc.execute(SetLane {
        id: lane,
        field: LaneField::Points(points),
    })
    .map(|_| ())
}

/// Sets `lane`'s points as an undo step of its own.
fn commit_points(doc: &mut Document, lane: LaneId, points: Vec<AutomationPoint>) -> Outcome {
    doc.seal();
    let r = set_lane_points(doc, lane, points);
    doc.seal();
    Outcome::from_result(r)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use daw_model::{AutomationTarget, TrackKind};

    use super::*;
    use crate::snap;

    /// A lane on its own, as a view other than the arrange view would
    /// host it: 100 px tall at the top of the view, with snapping off.
    fn rig() -> (Document, LaneId, LaneFrame, LaneGrid) {
        let mut project = Project::new("Lanes");
        let mut track = Track::new(TrackKind::Instrument, "Keys");
        let lane = AutomationLane::new(AutomationTarget::Pan);
        let id = lane.id;
        track.automation.push(lane);
        project.tracks.push(Arc::new(track));
        let frame = LaneFrame {
            view: TimelineView::default(),
            rect: Rect::new(0.0, 0.0, 800.0, 100.0),
            scale: LaneScale::PAN,
        };
        let grid = LaneGrid {
            snap: Snap::Off,
            meter: snap::meter(&project.tempo),
        };
        (Document::new(project), id, frame, grid)
    }

    fn points(doc: &Document, lane: LaneId) -> Vec<AutomationPoint> {
        lane_of(doc.project(), lane).unwrap().1.points.clone()
    }

    #[test]
    fn a_click_adds_a_point_that_drags_and_undoes_in_one_step() {
        let (mut doc, lane, frame, grid) = rig();
        let mut e = LaneEditor::default();
        let (x, y) = (200.0, frame.value_y(0.0));
        assert!(
            e.press(&mut doc, &frame, &grid, Pointer::left(x, y), lane, None)
                .document
        );
        assert_eq!(e.dragging(), Some(lane));
        let added = points(&doc, lane);
        assert_eq!(added.len(), 1);
        assert!(added[0].value.abs() < 0.05, "{}", added[0].value);

        // Up to the top of the lane: hard right.
        let top = frame.value_y(1.0);
        assert!(
            e.drag(&mut doc, Some(&frame), &grid, Pointer::left(x, top))
                .document
        );
        assert_eq!(points(&doc, lane)[0].value, 1.0);
        assert!(e.release(&mut doc, Pointer::left(x, top)).is_some());
        assert!(e.points_selected(doc.project()));

        doc.undo().unwrap();
        assert!(points(&doc, lane).is_empty());
        e.check(doc.project());
        assert!(!e.points_selected(doc.project()));
    }

    #[test]
    fn a_drag_whose_lane_is_gone_ends() {
        let (mut doc, lane, frame, grid) = rig();
        let mut e = LaneEditor::default();
        let y = frame.value_y(0.0);
        e.press(&mut doc, &frame, &grid, Pointer::left(100.0, y), lane, None);
        assert_eq!(
            e.drag(&mut doc, None, &grid, Pointer::left(150.0, y)),
            Outcome::default()
        );
        assert_eq!(e.dragging(), None);
    }
}
