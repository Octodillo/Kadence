//! Automation lanes under the piano roll.
//!
//! The open clip's track's lanes show in rows under the velocity lane, one
//! row per lane in the track's order, while [`PianoRoll::lanes_shown`] is
//! on. They are edited the same way as under the track in the arrange
//! view, by the shared [`LaneEditor`](daw_arrange::LaneEditor): the piano
//! roll only says where each lane is and passes the pointer on.
//!
//! How many rows there are is read from the project by
//! [`sync`](PianoRoll::sync), so the layout needs no project. The rows
//! never take more than [`MAX_LANES_SHARE`] of the space under the ruler;
//! many lanes share it in thinner rows, so every lane stays in reach.

use daw_arrange::automation::{lane_scale, static_value};
use daw_arrange::lane_editor::lane_of;
use daw_arrange::{
    Button, Cursor, LaneFrame, LaneGrid, Outcome, PointShape, Pointer, Prim, Rect, Scene,
    track_color,
};
use daw_model::{Document, LaneId, Project, Track};

use crate::{PianoRoll, outcome_view};

/// The most of the height under the ruler the lane rows take.
pub const MAX_LANES_SHARE: f64 = 0.5;

impl PianoRoll {
    // -----------------------------------------------------------------------
    // Geometry

    /// Lanes on the open clip's track, as of the last
    /// [`sync`](Self::sync).
    pub fn lane_count(&self) -> usize {
        self.lane_count
    }

    /// The height of one lane row: `lane_height`, or less when the rows
    /// would take more than their share of the item.
    pub fn lane_row_height(&self) -> f64 {
        let room = ((self.height - self.ruler_height) * MAX_LANES_SHARE).max(0.0);
        self.lane_height.min(room / self.lane_count.max(1) as f64)
    }

    /// The height the lane rows take: none while hidden.
    pub fn lanes_height(&self) -> f64 {
        if self.lanes_shown {
            self.lane_count as f64 * self.lane_row_height()
        } else {
            0.0
        }
    }

    /// The lane rows, under the velocity lane.
    pub fn lanes_rect(&self) -> Rect {
        let vel = self.velocity_rect();
        Rect::new(
            vel.x,
            vel.bottom(),
            vel.w,
            self.lanes_height().min(self.height - vel.bottom()).max(0.0),
        )
    }

    /// The row of the open clip's track's lane `index`.
    pub fn lane_rect(&self, index: usize) -> Rect {
        let r = self.lanes_rect();
        Rect::new(
            r.x,
            r.y + index as f64 * self.lane_row_height(),
            r.w,
            self.lane_row_height(),
        )
    }

    /// The track whose lanes show: the open clip's.
    pub(crate) fn lane_track<'a>(&self, project: &'a Project) -> Option<&'a Track> {
        project.track(self.track(project)?).map(|t| &**t)
    }

    /// The open clip's track's row in the project, which names it to the
    /// app's lane getters and picks its colour.
    pub fn track_row(&self, project: &Project) -> Option<usize> {
        let track = self.track(project)?;
        project.tracks.iter().position(|t| t.id == track)
    }

    fn frame_at(&self, track: &Track, index: usize) -> Option<LaneFrame> {
        let lane = track.automation.get(index)?;
        Some(LaneFrame {
            view: self.view,
            rect: self.lane_rect(index),
            scale: lane_scale(self.params.as_ref(), track, lane.target),
        })
    }

    /// The frame `lane` is drawn in, if it shows.
    pub fn lane_frame(&self, project: &Project, lane: LaneId) -> Option<LaneFrame> {
        if !self.lanes_shown {
            return None;
        }
        let track = self.lane_track(project)?;
        let (t, _) = lane_of(project, lane)?;
        if t.id != track.id {
            return None;
        }
        let index = track.automation.iter().position(|l| l.id == lane)?;
        self.frame_at(track, index)
    }

    fn lane_grid(&self, project: &Project) -> LaneGrid {
        LaneGrid {
            snap: self.snap,
            meter: self.meter(project),
        }
    }

    /// The lane under (`x`, `y`) in the lane rows, the frame it is drawn
    /// in, and the point of it there, if any.
    fn lane_at(
        &self,
        project: &Project,
        x: f64,
        y: f64,
    ) -> Option<(LaneId, LaneFrame, Option<usize>)> {
        let track = self.lane_track(project)?;
        let r = self.lanes_rect();
        if !r.contains(x, y) {
            return None;
        }
        let index = ((y - r.y) / self.lane_row_height()).floor() as usize;
        let lane = track.automation.get(index)?;
        let frame = self.frame_at(track, index)?;
        let point = frame.point_at(lane, x, y);
        Some((lane.id, frame, point))
    }

    /// Drops a point selection on a lane that no longer shows (hidden, or
    /// not on the open clip's track), so Delete can't remove points no one
    /// sees.
    pub(crate) fn forget_hidden_points(&mut self, project: &Project) {
        if let Some(lane) = self.lanes.selected_lane()
            && self.lane_frame(project, lane).is_none()
        {
            self.lanes.clear_selection();
        }
    }

    // -----------------------------------------------------------------------
    // Pointer input

    /// The pointer shape over the lane rows.
    pub(crate) fn lane_cursor(&self, project: &Project, x: f64, y: f64) -> Cursor {
        match self.lane_at(project, x, y) {
            Some((_, _, Some(_))) => Cursor::Move,
            Some((_, _, None)) => Cursor::Crosshair,
            None => Cursor::Arrow,
        }
    }

    /// A press on the lane rows. It leaves the notes, as a press on a lane
    /// in the arrange view leaves the clips, so Delete and the menu act on
    /// points. A right press selects the point under it and asks for the
    /// point menu.
    pub(crate) fn press_lane(&mut self, doc: &mut Document, p: Pointer) -> Outcome {
        if p.button != Button::Middle {
            self.selection.clear();
        }
        let project = doc.project();
        let Some((lane, frame, point)) = self.lane_at(project, p.x, p.y) else {
            self.lanes.clear_selection();
            return outcome_view();
        };
        let grid = self.lane_grid(project);
        if p.button == Button::Right {
            self.lane_menu_requested = Some(point.is_some());
        }
        self.lanes.press(doc, &frame, &grid, p, lane, point)
    }

    /// Moves a lane drag, if one is in progress.
    pub(crate) fn drag_lane(&mut self, doc: &mut Document, p: Pointer) -> Option<Outcome> {
        let lane = self.lanes.dragging()?;
        let project = doc.project();
        let frame = self.lane_frame(project, lane);
        let grid = self.lane_grid(project);
        Some(self.lanes.drag(doc, frame.as_ref(), &grid, p))
    }

    /// A double click on the lane rows deletes the point under it.
    pub(crate) fn double_click_lane(&mut self, doc: &mut Document, p: Pointer) -> Outcome {
        self.lanes.end_drag(doc);
        match self.lane_at(doc.project(), p.x, p.y) {
            Some((lane, _, Some(index))) => self.lanes.delete_point(doc, lane, index),
            _ => Outcome::default(),
        }
    }

    /// Whether the last press asked for the point menu, and whether it was
    /// on a point (the menu's shapes apply) rather than between points.
    /// Asking clears the request.
    pub fn take_lane_menu_request(&mut self) -> Option<bool> {
        self.lane_menu_requested.take()
    }

    // -----------------------------------------------------------------------
    // Commands on points

    /// Gives the segments after the selected points `shape`.
    pub fn set_point_shape(&mut self, doc: &mut Document, shape: PointShape) -> Outcome {
        self.lanes.set_point_shape(doc, shape)
    }

    /// The selected points' shape, if they share one.
    pub fn point_shape(&self, project: &Project) -> Option<PointShape> {
        self.lanes.selected_shape(project)
    }

    /// Deletes the selected points.
    pub fn delete_points(&mut self, doc: &mut Document) -> Outcome {
        self.lanes.delete_points(doc)
    }

    // -----------------------------------------------------------------------
    // Drawing

    /// Draws the lane rows: a shade off the background like the arrange
    /// view's, a line between rows, and each lane in its track's colour.
    pub(crate) fn draw_lanes(&self, s: &mut Scene, project: &Project) {
        let r = self.lanes_rect();
        let Some(track) = self.lane_track(project).filter(|_| r.h > 0.0) else {
            return;
        };

        let pal = self.palette;
        let line = pal.background.mix(pal.text, 0.15);
        let Some(row) = self.track_row(project) else {
            return;
        };
        let color = track_color(track, row);
        s.rect(r, pal.background.mix(pal.text, 0.04));
        s.push(Prim::PushClip(r));
        self.draw_grid_lines(s, &self.meter(project), r.y, r.bottom());
        for (i, lane) in track.automation.iter().enumerate() {
            let Some(frame) = self.frame_at(track, i) else {
                continue;
            };
            if frame.rect.y > r.bottom() {
                break;
            }
            s.hline(frame.rect.y + 0.5, r.x, r.right(), line, 1.0);
            let resting = static_value(self.params.as_ref(), track, lane.target);
            self.lanes.draw(s, &frame, &pal, lane, resting, color);
        }
        s.pop_clip();
    }
}
