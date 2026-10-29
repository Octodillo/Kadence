//! The arrange view, without a toolkit.
//!
//! [`Arrange`] holds the view state (zoom, scroll, selection, the drag in
//! progress) and turns pointer and keyboard input into `daw-model` commands
//! on a [`Document`]. [`Arrange::scene`] produces a [`Scene`]: a list of
//! rectangles, lines, text and waveforms in view pixels that a front end
//! paints as it likes. The Plasma front end paints it with QPainter; a GNOME
//! one could paint it with Cairo or GSK.
//!
//! Coordinates are logical pixels inside the timeline area: `x` from its
//! left edge, `y` from its top, with the ruler across the top. Track headers
//! are not part of it; they are ordinary widgets beside it, `track_height`
//! tall each and scrolled by [`Arrange::scroll_y`].

pub mod commands;
mod draw;
pub mod peaks;
pub mod scene;
pub mod snap;
pub mod view;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use daw_core::time::{PPQ, SampleTime, SegmentTempoMap, TempoMap, TickTime};
use daw_midi::grid::Meter;
use daw_model::commands::{AddClip, AddTrack, ClipField, MoveClip, RemoveClip, SetClip};
use daw_model::{
    AudioClip, Clip, ClipContent, ClipId, Command, Document, MidiClip, ModelError, Project, Track,
    TrackId, TrackKind,
};

use commands::{Group, TrimClip};
pub use draw::track_color;
pub use peaks::{PeakPool, Peaks};
pub use scene::{Palette, Prim, Rect, Rgba, Scene};
pub use snap::Snap;
pub use view::TimelineView;

/// Smallest and largest track height in logical pixels.
pub const MIN_TRACK_HEIGHT: f64 = 24.0;
pub const MAX_TRACK_HEIGHT: f64 = 400.0;

/// A pointer must move this far before a press becomes a drag.
const DRAG_THRESHOLD: f64 = 3.0;

/// The shortest clip a trim leaves, in ticks (a 64th note).
const MIN_CLIP_TICKS: i64 = PPQ / 16;

/// Everything about the project the view needs besides the document.
pub struct Env<'a> {
    pub tempo: &'a SegmentTempoMap,
    pub sample_rate: f64,
    /// The project bundle, which relative audio paths are inside.
    pub root: Option<&'a Path>,
    pub playhead: TickTime,
    pub loop_range: Option<(TickTime, TickTime)>,
}

impl Env<'_> {
    pub fn resolve(&self, source: &Path) -> PathBuf {
        match self.root {
            Some(r) if source.is_relative() => r.join(source),
            _ => source.to_path_buf(),
        }
    }

    fn ticks_to_samples(&self, t: TickTime) -> SampleTime {
        self.tempo.ticks_to_samples(t, self.sample_rate)
    }

    fn samples_to_ticks(&self, s: SampleTime) -> TickTime {
        self.tempo.samples_to_ticks(s, self.sample_rate)
    }
}

/// Which part of a clip the pointer is over.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Zone {
    Body,
    /// The left edge: dragging it trims the start.
    Start,
    /// The right edge: dragging it trims the end.
    End,
}

/// What is under a point.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Hit {
    Ruler(TickTime),
    Clip {
        track: TrackId,
        row: usize,
        clip: ClipId,
        zone: Zone,
    },
    Lane {
        track: TrackId,
        row: usize,
        tick: TickTime,
    },
    /// Below the last track.
    Empty(TickTime),
}

/// The pointer cursor a front end should show.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Cursor {
    Arrow,
    /// Over a clip body: it can be moved.
    Move,
    /// Over a clip edge: it can be resized horizontally.
    ResizeHorizontal,
    /// Over the ruler: click to move the playhead.
    Pointing,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
}

/// One pointer event in view coordinates.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Pointer {
    pub x: f64,
    pub y: f64,
    pub button: Button,
    pub shift: bool,
    /// Ctrl (Command on a Mac keyboard).
    pub ctrl: bool,
    pub alt: bool,
}

impl Pointer {
    pub fn left(x: f64, y: f64) -> Self {
        Self {
            x,
            y,
            button: Button::Left,
            shift: false,
            ctrl: false,
            alt: false,
        }
    }
}

/// What an input event did, so the front end knows what to update.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outcome {
    /// The document changed: take a new snapshot for the engine and redraw.
    pub document: bool,
    /// Only the view (zoom, scroll, selection, rubber band) changed: redraw.
    pub view: bool,
    /// Move the playhead here.
    pub locate: Option<TickTime>,
    /// Set (or with `Some(None)`, clear) the loop range.
    pub set_loop: Option<Option<(TickTime, TickTime)>>,
    /// The user asked to edit this clip (double click).
    pub open_clip: Option<ClipId>,
    /// The user asked for a context menu here.
    pub context_menu: Option<Hit>,
    /// A command was refused; the message is for the status bar.
    pub error: Option<String>,
}

impl Outcome {
    fn view() -> Self {
        Self {
            view: true,
            ..Default::default()
        }
    }

    fn document() -> Self {
        Self {
            document: true,
            view: true,
            ..Default::default()
        }
    }

    fn from_result(r: Result<(), ModelError>) -> Self {
        match r {
            Ok(()) => Self::document(),
            Err(e) => Self {
                view: true,
                error: Some(e.to_string()),
                ..Default::default()
            },
        }
    }
}

#[derive(Clone, Debug)]
enum Drag {
    /// Pressed but not moved past the threshold yet.
    Pending { start: Pointer, hit: Hit },
    Move {
        /// Tick and row under the pointer at the press.
        tick: TickTime,
        row: usize,
        /// The clip that was grabbed; its start is what snaps.
        lead: ClipId,
        /// Each selected clip's original row and start.
        clips: Vec<(ClipId, usize, TickTime)>,
    },
    Trim {
        clip: ClipId,
        zone: Zone,
        tick: TickTime,
        start: TickTime,
        length: TickTime,
        offset: Option<SampleTime>,
    },
    Rubber {
        x0: f64,
        y0: f64,
        x1: f64,
        y1: f64,
        base: BTreeSet<ClipId>,
    },
    Scrub,
    Loop {
        from: TickTime,
    },
}

/// The arrange view's state. See the crate docs.
pub struct Arrange {
    pub view: TimelineView,
    /// Size of the timeline area, ruler included.
    pub width: f64,
    pub height: f64,
    pub track_height: f64,
    pub ruler_height: f64,
    pub scroll_y: f64,
    /// How close to a clip edge counts as grabbing the edge.
    pub edge_px: f64,
    pub snap: Snap,
    pub palette: Palette,
    pub selection: BTreeSet<ClipId>,
    pub selected_track: Option<TrackId>,
    pub peaks: PeakPool,
    drag: Option<Drag>,
}

impl Default for Arrange {
    fn default() -> Self {
        Self::new()
    }
}

impl Arrange {
    pub fn new() -> Self {
        Self {
            view: TimelineView::default(),
            width: 800.0,
            height: 600.0,
            track_height: 64.0,
            ruler_height: 24.0,
            scroll_y: 0.0,
            edge_px: 6.0,
            snap: Snap::default(),
            palette: Palette::default(),
            selection: BTreeSet::new(),
            selected_track: None,
            peaks: PeakPool::new(),
            drag: None,
        }
    }

    // -----------------------------------------------------------------------
    // Geometry

    /// Top of track row `row` in view coordinates.
    pub fn row_y(&self, row: usize) -> f64 {
        self.ruler_height + row as f64 * self.track_height - self.scroll_y
    }

    /// The track row under `y`, if `y` is over a track.
    pub fn row_at(&self, y: f64, rows: usize) -> Option<usize> {
        if y < self.ruler_height {
            return None;
        }
        let r = ((y - self.ruler_height + self.scroll_y) / self.track_height).floor();
        (r >= 0.0 && (r as usize) < rows).then_some(r as usize)
    }

    /// Height of all tracks together.
    pub fn content_height(&self, project: &Project) -> f64 {
        project.tracks.len() as f64 * self.track_height
    }

    /// Width the timeline content needs: past the last clip, with room to
    /// add more, and never less than the view.
    pub fn content_width(&self, project: &Project) -> f64 {
        let end = project
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .map(|c| c.start.0 + c.length.0)
            .max()
            .unwrap_or(0);
        let x = end as f64 * self.view.px_per_tick() + self.width;
        x.max(self.width + self.view.scroll_x)
    }

    pub fn clip_rect(&self, row: usize, clip: &Clip) -> Rect {
        let x0 = self.view.tick_to_x(clip.start);
        let x1 = self.view.tick_to_x(TickTime(clip.start.0 + clip.length.0));
        Rect::new(x0, self.row_y(row) + 1.0, (x1 - x0).max(1.0), self.track_height - 2.0)
    }

    /// What is under `(x, y)`.
    pub fn hit(&self, project: &Project, x: f64, y: f64) -> Hit {
        let tick = self.view.x_to_tick(x).max(TickTime(0));
        if y < self.ruler_height {
            return Hit::Ruler(tick);
        }
        let Some(row) = self.row_at(y, project.tracks.len()) else {
            return Hit::Empty(tick);
        };
        let track = &project.tracks[row];
        // Later clips paint over earlier ones, so look from the end.
        for clip in track.clips.iter().rev() {
            let r = self.clip_rect(row, clip);
            if !r.contains(x, y) {
                continue;
            }
            let edge = self.edge_px.min(r.w / 4.0);
            let zone = if x < r.x + edge {
                Zone::Start
            } else if x >= r.right() - edge {
                Zone::End
            } else {
                Zone::Body
            };
            return Hit::Clip {
                track: track.id,
                row,
                clip: clip.id,
                zone,
            };
        }
        Hit::Lane {
            track: track.id,
            row,
            tick,
        }
    }

    pub fn cursor(&self, project: &Project, x: f64, y: f64) -> Cursor {
        match &self.drag {
            Some(Drag::Move { .. }) => return Cursor::Move,
            Some(Drag::Trim { .. }) => return Cursor::ResizeHorizontal,
            _ => {}
        }
        match self.hit(project, x, y) {
            Hit::Ruler(_) => Cursor::Pointing,
            Hit::Clip { zone: Zone::Body, .. } => Cursor::Move,
            Hit::Clip { .. } => Cursor::ResizeHorizontal,
            _ => Cursor::Arrow,
        }
    }

    /// Whether a drag is in progress (the front end should grab the pointer).
    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    // -----------------------------------------------------------------------
    // View changes

    /// Resizes the view, keeping the vertical scroll in range.
    pub fn resize(&mut self, project: &Project, width: f64, height: f64) {
        self.width = width.max(1.0);
        self.height = height.max(1.0);
        self.clamp_scroll(project);
    }

    fn clamp_scroll(&mut self, project: &Project) {
        let visible = self.height - self.ruler_height;
        let max = (self.content_height(project) - visible).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, max);
        self.view.scroll_x = self.view.scroll_x.max(0.0);
    }

    /// A scroll wheel or touchpad: `dx`/`dy` in pixels (positive scrolls
    /// content left/up, as `QWheelEvent::pixelDelta` reports a swipe). Ctrl
    /// zooms time around the pointer, Ctrl+Shift zooms track height, Shift
    /// turns vertical wheel motion into horizontal scrolling.
    pub fn wheel(&mut self, project: &Project, p: Pointer, dx: f64, dy: f64) -> Outcome {
        if p.ctrl && p.shift {
            self.zoom_tracks(project, (dy / 240.0).exp2(), p.y);
        } else if p.ctrl {
            self.view.zoom_at((dy / 240.0).exp2(), p.x);
        } else if p.shift {
            self.view.scroll_by(-(dx + dy));
        } else {
            self.view.scroll_by(-dx);
            self.scroll_y -= dy;
        }
        self.clamp_scroll(project);
        Outcome::view()
    }

    /// A pinch gesture: `scale` relative to the previous update, centred on
    /// `x`.
    pub fn pinch(&mut self, project: &Project, scale: f64, x: f64) -> Outcome {
        self.view.zoom_at(scale, x);
        self.clamp_scroll(project);
        Outcome::view()
    }

    pub fn zoom_tracks(&mut self, project: &Project, factor: f64, anchor_y: f64) {
        let content_y = anchor_y - self.ruler_height + self.scroll_y;
        let rows = content_y / self.track_height;
        self.track_height = (self.track_height * factor).clamp(MIN_TRACK_HEIGHT, MAX_TRACK_HEIGHT);
        self.scroll_y = rows * self.track_height - (anchor_y - self.ruler_height);
        self.clamp_scroll(project);
    }

    /// Zooms so the whole project fits the width.
    pub fn zoom_to_fit(&mut self, project: &Project) -> Outcome {
        let end = project
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .map(|c| c.start.0 + c.length.0)
            .max()
            .unwrap_or(16 * PPQ)
            .max(PPQ);
        let ppb = (self.width * 0.95) / (end as f64 / PPQ as f64);
        self.view.px_per_beat = ppb.clamp(view::MIN_PX_PER_BEAT, view::MAX_PX_PER_BEAT);
        self.view.scroll_x = 0.0;
        Outcome::view()
    }

    /// Keeps the playhead visible while playing.
    pub fn follow(&mut self, playhead: TickTime) -> bool {
        let before = self.view.scroll_x;
        let x = self.view.tick_to_x(playhead);
        if x < 0.0 || x > self.width {
            // Page-flip like most DAWs: the playhead jumps back to the left.
            self.view.scroll_x = (playhead.0 as f64 * self.view.px_per_tick() - 40.0).max(0.0);
        }
        self.view.scroll_x != before
    }

    // -----------------------------------------------------------------------
    // Pointer input

    pub fn press(&mut self, doc: &mut Document, p: Pointer) -> Outcome {
        let project = doc.project();
        let hit = self.hit(project, p.x, p.y);
        if p.button == Button::Right {
            if let Hit::Clip { clip, .. } = hit
                && !self.selection.contains(&clip)
            {
                self.selection = BTreeSet::from([clip]);
            }
            return Outcome {
                view: true,
                context_menu: Some(hit),
                ..Default::default()
            };
        }
        if p.button == Button::Middle {
            return Outcome::default();
        }
        match hit {
            Hit::Ruler(t) => {
                if p.shift {
                    let t = self.snap.apply(t, self.view.px_per_tick(), &self.meter(project));
                    self.drag = Some(Drag::Loop { from: t });
                    Outcome::view()
                } else {
                    self.drag = Some(Drag::Scrub);
                    Outcome {
                        locate: Some(t),
                        ..Outcome::view()
                    }
                }
            }
            Hit::Clip { clip, track, .. } => {
                self.selected_track = Some(track);
                if p.ctrl {
                    if !self.selection.remove(&clip) {
                        self.selection.insert(clip);
                    }
                } else if !self.selection.contains(&clip) {
                    if !p.shift {
                        self.selection.clear();
                    }
                    self.selection.insert(clip);
                }
                self.drag = Some(Drag::Pending { start: p, hit });
                Outcome::view()
            }
            Hit::Lane { track, .. } => {
                self.selected_track = Some(track);
                self.drag = Some(Drag::Pending { start: p, hit });
                Outcome::view()
            }
            Hit::Empty(_) => {
                self.drag = Some(Drag::Pending { start: p, hit });
                Outcome::view()
            }
        }
    }

    pub fn motion(&mut self, doc: &mut Document, env: &Env, p: Pointer) -> Outcome {
        let Some(drag) = self.drag.clone() else {
            return Outcome::default();
        };
        match drag {
            Drag::Pending { start, hit } => {
                if (p.x - start.x).hypot(p.y - start.y) < DRAG_THRESHOLD {
                    return Outcome::default();
                }
                self.begin_drag(doc, start, hit);
                self.motion(doc, env, p)
            }
            Drag::Move {
                tick,
                row,
                lead,
                clips,
            } => self.drag_move(doc, p, tick, row, lead, &clips),
            Drag::Trim {
                clip,
                zone,
                tick,
                start,
                length,
                offset,
            } => self.drag_trim(doc, env, p, clip, zone, tick, start, length, offset),
            Drag::Rubber { x0, y0, base, .. } => {
                let r = Rect::spanning(x0, y0, p.x, p.y);
                let mut sel = base.clone();
                for (row, t) in doc.project().tracks.iter().enumerate() {
                    for c in &t.clips {
                        if self.clip_rect(row, c).intersects(&r) {
                            sel.insert(c.id);
                        }
                    }
                }
                self.selection = sel;
                self.drag = Some(Drag::Rubber {
                    x0,
                    y0,
                    x1: p.x,
                    y1: p.y,
                    base,
                });
                Outcome::view()
            }
            Drag::Scrub => Outcome {
                locate: Some(self.view.x_to_tick(p.x).max(TickTime(0))),
                ..Default::default()
            },
            Drag::Loop { from } => {
                let to = self.snap.apply(
                    self.view.x_to_tick(p.x).max(TickTime(0)),
                    self.view.px_per_tick(),
                    &self.meter(doc.project()),
                );
                let range = (from.min(to), from.max(to));
                Outcome {
                    set_loop: Some((range.1 > range.0).then_some(range)),
                    ..Outcome::view()
                }
            }
        }
    }

    pub fn release(&mut self, doc: &mut Document, p: Pointer) -> Outcome {
        let drag = self.drag.take();
        let out = match drag {
            Some(Drag::Pending { hit, start }) => match hit {
                // A click (no drag) on a clip selects only it.
                Hit::Clip { clip, .. } if !start.shift && !start.ctrl => {
                    self.selection = BTreeSet::from([clip]);
                    Outcome::view()
                }
                Hit::Lane { .. } | Hit::Empty(_) if !start.shift && !start.ctrl => {
                    self.selection.clear();
                    Outcome::view()
                }
                _ => Outcome::view(),
            },
            Some(Drag::Move { .. } | Drag::Trim { .. }) => {
                // The drag merged into one undo step; end it so the next edit
                // doesn't merge in too.
                doc.seal();
                Outcome::view()
            }
            Some(Drag::Rubber { .. }) => Outcome::view(),
            Some(Drag::Scrub) => Outcome {
                locate: Some(self.view.x_to_tick(p.x).max(TickTime(0))),
                ..Default::default()
            },
            Some(Drag::Loop { .. }) => Outcome::view(),
            None => Outcome::default(),
        };
        out
    }

    /// Drops the drag in progress (Escape, or the pointer was taken away).
    pub fn cancel(&mut self, doc: &mut Document) -> Outcome {
        match self.drag.take() {
            Some(Drag::Move { .. } | Drag::Trim { .. }) => {
                // The drag is one merged undo step: undo it.
                doc.seal();
                let _ = doc.undo();
                Outcome::document()
            }
            Some(_) => Outcome::view(),
            None => Outcome::default(),
        }
    }

    pub fn double_click(&mut self, doc: &mut Document, p: Pointer) -> Outcome {
        self.drag = None;
        match self.hit(doc.project(), p.x, p.y) {
            Hit::Clip { clip, .. } => Outcome {
                open_clip: Some(clip),
                ..Outcome::view()
            },
            Hit::Lane { track, tick, row } => {
                let project = doc.project();
                if project.tracks[row].kind != TrackKind::Instrument {
                    return Outcome::default();
                }
                let meter = self.meter(project);
                let bar = meter.bar_at(tick);
                let clip = Clip::new(
                    "MIDI clip",
                    bar.start,
                    TickTime(bar.len),
                    ClipContent::Midi(MidiClip::default()),
                );
                let id = clip.id;
                let r = doc.execute(AddClip::new(track, clip)).map(|_| ());
                doc.seal();
                if r.is_ok() {
                    self.selection = BTreeSet::from([id]);
                }
                Outcome::from_result(r)
            }
            _ => Outcome::default(),
        }
    }

    fn begin_drag(&mut self, doc: &mut Document, start: Pointer, hit: Hit) {
        let project = doc.project();
        let tick = self.view.x_to_tick(start.x);
        self.drag = match hit {
            Hit::Clip {
                clip,
                zone: Zone::Body,
                row,
                ..
            } => {
                let clips = project
                    .tracks
                    .iter()
                    .enumerate()
                    .flat_map(|(r, t)| t.clips.iter().map(move |c| (r, c)))
                    .filter(|(_, c)| self.selection.contains(&c.id))
                    .map(|(r, c)| (c.id, r, c.start))
                    .collect();
                doc.seal();
                Some(Drag::Move {
                    tick,
                    row,
                    lead: clip,
                    clips,
                })
            }
            Hit::Clip { clip, zone, .. } => {
                let c = project.clip(clip).expect("hit a clip that exists");
                let offset = match &c.content {
                    ClipContent::Audio(a) => Some(a.offset),
                    ClipContent::Midi(_) => None,
                };
                let (start, length) = (c.start, c.length);
                self.selection = BTreeSet::from([clip]);
                doc.seal();
                Some(Drag::Trim {
                    clip,
                    zone,
                    tick,
                    start,
                    length,
                    offset,
                })
            }
            Hit::Lane { .. } | Hit::Empty(_) => {
                let base = if start.shift || start.ctrl {
                    self.selection.clone()
                } else {
                    BTreeSet::new()
                };
                Some(Drag::Rubber {
                    x0: start.x,
                    y0: start.y,
                    x1: start.x,
                    y1: start.y,
                    base,
                })
            }
            Hit::Ruler(_) => None,
        };
    }

    fn drag_move(
        &mut self,
        doc: &mut Document,
        p: Pointer,
        tick: TickTime,
        row: usize,
        lead: ClipId,
        clips: &[(ClipId, usize, TickTime)],
    ) -> Outcome {
        let project = doc.project();
        let rows = project.tracks.len();
        let Some(&(_, _, lead_start)) = clips.iter().find(|c| c.0 == lead) else {
            return Outcome::default();
        };
        // Horizontal: move the grabbed clip's start to the grid (Shift
        // turns snapping off), and everything else by the same amount.
        let raw = lead_start.0 + (self.view.x_to_tick(p.x).0 - tick.0);
        let snapped = if p.shift {
            TickTime(raw)
        } else {
            self.snap
                .apply(TickTime(raw), self.view.px_per_tick(), &self.meter(project))
        };
        let earliest = clips.iter().map(|c| c.2.0).min().unwrap_or(0);
        let delta = (snapped.0 - lead_start.0).max(-earliest);

        // Vertical: by whole rows, only if every clip lands on a track that
        // takes it.
        let target_row = self
            .row_at(p.y, rows)
            .unwrap_or(if p.y < self.ruler_height { 0 } else { rows.saturating_sub(1) });
        let mut drow = target_row as i64 - row as i64;
        let fits = |d: i64| {
            clips.iter().all(|&(id, r, _)| {
                let to = r as i64 + d;
                to >= 0
                    && (to as usize) < rows
                    && project
                        .clip(id)
                        .is_some_and(|c| project.tracks[to as usize].kind.accepts(&c.content))
            })
        };
        if !fits(drow) {
            drow = 0;
        }

        let moves: Vec<Box<dyn Command>> = clips
            .iter()
            .map(|&(id, r, start)| {
                let to = (r as i64 + drow) as usize;
                Box::new(MoveClip::new(
                    id,
                    project.tracks[to].id,
                    TickTime(start.0 + delta),
                )) as Box<dyn Command>
            })
            .collect();
        let key = clips.iter().map(|c| c.0).collect();
        let r = doc
            .execute(Group::new("Move Clips", key, moves))
            .map(|_| ());
        Outcome::from_result(r)
    }

    #[allow(clippy::too_many_arguments)]
    fn drag_trim(
        &mut self,
        doc: &mut Document,
        env: &Env,
        p: Pointer,
        id: ClipId,
        zone: Zone,
        tick: TickTime,
        start: TickTime,
        length: TickTime,
        offset: Option<SampleTime>,
    ) -> Outcome {
        let project = doc.project();
        let Some(clip) = project.clip(id) else {
            self.drag = None;
            return Outcome::default();
        };
        let file_end = self.file_end_tick(env, clip, start, offset);
        let meter = self.meter(project);
        let moved = TickTime(self.view.x_to_tick(p.x).0 - tick.0);
        let snap = |t: TickTime| {
            if p.shift {
                t
            } else {
                self.snap.apply(t, self.view.px_per_tick(), &meter)
            }
        };
        let end = start.0 + length.0;
        let cmd = match zone {
            Zone::End => {
                let mut new_end = snap(TickTime(end + moved.0)).0.max(start.0 + MIN_CLIP_TICKS);
                if let Some(limit) = file_end {
                    new_end = new_end.min(limit.max(start.0 + MIN_CLIP_TICKS));
                }
                TrimClip::new(id, start, TickTime(new_end - start.0), offset)
            }
            Zone::Start | Zone::Body => {
                let mut new_start = snap(TickTime(start.0 + moved.0)).0.min(end - MIN_CLIP_TICKS);
                new_start = new_start.max(0);
                let new_offset = match offset {
                    Some(o) => {
                        // Can't start before the beginning of the file.
                        let earliest = env.samples_to_ticks(SampleTime(
                            env.ticks_to_samples(start).0 - o.0,
                        ));
                        new_start = new_start.max(earliest.0);
                        let shift = env.ticks_to_samples(TickTime(new_start)).0
                            - env.ticks_to_samples(start).0;
                        Some(SampleTime((o.0 + shift).max(0)))
                    }
                    None => {
                        // Don't cut notes off the front of a MIDI clip.
                        // Notes keep their timeline position through a trim, so
                        // the first one's is the same in every step of a drag.
                        if let ClipContent::Midi(m) = &clip.content
                            && let Some(first) = m.notes.first()
                        {
                            new_start = new_start.min(clip.start.0 + first.start.0);
                        }
                        None
                    }
                };
                TrimClip::new(id, TickTime(new_start), TickTime(end - new_start), new_offset)
            }
        };
        let r = doc.execute(cmd).map(|_| ());
        Outcome::from_result(r)
    }

    /// The tick where an audio clip's file runs out, if known.
    fn file_end_tick(
        &mut self,
        env: &Env,
        clip: &Clip,
        start: TickTime,
        offset: Option<SampleTime>,
    ) -> Option<i64> {
        let (ClipContent::Audio(a), Some(o)) = (&clip.content, offset) else {
            return None;
        };
        let Peaks::Ready(cache) = self.peaks.get(&env.resolve(&a.source)) else {
            return None;
        };
        let frames =
            (cache.frames as f64 * env.sample_rate / f64::from(cache.sample_rate.max(1))) as i64;
        let start_s = env.ticks_to_samples(start).0;
        Some(env.samples_to_ticks(SampleTime(start_s + frames - o.0)).0)
    }

    // -----------------------------------------------------------------------
    // Commands from menus and shortcuts

    fn selected_clips<'a>(&self, project: &'a Project) -> Vec<(&'a Arc<Track>, &'a Arc<Clip>)> {
        project
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter().map(move |c| (t, c)))
            .filter(|(_, c)| self.selection.contains(&c.id))
            .collect()
    }

    pub fn select_all(&mut self, project: &Project) -> Outcome {
        self.selection = project
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter().map(|c| c.id))
            .collect();
        Outcome::view()
    }

    pub fn delete_selection(&mut self, doc: &mut Document) -> Outcome {
        let ids: Vec<ClipId> = self
            .selected_clips(doc.project())
            .iter()
            .map(|(_, c)| c.id)
            .collect();
        if ids.is_empty() {
            return Outcome::default();
        }
        let items = ids
            .iter()
            .map(|&id| Box::new(RemoveClip::new(id)) as Box<dyn Command>)
            .collect();
        let label = if ids.len() == 1 {
            "Delete Clip"
        } else {
            "Delete Clips"
        };
        let r = doc.execute(Group::new(label, ids, items)).map(|_| ());
        doc.seal();
        self.selection.clear();
        Outcome::from_result(r)
    }

    /// Copies the selection to just after itself and selects the copies.
    pub fn duplicate_selection(&mut self, doc: &mut Document) -> Outcome {
        let project = doc.project();
        let sel = self.selected_clips(project);
        let (Some(first), Some(last)) = (
            sel.iter().map(|(_, c)| c.start.0).min(),
            sel.iter().map(|(_, c)| c.start.0 + c.length.0).max(),
        ) else {
            return Outcome::default();
        };
        let shift = last - first;
        let mut new_ids = BTreeSet::new();
        let items: Vec<Box<dyn Command>> = sel
            .iter()
            .map(|(t, c)| {
                let mut copy = (***c).clone();
                copy.id = ClipId::new();
                copy.start = TickTime(c.start.0 + shift);
                if let ClipContent::Midi(m) = &mut copy.content {
                    for n in &mut m.notes {
                        n.id = daw_model::NoteUid::new();
                    }
                }
                new_ids.insert(copy.id);
                Box::new(AddClip::new(t.id, copy)) as Box<dyn Command>
            })
            .collect();
        let key = new_ids.iter().copied().collect();
        let r = doc.execute(Group::new("Duplicate", key, items)).map(|_| ());
        doc.seal();
        if r.is_ok() {
            self.selection = new_ids;
        }
        Outcome::from_result(r)
    }

    /// Splits every selected clip under `at` in two (or, with nothing
    /// selected, every clip under it on the selected track).
    pub fn split_at(&mut self, doc: &mut Document, env: &Env, at: TickTime) -> Outcome {
        let project = doc.project();
        let targets: Vec<(TrackId, Arc<Clip>)> = project
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter().map(move |c| (t, c)))
            .filter(|(t, c)| {
                let inside = c.start < at && at.0 < c.start.0 + c.length.0;
                let chosen = if self.selection.is_empty() {
                    self.selected_track == Some(t.id)
                } else {
                    self.selection.contains(&c.id)
                };
                inside && chosen
            })
            .map(|(t, c)| (t.id, c.clone()))
            .collect();
        if targets.is_empty() {
            return Outcome::default();
        }
        let mut items: Vec<Box<dyn Command>> = Vec::new();
        let mut right_ids = Vec::new();
        for (track, c) in &targets {
            let left_len = at.0 - c.start.0;
            let mut right = (**c).clone();
            right.id = ClipId::new();
            right.start = at;
            right.length = TickTime(c.length.0 - left_len);
            match &mut right.content {
                ClipContent::Audio(a) => {
                    let shift = env.ticks_to_samples(at).0 - env.ticks_to_samples(c.start).0;
                    a.offset = SampleTime(a.offset.0 + shift);
                }
                ClipContent::Midi(m) => {
                    // Notes starting in the right half move to the new clip.
                    m.notes.retain(|n| n.start.0 >= left_len);
                    for n in &mut m.notes {
                        n.start.0 -= left_len;
                        n.id = daw_model::NoteUid::new();
                    }
                }
            }
            if let ClipContent::Midi(m) = &c.content {
                let moved: Vec<_> = m
                    .notes
                    .iter()
                    .filter(|n| n.start.0 >= left_len)
                    .map(|n| n.id)
                    .collect();
                if !moved.is_empty() {
                    items.push(Box::new(daw_model::commands::RemoveNotes::new(c.id, moved)));
                }
            }
            items.push(Box::new(SetClip::new(c.id, ClipField::Length(TickTime(left_len)))));
            right_ids.push(right.id);
            items.push(Box::new(AddClip::new(*track, right)));
        }
        let key = targets.iter().map(|(_, c)| c.id).collect();
        let r = doc.execute(Group::new("Split", key, items)).map(|_| ());
        doc.seal();
        if r.is_ok() {
            self.selection.extend(right_ids);
        }
        Outcome::from_result(r)
    }

    /// Moves the selection one grid step left (`-1`) or right (`1`).
    pub fn nudge(&mut self, doc: &mut Document, direction: i64) -> Outcome {
        let project = doc.project();
        let meter = self.meter(project);
        let sel = self.selected_clips(project);
        let Some((_, lead)) = sel.iter().min_by_key(|(_, c)| c.start) else {
            return Outcome::default();
        };
        let ppt = self.view.px_per_tick();
        let to = if direction < 0 {
            self.snap.prev(lead.start, ppt, &meter)
        } else {
            self.snap.next(lead.start, ppt, &meter)
        };
        let delta = (to.0 - lead.start.0).max(-lead.start.0);
        if delta == 0 {
            return Outcome::default();
        }
        let items: Vec<Box<dyn Command>> = sel
            .iter()
            .map(|(t, c)| {
                Box::new(MoveClip::new(c.id, t.id, TickTime(c.start.0 + delta)))
                    as Box<dyn Command>
            })
            .collect();
        let key = sel.iter().map(|(_, c)| c.id).collect();
        let r = doc.execute(Group::new("Nudge", key, items)).map(|_| ());
        Outcome::from_result(r)
    }

    /// Adds an audio clip for `file` at `at` on `track`, or on a new audio
    /// track after the last one when `track` is `None` or doesn't take audio.
    /// `frames` and `file_rate` give the file's length; the clip covers it
    /// all. Returns the new clip.
    #[allow(clippy::too_many_arguments)]
    pub fn import_audio(
        &mut self,
        doc: &mut Document,
        env: &Env,
        file: &Path,
        frames: u64,
        file_rate: u32,
        track: Option<TrackId>,
        at: TickTime,
    ) -> Result<ClipId, ModelError> {
        let project = doc.project();
        let at = TickTime(at.0.max(0));
        let name = file
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Audio".into());
        let mut items: Vec<Box<dyn Command>> = Vec::new();
        let target = match track.and_then(|t| project.track(t)) {
            Some(t) if t.kind == TrackKind::Audio => t.id,
            _ => {
                let t = Track::new(TrackKind::Audio, name.clone());
                let id = t.id;
                items.push(Box::new(AddTrack::new(project.tracks.len(), t)));
                id
            }
        };
        let engine_frames = (frames as f64 * env.sample_rate / f64::from(file_rate.max(1))) as i64;
        let s0 = env.ticks_to_samples(at).0;
        let end = env.samples_to_ticks(SampleTime(s0 + engine_frames));
        let length = TickTime((end.0 - at.0).max(1));
        let source = match env.root {
            Some(root) => file
                .strip_prefix(root)
                .map(Path::to_path_buf)
                .unwrap_or_else(|_| file.to_path_buf()),
            None => file.to_path_buf(),
        };
        let clip = Clip::new(
            name,
            at,
            length,
            ClipContent::Audio(AudioClip {
                source,
                offset: SampleTime(0),
                gain_db: 0.0,
            }),
        );
        let id = clip.id;
        items.push(Box::new(AddClip::new(target, clip)));
        doc.execute(Group::new("Import Audio", vec![id], items))?;
        doc.seal();
        self.selection = BTreeSet::from([id]);
        self.selected_track = Some(target);
        Ok(id)
    }

    /// The tick a drop at view `x` lands on, snapped.
    pub fn drop_tick(&self, project: &Project, x: f64) -> TickTime {
        let t = self.view.x_to_tick(x).max(TickTime(0));
        self.snap.apply(t, self.view.px_per_tick(), &self.meter(project))
    }

    /// The track a drop at view `y` lands on.
    pub fn drop_track(&self, project: &Project, y: f64) -> Option<TrackId> {
        self.row_at(y, project.tracks.len())
            .map(|r| project.tracks[r].id)
    }

    pub fn meter(&self, project: &Project) -> Meter {
        snap::meter(&project.tempo)
    }

    /// The display list for the current state. Starts loading the peaks of
    /// any visible audio clip that has none yet.
    pub fn scene(&mut self, project: &Project, env: &Env) -> Scene {
        draw::scene(self, project, env)
    }

    pub(crate) fn rubber_band(&self) -> Option<Rect> {
        match &self.drag {
            Some(Drag::Rubber { x0, y0, x1, y1, .. }) => Some(Rect::spanning(*x0, *y0, *x1, *y1)),
            _ => None,
        }
    }
}
