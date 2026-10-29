//! Piano-roll geometry, without any UI toolkit.
//!
//! The painted piano-roll item asks this module where things are and what a
//! pointer gesture means; it never does the arithmetic itself. Horizontal
//! zoom is pixels per beat (a quarter note), matching the arrange view.
//! Keys run from 127 at the top to 0 at the bottom.
//!
//! Note times are relative to the clip start, as in the model. The view
//! works in timeline ticks, and `clip_start` maps between the two.

use daw_core::time::{PPQ, TickTime};

use crate::edit::{self, MIN_LENGTH, Snap};
use crate::note::{EditNote, NoteData};

#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    /// A rectangle spanning two corners in any order.
    pub fn from_corners(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Self {
            x: x0.min(x1),
            y: y0.min(y1),
            w: (x1 - x0).abs(),
            h: (y1 - y0).abs(),
        }
    }

    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.x + o.w && o.x < self.x + self.w && self.y < o.y + o.h && o.y < self.y + self.h
    }
}

/// The visible part of the piano roll.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct PianoRollView {
    /// Timeline tick at the left edge.
    pub left: TickTime,
    /// Horizontal zoom.
    pub px_per_beat: f64,
    /// Vertical scroll: pixels from the top of key 127's row to the top
    /// of the view.
    pub scroll_y: f64,
    /// Height of one key row.
    pub key_height: f64,
    /// Where the edited clip starts on the timeline.
    pub clip_start: TickTime,
}

impl Default for PianoRollView {
    fn default() -> Self {
        Self {
            left: TickTime(0),
            px_per_beat: 80.0,
            scroll_y: 0.0,
            key_height: 14.0,
            clip_start: TickTime(0),
        }
    }
}

/// Which part of a note the pointer is over.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum NotePart {
    Body,
    /// Left edge: drags resize the start.
    Start,
    /// Right edge: drags resize the end.
    End,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub index: usize,
    pub part: NotePart,
}

impl PianoRollView {
    fn px_per_tick(&self) -> f64 {
        self.px_per_beat / PPQ as f64
    }

    /// X of a timeline tick.
    pub fn tick_to_x(&self, t: TickTime) -> f64 {
        (t.0 - self.left.0) as f64 * self.px_per_tick()
    }

    /// Timeline tick at `x`, rounded to the nearest tick.
    pub fn x_to_tick(&self, x: f64) -> TickTime {
        TickTime(self.left.0 + (x / self.px_per_tick()).round() as i64)
    }

    /// Clip-relative tick at `x`.
    pub fn x_to_clip_tick(&self, x: f64) -> TickTime {
        TickTime(self.x_to_tick(x).0 - self.clip_start.0)
    }

    /// Top of `key`'s row.
    pub fn key_to_y(&self, key: u8) -> f64 {
        (127 - key.min(127)) as f64 * self.key_height - self.scroll_y
    }

    /// The key whose row contains `y`, clamped to 0..=127.
    pub fn y_to_key(&self, y: f64) -> u8 {
        let row = ((y + self.scroll_y) / self.key_height).floor();
        (127.0 - row).clamp(0.0, 127.0) as u8
    }

    /// Where a note is drawn.
    pub fn note_rect(&self, n: &NoteData) -> Rect {
        let x = self.tick_to_x(TickTime(self.clip_start.0 + n.start.0));
        let w = n.length.0 as f64 * self.px_per_tick();
        Rect {
            x,
            y: self.key_to_y(n.key),
            w,
            h: self.key_height,
        }
    }

    /// The visible timeline ticks for a view `width` pixels wide.
    pub fn visible_ticks(&self, width: f64) -> std::ops::Range<TickTime> {
        self.left..TickTime(self.x_to_tick(width).0 + 1)
    }

    /// The visible keys, lowest first, for a view `height` pixels tall.
    pub fn visible_keys(&self, height: f64) -> std::ops::RangeInclusive<u8> {
        self.y_to_key(height)..=self.y_to_key(0.0)
    }

    /// The note under the pointer, and which part of it. Later notes are
    /// drawn on top, so they win. Edge zones are `edge_px` wide, but never
    /// more than a third of a short note.
    pub fn hit_test<N: EditNote>(&self, notes: &[N], x: f64, y: f64, edge_px: f64) -> Option<Hit> {
        notes.iter().enumerate().rev().find_map(|(index, n)| {
            let r = self.note_rect(&n.data());
            if !r.contains(x, y) {
                return None;
            }
            let edge = edge_px.min(r.w / 3.0);
            let part = if x < r.x + edge {
                NotePart::Start
            } else if x >= r.x + r.w - edge {
                NotePart::End
            } else {
                NotePart::Body
            };
            Some(Hit { index, part })
        })
    }

    /// Indices of the notes a rubber band touches.
    pub fn notes_in_rect<N: EditNote>(&self, notes: &[N], band: &Rect) -> Vec<usize> {
        notes
            .iter()
            .enumerate()
            .filter(|(_, n)| self.note_rect(&n.data()).intersects(band))
            .map(|(i, _)| i)
            .collect()
    }

    /// The note a click with the draw tool makes: on the clicked key,
    /// starting at the grid line at or before the pointer, `length` long
    /// (one grid step when `None`).
    pub fn new_note(
        &self,
        x: f64,
        y: f64,
        snap: Option<&Snap>,
        length: Option<i64>,
        velocity: f64,
    ) -> NoteData {
        let t = self.x_to_clip_tick(x);
        let (start, step) = match snap {
            Some(s) => (s.floor(t), s.step_at(t)),
            None => (t, PPQ / 4),
        };
        NoteData {
            start: TickTime(start.0.max(0)),
            length: TickTime(length.unwrap_or(step).max(MIN_LENGTH)),
            key: self.y_to_key(y),
            channel: 0,
            velocity: edit::clamp_velocity(velocity),
        }
    }
}

/// What a drag does to the selection.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DragKind {
    Move,
    ResizeStart,
    ResizeEnd,
}

impl From<NotePart> for DragKind {
    fn from(p: NotePart) -> Self {
        match p {
            NotePart::Body => DragKind::Move,
            NotePart::Start => DragKind::ResizeStart,
            NotePart::End => DragKind::ResizeEnd,
        }
    }
}

/// A drag of the selected notes. Snapping follows the grabbed note: a move
/// puts its start on the grid, a resize puts the dragged edge on the grid,
/// and the rest of the selection moves by the same amount.
#[derive(Clone, Debug)]
pub struct Drag {
    pub kind: DragKind,
    anchor_tick: TickTime,
    anchor_key: u8,
    grabbed: NoteData,
    originals: Vec<NoteData>,
}

impl Drag {
    /// Starts a drag at the pointer. `grabbed` is the note under the
    /// pointer; `selection` is every note that moves, grabbed one included.
    pub fn begin<N: EditNote>(
        kind: DragKind,
        view: &PianoRollView,
        x: f64,
        y: f64,
        grabbed: &N,
        selection: &[N],
    ) -> Self {
        Self {
            kind,
            anchor_tick: view.x_to_tick(x),
            anchor_key: view.y_to_key(y),
            grabbed: grabbed.data(),
            originals: selection.iter().map(|n| n.data()).collect(),
        }
    }

    /// The selection's notes, in `selection` order, for the pointer at
    /// `x`, `y`. Pass `None` to drag without snapping.
    pub fn update(
        &self,
        view: &PianoRollView,
        x: f64,
        y: f64,
        snap: Option<&Snap>,
    ) -> Vec<NoteData> {
        // The anchor is a tick, so scrolling or zooming mid-drag keeps the
        // note under the pointer.
        let raw = view.x_to_tick(x).0 - self.anchor_tick.0;
        let g = self.grabbed;
        let mut notes = self.originals.clone();
        match self.kind {
            DragKind::Move => {
                let dt = match snap {
                    Some(s) => s.snap(TickTime(g.start.0 + raw)).0 - g.start.0,
                    None => raw,
                };
                let dkey = view.y_to_key(y) as i32 - self.anchor_key as i32;
                edit::shift(&mut notes, dt, dkey);
            }
            DragKind::ResizeEnd => {
                let end = g.end().0 + raw;
                let end = match snap {
                    Some(s) => {
                        let e = s.snap(TickTime(end)).0;
                        if e <= g.start.0 { s.next(g.start).0 } else { e }
                    }
                    None => end,
                };
                edit::resize_end(&mut notes, end - g.end().0);
            }
            DragKind::ResizeStart => {
                let start = g.start.0 + raw;
                let start = match snap {
                    Some(s) => {
                        let st = s.snap(TickTime(start)).0;
                        if st >= g.end().0 {
                            s.floor(TickTime(g.end().0 - 1)).0
                        } else {
                            st
                        }
                    }
                    None => start,
                };
                edit::resize_start(&mut notes, start - g.start.0);
            }
        }
        notes
    }
}

/// The velocity lane under the piano roll: velocity 1 at the top, 0 at
/// the bottom, one stem per note at its start.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct VelocityLane {
    pub height: f64,
}

impl VelocityLane {
    pub fn velocity_to_y(&self, v: f64) -> f64 {
        (1.0 - v.clamp(0.0, 1.0)) * self.height
    }

    pub fn y_to_velocity(&self, y: f64) -> f64 {
        if self.height <= 0.0 {
            return 0.0;
        }
        (1.0 - y / self.height).clamp(0.0, 1.0)
    }

    /// The stem nearest to `x`, within `tolerance_px`. Among stems at the
    /// same x (a chord), the last drawn wins.
    pub fn hit_test<N: EditNote>(
        &self,
        view: &PianoRollView,
        notes: &[N],
        x: f64,
        tolerance_px: f64,
    ) -> Option<usize> {
        let mut best: Option<(usize, f64)> = None;
        for (i, n) in notes.iter().enumerate() {
            let sx = view.note_rect(&n.data()).x;
            let d = (sx - x).abs();
            if d <= tolerance_px && best.is_none_or(|(_, bd)| d <= bd) {
                best = Some((i, d));
            }
        }
        best.map(|(i, _)| i)
    }

    /// Draws a straight line of velocities from (`x0`, `y0`) to (`x1`,
    /// `y1`): every note whose stem lies between the two x positions gets
    /// the velocity of the line at its stem. Returns the indices changed.
    pub fn draw_line<N: EditNote>(
        &self,
        view: &PianoRollView,
        notes: &mut [N],
        (x0, y0): (f64, f64),
        (x1, y1): (f64, f64),
    ) -> Vec<usize> {
        let (lo, hi) = if x0 <= x1 { (x0, x1) } else { (x1, x0) };
        let mut changed = Vec::new();
        for (i, n) in notes.iter_mut().enumerate() {
            let mut d = n.data();
            let sx = view.note_rect(&d).x;
            if sx < lo || sx > hi {
                continue;
            }
            let y = if hi > lo {
                y0 + (y1 - y0) * (sx - x0) / (x1 - x0)
            } else {
                y1
            };
            d.velocity = self.y_to_velocity(y);
            n.set_data(d);
            changed.push(i);
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::{Grid, Meter};

    const Q: i64 = PPQ;

    fn view() -> PianoRollView {
        PianoRollView {
            left: TickTime(0),
            px_per_beat: 96.0,
            scroll_y: 0.0,
            key_height: 10.0,
            clip_start: TickTime(Q),
        }
    }

    #[test]
    fn coordinates_round_trip() {
        let v = view();
        assert_eq!(v.tick_to_x(TickTime(Q)), 96.0);
        assert_eq!(v.x_to_tick(48.0), TickTime(Q / 2));
        assert_eq!(v.x_to_clip_tick(96.0), TickTime(0));
        assert_eq!(v.key_to_y(127), 0.0);
        assert_eq!(v.key_to_y(60), 670.0);
        assert_eq!(v.y_to_key(675.0), 60);
        assert_eq!(v.y_to_key(-50.0), 127);
        assert_eq!(v.y_to_key(1e9), 0);
        for key in 0..=127u8 {
            assert_eq!(v.y_to_key(v.key_to_y(key) + 1.0), key);
        }
        let r = v.note_rect(&NoteData::new(Q, Q / 2, 60, 1.0));
        assert_eq!(
            r,
            Rect {
                x: 192.0,
                y: 670.0,
                w: 48.0,
                h: 10.0
            }
        );
        assert_eq!(v.visible_keys(100.0), 117..=127);
    }

    #[test]
    fn hit_test_parts_and_order() {
        let v = view();
        // Clip-relative: both notes start at the clip start, x = 96.
        let notes = [
            NoteData::new(0, Q, 60, 1.0),
            NoteData::new(0, Q / 4, 60, 1.0),
        ];
        // The short note (drawn last) is on top.
        assert_eq!(
            v.hit_test(&notes, 108.0, 675.0, 5.0),
            Some(Hit {
                index: 1,
                part: NotePart::Body
            })
        );
        assert_eq!(
            v.hit_test(&notes, 97.0, 675.0, 5.0).unwrap().part,
            NotePart::Start
        );
        assert_eq!(
            v.hit_test(&notes, 190.0, 675.0, 5.0),
            Some(Hit {
                index: 0,
                part: NotePart::End
            })
        );
        assert_eq!(
            v.hit_test(&notes, 150.0, 675.0, 5.0),
            Some(Hit {
                index: 0,
                part: NotePart::Body
            })
        );
        assert_eq!(v.hit_test(&notes, 150.0, 665.0, 5.0), None);
        let band = Rect::from_corners(200.0, 600.0, 150.0, 700.0);
        assert_eq!(v.notes_in_rect(&notes, &band), [0]);
    }

    #[test]
    fn drag_move_snaps_grabbed_note() {
        let v = view();
        let grid = Grid::note(16);
        let meter = Meter::default();
        let snap = Snap::new(&grid, &meter, v.clip_start);
        let sel = [
            NoteData::new(10, 100, 60, 1.0),
            NoteData::new(300, 100, 64, 1.0),
        ];
        let d = Drag::begin(DragKind::Move, &v, 100.0, 675.0, &sel[0], &sel);
        // About 130 ticks right and two rows up.
        let out = d.update(&v, 113.0, 655.0, Some(&snap));
        assert_eq!(out[0].start.0, 240);
        assert_eq!(out[1].start.0, 530);
        assert_eq!((out[0].key, out[1].key), (62, 66));
        let free = d.update(&v, 113.0, 675.0, None);
        assert_eq!(free[0].start.0, 140);
        // Dragging far left clamps the group at 0.
        let out = d.update(&v, -500.0, 675.0, Some(&snap));
        assert_eq!(out[0].start.0, 0);
        assert_eq!(out[1].start.0, 290);
    }

    #[test]
    fn drag_resize_snaps_edges() {
        let v = view();
        let grid = Grid::note(16);
        let meter = Meter::default();
        let snap = Snap::new(&grid, &meter, TickTime(0));
        let sel = [
            NoteData::new(0, 240, 60, 1.0),
            NoteData::new(240, 240, 62, 1.0),
        ];
        let d = Drag::begin(DragKind::ResizeEnd, &v, 120.0, 675.0, &sel[0], &sel);
        let out = d.update(&v, 140.0, 675.0, Some(&snap));
        assert_eq!((out[0].length.0, out[1].length.0), (480, 480));
        // Dragging the end before the start keeps one grid step.
        let out = d.update(&v, -1000.0, 675.0, Some(&snap));
        assert_eq!(out[0].length.0, 240);
        assert_eq!(out[1].length.0, 240);

        let d = Drag::begin(DragKind::ResizeStart, &v, 96.0, 675.0, &sel[1], &sel);
        // 200 ticks earlier snaps the start to 0; the end stays.
        let out = d.update(&v, 76.0, 675.0, Some(&snap));
        assert_eq!((out[1].start.0, out[1].end().0), (0, 480));
        assert_eq!((out[0].start.0, out[0].end().0), (0, 240));
        // Past the end keeps the start one step before it.
        let out = d.update(&v, 500.0, 675.0, Some(&snap));
        assert_eq!((out[1].start.0, out[1].end().0), (240, 480));
    }

    #[test]
    fn draw_tool_note() {
        let v = view();
        let grid = Grid::note(8);
        let meter = Meter::default();
        let snap = Snap::new(&grid, &meter, v.clip_start);
        let n = v.new_note(96.0 + 60.0, 675.0, Some(&snap), None, 0.7);
        assert_eq!((n.start.0, n.length.0, n.key), (Q / 2, Q / 2, 60));
        // Left of the clip start clamps to 0.
        assert_eq!(
            v.new_note(10.0, 675.0, Some(&snap), Some(10), 0.7).start.0,
            0
        );
    }

    #[test]
    fn velocity_lane() {
        let v = view();
        let lane = VelocityLane { height: 100.0 };
        assert_eq!(lane.velocity_to_y(1.0), 0.0);
        assert_eq!(lane.y_to_velocity(75.0), 0.25);
        let mut notes = [
            NoteData::new(0, 10, 60, 0.5),
            NoteData::new(Q, 10, 60, 0.5),
            NoteData::new(2 * Q, 10, 60, 0.5),
        ];
        assert_eq!(lane.hit_test(&v, &notes, 190.0, 4.0), Some(1));
        assert_eq!(lane.hit_test(&v, &notes, 150.0, 4.0), None);
        let changed = lane.draw_line(&v, &mut notes, (96.0, 100.0), (288.0, 0.0));
        assert_eq!(changed, [0, 1, 2]);
        let vel: Vec<f64> = notes.iter().map(|n| n.velocity).collect();
        assert_eq!(vel, [0.0, 0.5, 1.0]);
    }

    #[test]
    fn drag_follows_content_when_the_view_scrolls() {
        let mut v = view();
        let sel = [NoteData::new(0, 100, 60, 1.0)];
        let d = Drag::begin(DragKind::Move, &v, 100.0, 675.0, &sel[0], &sel);
        // Autoscroll one beat to the right while the pointer stays put.
        v.left = TickTime(Q);
        assert_eq!(d.update(&v, 100.0, 675.0, None)[0].start.0, Q);
        // Zoom in 2x around the left edge: the same x is now half as far.
        v.left = TickTime(0);
        v.px_per_beat *= 2.0;
        assert_eq!(d.update(&v, 200.0, 675.0, None)[0].start.0, 0);
    }
}
