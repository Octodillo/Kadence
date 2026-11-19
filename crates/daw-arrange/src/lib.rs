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

pub mod automation;
pub mod commands;
mod draw;
pub mod gesture;
pub mod lane_editor;
pub mod peaks;
pub mod rows;
pub mod scene;
pub mod snap;
pub mod surface;
pub mod view;

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use daw_core::time::{PPQ, SampleTime, SegmentTempoMap, TempoMap, TickTime};
use daw_midi::grid::Meter;
use daw_model::commands::{AddClip, AddTrack, Batch, ClipField, MoveClip, RemoveClip, SetClip};
use daw_model::{
    AudioClip, Clip, ClipContent, ClipId, Command, Document, Fade, FadeShape, LaneId, MidiClip,
    ModelError, Project, Track, TrackId, TrackKind,
};

pub use automation::{LaneScale, ParamCatalog, PointShape};
use commands::TrimClip;
pub use draw::track_color;
use gesture::DRAG_THRESHOLD;
pub use gesture::DragStep;
pub use lane_editor::{LaneEditor, LaneFrame, LaneGrid};
pub use peaks::{PeakPool, Peaks};
pub use rows::{Group, LaneRow, OpenGroups, RowKind};
pub use scene::{Palette, Prim, Rect, Rgba, Scene};
pub use snap::Snap;
pub use surface::{Input, Surface};
pub use view::TimelineView;

/// Smallest and largest track height in logical pixels.
pub const MIN_TRACK_HEIGHT: f64 = 24.0;
pub const MAX_TRACK_HEIGHT: f64 = 400.0;
/// A new timeline's track height.
pub const DEFAULT_TRACK_HEIGHT: f64 = 64.0;

/// The shortest clip a trim leaves, in ticks (a 64th note).
const MIN_CLIP_TICKS: i64 = PPQ / 16;

/// Clip gain a gain drag can set, in dB.
pub const MIN_CLIP_GAIN_DB: f64 = -60.0;
pub const MAX_CLIP_GAIN_DB: f64 = 24.0;
/// dB per pixel of a gain drag, and with Shift held for fine control.
const GAIN_DB_PER_PX: f64 = 0.25;
const FINE_GAIN_DB_PER_PX: f64 = 0.025;

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

    /// Frames of a file at `rate` (what clip offsets count) as engine
    /// samples, and back.
    fn file_to_engine(&self, frames: i64, rate: f64) -> i64 {
        daw_core::time::file_to_engine_frames(frames, rate, self.sample_rate)
    }

    fn engine_to_file(&self, samples: i64, rate: f64) -> i64 {
        daw_core::time::engine_to_file_frames(samples, rate, self.sample_rate)
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
    /// An audio clip's fade-in handle (top left).
    FadeIn,
    /// An audio clip's fade-out handle (top right).
    FadeOut,
    /// An audio clip's gain handle (top centre): drag up or down.
    Gain,
}

/// Where an audio clip's handles are, in view pixels. See [`Arrange::handles`].
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Handles {
    pub fade_in: Rect,
    pub fade_out: Rect,
    pub gain: Rect,
    /// The x where the fade-in ends and the fade-out starts (shortened
    /// together to fit the clip, as playback does).
    pub fade_in_x: f64,
    pub fade_out_x: f64,
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
    /// A row under an expanded track (see [`Arrange::lane_rows`]): `index`
    /// counts the track's rows from 0. `lane` is the lane drawn there, if
    /// the row has one; `point` is the index of the point under the pointer.
    Automation {
        track: TrackId,
        row: usize,
        index: usize,
        lane: Option<LaneId>,
        tick: TickTime,
        point: Option<usize>,
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
    /// Over a clip's gain handle: drag up or down.
    ResizeVertical,
    /// Over the ruler: click to move the playhead.
    Pointing,
    /// Over an automation lane: click to add a point.
    Crosshair,
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
    /// A command was refused. The front end words it for the status bar.
    pub error: Option<Refusal>,
}

/// Why a gesture or command did nothing, as data for the front end to word.
#[derive(Clone, Debug, PartialEq)]
pub enum Refusal {
    /// The model refused the edit (usually a bug; the detail is English).
    Edit(ModelError),
    /// A note was drawn past the end of its clip.
    NoteOutsideClip,
    /// An automation lane's points were to be edited, but nothing describes
    /// its target's range (a parameter of a plugin that is not loaded).
    LaneNotDescribed,
}

impl Outcome {
    pub(crate) fn view() -> Self {
        Self {
            view: true,
            ..Default::default()
        }
    }

    pub(crate) fn document() -> Self {
        Self {
            document: true,
            view: true,
            ..Default::default()
        }
    }

    /// Nothing changed, for reason `r`.
    pub fn refused(r: Refusal) -> Self {
        Self {
            view: true,
            error: Some(r),
            ..Default::default()
        }
    }

    pub(crate) fn from_result(r: Result<(), ModelError>) -> Self {
        match r {
            Ok(()) => Self::document(),
            Err(e) => Self::refused(Refusal::Edit(e)),
        }
    }
}

#[derive(Clone, Debug)]
enum Drag {
    /// Pressed but not moved past the threshold yet.
    Pending {
        start: Pointer,
        hit: Hit,
    },
    /// Clips being edited, and the undo step that holds the edit so far.
    Edit {
        edit: ClipDrag,
        step: DragStep,
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

/// What a drag on a clip changes, with what it needs to know of the press.
/// Every pointer move works the edit out from this again (see [`DragStep`]).
#[derive(Clone, Debug)]
enum ClipDrag {
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
        /// Tick under the pointer at the press, and the clip's extent then.
        tick: TickTime,
        start: TickTime,
        length: TickTime,
        offset: Option<SampleTime>,
    },
    Fade {
        clip: ClipId,
        out: bool,
        /// Ticks from the press to the fade's edge, so grabbing the handle
        /// anywhere doesn't make the fade jump.
        grab: i64,
    },
    Gain {
        clip: ClipId,
        /// Pointer y and clip gain at the press.
        y: f64,
        db: f64,
    },
}

impl ClipDrag {
    fn cursor(&self) -> Cursor {
        match self {
            Self::Move { .. } => Cursor::Move,
            Self::Trim { .. } | Self::Fade { .. } => Cursor::ResizeHorizontal,
            Self::Gain { .. } => Cursor::ResizeVertical,
        }
    }
}

/// Names the arrange view gives what it creates. The front end sets them in
/// the user's language; the defaults are English.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefaultNames {
    /// A MIDI clip drawn on an instrument lane.
    pub midi_clip: String,
    /// An audio clip, and its new track, when the file name has no stem.
    pub audio: String,
    /// A MIDI clip made by recording.
    pub recording: String,
}

impl Default for DefaultNames {
    fn default() -> Self {
        Self {
            midi_clip: "MIDI clip".into(),
            audio: "Audio".into(),
            recording: "Recording".into(),
        }
    }
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
    /// Size of the fade and gain handles on audio clips.
    pub handle_px: f64,
    pub snap: Snap,
    pub palette: Palette,
    pub selection: BTreeSet<ClipId>,
    pub selected_track: Option<TrackId>,
    /// The clip under the pointer, which shows its handles like a selected
    /// one. Set by [`Arrange::hover`].
    pub hovered: Option<ClipId>,
    /// Tracks whose rows (controls and their lanes) are shown under them.
    pub expanded: BTreeSet<TrackId>,
    /// The open groups among the rows of expanded tracks (see [`rows`]).
    pub open: OpenGroups,
    /// Height of one row under a track.
    pub lane_height: f64,
    /// Selected automation points and drags on lanes.
    pub lanes: LaneEditor,
    /// Ranges and parameter lists of device parameters, for lanes that
    /// automate them. Without one, device parameter lanes show 0 to 1.
    pub params: Option<Arc<dyn ParamCatalog>>,
    pub peaks: PeakPool,
    pub names: DefaultNames,
    drag: Option<Drag>,
    /// Sample rates of audio files whose peaks aren't loaded yet.
    rates: HashMap<PathBuf, f64>,
    /// Top of each track row in content coordinates (from the top of the
    /// first track), and the bottom of the last: rows are as tall as a
    /// track plus its shown lanes. See [`Arrange::relayout`].
    tops: Vec<f64>,
    /// The rows under each track, none for a collapsed one. Laid out with
    /// `tops`.
    rows: Vec<TrackRows>,
}

/// The rows under one track, and what they were made from: the track as it
/// was (edits replace a track, so the same `Arc` means the same track), its
/// open groups and the parameter catalog's revision. [`Arrange::relayout`]
/// keeps them while all stay the same, so laying out every frame doesn't
/// list every device's parameters.
#[derive(Debug, Default)]
struct TrackRows {
    track: Option<Arc<Track>>,
    open: Vec<Group>,
    params: u64,
    rows: Vec<LaneRow>,
}

impl TrackRows {
    /// Whether these are the rows of `track` with the groups `open`, from
    /// the catalog at revision `params`.
    fn of(&self, track: &Track, open: &[Group], params: u64) -> bool {
        self.track
            .as_deref()
            .is_some_and(|t| std::ptr::eq(t, track))
            && self.open == open
            && self.params == params
    }
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
            track_height: DEFAULT_TRACK_HEIGHT,
            ruler_height: 24.0,
            scroll_y: 0.0,
            edge_px: 6.0,
            handle_px: 8.0,
            snap: Snap::default(),
            palette: Palette::default(),
            selection: BTreeSet::new(),
            selected_track: None,
            hovered: None,
            expanded: BTreeSet::new(),
            open: OpenGroups::new(),
            lane_height: 40.0,
            lanes: LaneEditor::default(),
            params: None,
            peaks: PeakPool::new(),
            names: DefaultNames::default(),
            drag: None,
            rates: HashMap::new(),
            tops: Vec::new(),
            rows: Vec::new(),
        }
    }

    // -----------------------------------------------------------------------
    // Geometry

    /// Works out the row heights again. Call it after the project or
    /// [`expanded`](Self::expanded) changed and before asking for geometry;
    /// every input method and [`scene`](Self::scene) call it themselves.
    pub fn relayout(&mut self, project: &Project) {
        self.lanes.check(project);
        self.expanded
            .retain(|id| project.tracks.iter().any(|t| t.id == *id));
        self.open.retain(|(id, group)| {
            project.track(*id).is_some_and(|t| match group {
                Group::Device(d) | Group::More(d) => t.devices.iter().any(|x| x.id == *d),
                _ => true,
            })
        });
        self.tops.clear();
        self.tops.reserve(project.tracks.len() + 1);
        let mut laid: HashMap<TrackId, TrackRows> = std::mem::take(&mut self.rows)
            .into_iter()
            .filter_map(|r| Some((r.track.as_ref()?.id, r)))
            .collect();
        self.rows.reserve(project.tracks.len());
        let params = self.params.as_deref().map_or(0, |p| p.revision());
        let mut y = 0.0;
        self.tops.push(y);
        for t in &project.tracks {
            let rows = if self.expanded.contains(&t.id) {
                let open = self.open_groups(t.id);
                match laid.remove(&t.id) {
                    Some(r) if r.of(t, &open, params) => r,
                    _ => TrackRows {
                        rows: rows::track_rows(t, self.params.as_deref(), &self.open),
                        track: Some(Arc::clone(t)),
                        open,
                        params,
                    },
                }
            } else {
                TrackRows::default()
            };
            y += self.track_height + rows.rows.len() as f64 * self.lane_height;
            self.tops.push(y);
            self.rows.push(rows);
        }
    }

    /// The groups of `track` that are open, in order.
    fn open_groups(&self, track: TrackId) -> Vec<Group> {
        self.open
            .iter()
            .filter(|(t, _)| *t == track)
            .map(|&(_, g)| g)
            .collect()
    }

    /// The rows `track` shows when expanded, as things are now: the laid
    /// out ones while they still fit (see [`relayout`](Self::relayout)), so
    /// the track headers can ask right after an edit, and cheaply.
    pub fn rows_of(&self, track: &Track) -> Vec<LaneRow> {
        let open = self.open_groups(track.id);
        let params = self.params.as_deref().map_or(0, |p| p.revision());
        match self.rows.iter().find(|r| r.of(track, &open, params)) {
            Some(r) => r.rows.clone(),
            None => rows::track_rows(track, self.params.as_deref(), &self.open),
        }
    }

    /// The rows track row `row` shows under it, as last laid out: none
    /// while it is collapsed. [`hit`](Self::hit) and
    /// [`lane_frame`](Self::lane_frame) read these too, so whoever changes
    /// the document or [`expanded`](Self::expanded) from outside calls
    /// [`relayout`](Self::relayout) before asking.
    pub fn lane_rows(&self, row: usize) -> &[LaneRow] {
        self.rows.get(row).map_or(&[], |r| r.rows.as_slice())
    }

    /// Top of row `row` in content coordinates.
    fn row_top(&self, row: usize) -> f64 {
        match self.tops.get(row) {
            Some(&y) => y,
            // Not laid out yet: rows past the known ones are bare tracks.
            None => {
                let known = self.tops.len().saturating_sub(1);
                self.tops.last().copied().unwrap_or(0.0) + (row - known) as f64 * self.track_height
            }
        }
    }

    /// Top of track row `row` in view coordinates.
    pub fn row_y(&self, row: usize) -> f64 {
        self.ruler_height + self.row_top(row) - self.scroll_y
    }

    /// Height of row `row`: its track and the lanes shown under it.
    pub fn row_height(&self, row: usize) -> f64 {
        self.row_top(row + 1) - self.row_top(row)
    }

    /// Top of lane row `lane` (counting from 0 under the track) of track
    /// row `row`, in view coordinates.
    pub fn lane_y(&self, row: usize, lane: usize) -> f64 {
        self.row_y(row) + self.track_height + lane as f64 * self.lane_height
    }

    /// The track row under `y`, if `y` is over a track or its lanes.
    pub fn row_at(&self, y: f64, rows: usize) -> Option<usize> {
        if y < self.ruler_height || rows == 0 {
            return None;
        }
        let cy = y - self.ruler_height + self.scroll_y;
        if cy < 0.0 {
            return None;
        }
        let laid_out = self.tops.len() == rows + 1;
        let r = if laid_out {
            self.tops.partition_point(|&t| t <= cy).checked_sub(1)?
        } else {
            (cy / self.track_height).floor() as usize
        };
        (r < rows).then_some(r)
    }

    /// The row whose top is closest above content-y `cy`, clamped to the
    /// rows there are: where a dragged track header lands.
    pub fn row_at_content(&self, cy: f64, rows: usize) -> usize {
        let y = cy + self.ruler_height - self.scroll_y;
        match self.row_at(y, rows) {
            Some(r) => r,
            None if cy < 0.0 => 0,
            None => rows.saturating_sub(1),
        }
    }

    /// Height of all tracks together, with their lanes.
    pub fn content_height(&self, project: &Project) -> f64 {
        if self.tops.len() == project.tracks.len() + 1 {
            self.tops.last().copied().unwrap_or(0.0)
        } else {
            project.tracks.len() as f64 * self.track_height
        }
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
        Rect::new(
            x0,
            self.row_y(row) + 1.0,
            (x1 - x0).max(1.0),
            self.track_height - 2.0,
        )
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
        if y >= self.row_y(row) + self.track_height {
            return self.lane_hit(track, row, x, y, tick);
        }
        // Later clips paint over earlier ones, so look from the end.
        for clip in track.clips.iter().rev() {
            let r = self.clip_rect(row, clip);
            if !r.contains(x, y) {
                continue;
            }
            let edge = self.edge_px.min(r.w / 4.0);
            let handle = self.active_handles(row, clip).and_then(|h| {
                [
                    (h.fade_in, Zone::FadeIn),
                    (h.fade_out, Zone::FadeOut),
                    (h.gain, Zone::Gain),
                ]
                .into_iter()
                .find(|(r, _)| r.contains(x, y))
                .map(|(_, z)| z)
            });
            let zone = if let Some(z) = handle {
                z
            } else if x < r.x + edge {
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
        if let Some(c) = self.lanes.cursor() {
            return c;
        }
        if let Some(Drag::Edit { edit, .. }) = &self.drag {
            return edit.cursor();
        }
        match self.hit(project, x, y) {
            Hit::Ruler(_) => Cursor::Pointing,
            Hit::Clip {
                zone: Zone::Body, ..
            } => Cursor::Move,
            Hit::Clip {
                zone: Zone::Gain, ..
            } => Cursor::ResizeVertical,
            Hit::Clip { .. } => Cursor::ResizeHorizontal,
            Hit::Automation {
                lane: Some(lane),
                point,
                ..
            } => self
                .lane_frame(project, lane)
                .map_or(Cursor::Arrow, |f| f.cursor(point.is_some())),
            // A control without a lane: a click starts one.
            Hit::Automation { row, index, .. }
                if self
                    .lane_rows(row)
                    .get(index)
                    .is_some_and(|r| r.target().is_some()) =>
            {
                Cursor::Crosshair
            }
            _ => Cursor::Arrow,
        }
    }

    /// Whether a drag is in progress (the front end should grab the pointer).
    pub fn dragging(&self) -> bool {
        self.drag.is_some() || self.lanes.dragging().is_some()
    }

    /// The fade and gain handles of an audio clip on track row `row`, or
    /// `None` for a MIDI clip or one too small to show them.
    pub fn handles(&self, row: usize, clip: &Clip) -> Option<Handles> {
        let ClipContent::Audio(a) = &clip.content else {
            return None;
        };
        let r = self.clip_rect(row, clip);
        let h = self.handle_px;
        if r.w < 4.0 * h || r.h < 2.0 * h {
            return None;
        }
        let ppt = self.view.px_per_tick();
        let (fi, fo) = daw_model::fit_fades(a.fade_in.length.0, a.fade_out.length.0, clip.length.0);
        let fade_in_x = r.x + fi as f64 * ppt;
        let fade_out_x = r.right() - fo as f64 * ppt;
        // Keep the handles inside the clip and apart from each other.
        let in_x = fade_in_x.clamp(r.x, r.right() - 2.0 * h);
        let out_x = fade_out_x.clamp(in_x + 2.0 * h, r.right());
        let fade_in = Rect::new(in_x, r.y, h, h);
        let fade_out = Rect::new(out_x - h, r.y, h, h);
        // The gain handle steps down a row where a fade ends mid-clip, so
        // neither covers the other.
        let mut gain = Rect::new(r.x + (r.w - h) / 2.0, r.y, h, h);
        if gain.intersects(&fade_in) || gain.intersects(&fade_out) {
            gain.y += h;
        }
        Some(Handles {
            fade_in,
            fade_out,
            gain,
            fade_in_x,
            fade_out_x,
        })
    }

    /// [`handles`](Self::handles), only for a clip that shows them: the
    /// selected and hovered ones. Others keep the whole clip for moving and
    /// trimming, and stay uncluttered.
    pub fn active_handles(&self, row: usize, clip: &Clip) -> Option<Handles> {
        if self.selection.contains(&clip.id) || self.hovered == Some(clip.id) {
            self.handles(row, clip)
        } else {
            None
        }
    }

    /// The pointer moved without a button. Returns whether the hovered clip
    /// changed, so the front end redraws.
    pub fn hover(&mut self, project: &Project, x: f64, y: f64) -> bool {
        let clip = match self.hit(project, x, y) {
            Hit::Clip { clip, .. } => Some(clip),
            _ => None,
        };
        let changed = clip != self.hovered;
        self.hovered = clip;
        changed
    }

    /// The gain of the clip being gain-dragged, so the front end can show
    /// the value by the pointer.
    pub fn dragged_gain(&self, project: &Project) -> Option<f64> {
        let Some(Drag::Edit {
            edit: ClipDrag::Gain { clip, .. },
            ..
        }) = &self.drag
        else {
            return None;
        };
        match &project.clip(*clip)?.content {
            ClipContent::Audio(a) => Some(a.gain_db),
            ClipContent::Midi(_) => None,
        }
    }

    // -----------------------------------------------------------------------
    // View changes

    /// Resizes the view, keeping the vertical scroll in range.
    pub fn resize(&mut self, project: &Project, width: f64, height: f64) {
        self.width = width.max(1.0);
        self.height = height.max(1.0);
        self.relayout(project);
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
        self.relayout(project);
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

    /// Whether a one-finger touch drag from `(x, y)` should scroll the view
    /// rather than edit: on empty lane space, an automation row with no lane
    /// in it, and below the last track. On a clip, the ruler or an
    /// automation lane it edits, as a mouse would.
    pub fn touch_pans(&self, project: &Project, x: f64, y: f64) -> bool {
        matches!(
            self.hit(project, x, y),
            Hit::Lane { .. } | Hit::Empty(_) | Hit::Automation { lane: None, .. }
        )
    }

    /// A pinch gesture: `scale` relative to the previous update, centred on
    /// `x`.
    pub fn pinch(&mut self, project: &Project, scale: f64, x: f64) -> Outcome {
        if !scale.is_finite() || scale <= 0.0 {
            return Outcome::default();
        }
        self.view.zoom_at(scale, x);
        self.clamp_scroll(project);
        Outcome::view()
    }

    pub fn zoom_tracks(&mut self, project: &Project, factor: f64, anchor_y: f64) {
        self.relayout(project);
        // Keep the same spot of the same row under the anchor.
        let rows = project.tracks.len();
        let content_y = anchor_y - self.ruler_height + self.scroll_y;
        let row = self.row_at_content(content_y, rows);
        let frac = if rows == 0 {
            0.0
        } else {
            ((content_y - self.row_top(row)) / self.row_height(row).max(1.0)).clamp(0.0, 1.0)
        };
        self.track_height = (self.track_height * factor).clamp(MIN_TRACK_HEIGHT, MAX_TRACK_HEIGHT);
        self.relayout(project);
        self.scroll_y =
            self.row_top(row) + frac * self.row_height(row) - (anchor_y - self.ruler_height);
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
        // At most one drag at a time: one whose release got lost ends here.
        self.end_drag(doc);
        self.relayout(doc.project());
        let project = doc.project();
        let hit = self.hit(project, p.x, p.y);
        if let Hit::Automation { .. } = hit {
            return self.press_lane(doc, p, hit);
        }
        if p.button != Button::Middle && !matches!(hit, Hit::Ruler(_)) {
            self.lanes.clear_selection();
        }
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
                    let t = self
                        .snap
                        .apply(t, self.view.px_per_tick(), &self.meter(project));
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
            Hit::Automation { .. } => unreachable!("handled by press_lane"),
        }
    }

    pub fn motion(&mut self, doc: &mut Document, env: &Env, p: Pointer) -> Outcome {
        if let Some(lane) = self.lanes.dragging() {
            self.relayout(doc.project());
            let frame = self.lane_frame(doc.project(), lane);
            let grid = self.lane_grid(doc.project());
            return self.lanes.drag(doc, frame.as_ref(), &grid, p);
        }
        let Some(drag) = self.drag.clone() else {
            return Outcome::default();
        };
        self.relayout(doc.project());
        match drag {
            Drag::Pending { start, hit } => {
                if (p.x - start.x).hypot(p.y - start.y) < DRAG_THRESHOLD {
                    return Outcome::default();
                }
                self.begin_drag(doc, start, hit);
                self.motion(doc, env, p)
            }
            Drag::Edit { edit, mut step } => {
                let o = step.replace(doc, |project| self.clip_edit(project, env, p, &edit));
                self.drag = Some(Drag::Edit { edit, step });
                o
            }
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
        if let Some(o) = self.lanes.release(doc, p) {
            return o;
        }
        let drag = self.drag.take();
        match drag {
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
            Some(Drag::Edit { .. }) => {
                // End the drag's undo step, so the next edit doesn't merge
                // into it.
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
        }
    }

    /// Drops the drag in progress and takes back what it did (Escape, or the
    /// pointer was taken away). Nothing of it is left to undo or redo.
    pub fn cancel(&mut self, doc: &mut Document) -> Outcome {
        if let Some(o) = self.lanes.cancel(doc) {
            return o;
        }
        match self.drag.take() {
            Some(Drag::Edit { step, .. }) => step.cancel(doc),
            Some(_) => Outcome::view(),
            None => Outcome::default(),
        }
    }

    /// Ends the drag in progress, keeping what it did. Call it before any
    /// other edit (a shortcut, undo) while a button may still be held, so the
    /// drag neither merges with that edit nor works on clips it removed.
    pub fn end_drag(&mut self, doc: &mut Document) {
        self.lanes.end_drag(doc);
        if let Some(Drag::Edit { .. }) = self.drag.take() {
            doc.seal();
        }
    }

    pub fn double_click(&mut self, doc: &mut Document, p: Pointer) -> Outcome {
        self.end_drag(doc);
        self.relayout(doc.project());
        match self.hit(doc.project(), p.x, p.y) {
            // Double-clicking an automation point deletes it.
            Hit::Automation {
                lane: Some(lane),
                point: Some(i),
                ..
            } => match self.lane_frame(doc.project(), lane) {
                Some(frame) => self.lanes.delete_point(doc, &frame, lane, i),
                None => Outcome::default(),
            },
            // Double-clicking a handle resets what it sets.
            Hit::Clip {
                clip,
                zone: zone @ (Zone::FadeIn | Zone::FadeOut | Zone::Gain),
                ..
            } => {
                let field = match zone {
                    Zone::FadeIn => ClipField::FadeIn(Fade::default()),
                    Zone::FadeOut => ClipField::FadeOut(Fade::default()),
                    _ => ClipField::AudioGainDb(0.0),
                };
                let r = doc.execute(SetClip::new(clip, field)).map(|_| ());
                doc.seal();
                Outcome::from_result(r)
            }
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
                    self.names.midi_clip.clone(),
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
        let edit = |edit| {
            Some(Drag::Edit {
                edit,
                step: DragStep::default(),
            })
        };
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
                edit(ClipDrag::Move {
                    tick,
                    row,
                    lead: clip,
                    clips,
                })
            }
            // The clip can be gone if a shortcut deleted it while the button
            // was down.
            Hit::Clip { clip, zone, .. } => project.clip(clip).and_then(|c| {
                self.selection = BTreeSet::from([clip]);
                match (zone, &c.content) {
                    (Zone::Gain, ClipContent::Audio(a)) => edit(ClipDrag::Gain {
                        clip,
                        y: start.y,
                        db: a.gain_db,
                    }),
                    (Zone::FadeIn | Zone::FadeOut, ClipContent::Audio(a)) => {
                        let (fi, fo) = daw_model::fit_fades(
                            a.fade_in.length.0,
                            a.fade_out.length.0,
                            c.length.0,
                        );
                        let out = zone == Zone::FadeOut;
                        let edge = if out {
                            c.start.0 + c.length.0 - fo
                        } else {
                            c.start.0 + fi
                        };
                        edit(ClipDrag::Fade {
                            clip,
                            out,
                            grab: edge - tick.0,
                        })
                    }
                    // Only audio clips have handles.
                    (Zone::Gain | Zone::FadeIn | Zone::FadeOut, ClipContent::Midi(_)) => None,
                    (Zone::Start | Zone::End | Zone::Body, content) => edit(ClipDrag::Trim {
                        clip,
                        zone,
                        tick,
                        start: c.start,
                        length: c.length,
                        offset: match content {
                            ClipContent::Audio(a) => Some(a.offset),
                            ClipContent::Midi(_) => None,
                        },
                    }),
                }
            }),
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
            Hit::Ruler(_) | Hit::Automation { .. } => None,
        };
    }

    /// The edit that takes the clips of a drag to the pointer at `p`, or
    /// `None` when that is how `project` has them. Worked out from what the
    /// drag noted at the press (see [`DragStep::replace`]).
    fn clip_edit(
        &mut self,
        project: &Project,
        env: &Env,
        p: Pointer,
        edit: &ClipDrag,
    ) -> Option<Box<dyn Command>> {
        match *edit {
            ClipDrag::Move {
                tick,
                row,
                lead,
                ref clips,
            } => self.move_edit(project, p, tick, row, lead, clips),
            ClipDrag::Trim {
                clip,
                zone,
                tick,
                start,
                length,
                offset,
            } => self.trim_edit(project, env, p, clip, zone, tick, (start, length, offset)),
            ClipDrag::Fade { clip, out, grab } => self.fade_edit(project, p, clip, out, grab),
            ClipDrag::Gain { clip, y, db } => Self::gain_edit(project, p, clip, y, db),
        }
    }

    /// The edit that moves `clips` with the pointer at `p`, or `None` when
    /// that is where they are (or they are gone).
    fn move_edit(
        &self,
        project: &Project,
        p: Pointer,
        tick: TickTime,
        row: usize,
        lead: ClipId,
        clips: &[(ClipId, usize, TickTime)],
    ) -> Option<Box<dyn Command>> {
        let rows = project.tracks.len();
        if clips
            .iter()
            .any(|&(id, r, _)| r >= rows || project.clip(id).is_none())
        {
            // Tracks or clips went away under the drag.
            return None;
        }
        let &(_, _, lead_start) = clips.iter().find(|c| c.0 == lead)?;
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
            .unwrap_or(if p.y < self.ruler_height {
                0
            } else {
                rows.saturating_sub(1)
            });
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

        let is_at = |id: ClipId, track: TrackId, start: TickTime| {
            project
                .track(track)
                .is_some_and(|t| t.clips.iter().any(|c| c.id == id && c.start == start))
        };
        let moves: Vec<(ClipId, TrackId, TickTime)> = clips
            .iter()
            .map(|&(id, r, start)| {
                let to = (r as i64 + drow) as usize;
                (id, project.tracks[to].id, TickTime(start.0 + delta))
            })
            .collect();
        if moves
            .iter()
            .all(|&(id, track, start)| is_at(id, track, start))
        {
            return None;
        }
        let items = moves
            .into_iter()
            .map(|(id, track, start)| Box::new(MoveClip::new(id, track, start)) as Box<dyn Command>)
            .collect();
        Some(Box::new(Batch::new("Move Clips", items)))
    }

    /// The edit that sets a fade's length to reach the pointer, leaving the
    /// other fade room, or `None` when the fade is that long already. Not
    /// snapped: fades are usually set by ear.
    fn fade_edit(
        &self,
        project: &Project,
        p: Pointer,
        id: ClipId,
        out: bool,
        grab: i64,
    ) -> Option<Box<dyn Command>> {
        let clip = project.clip(id)?;
        let ClipContent::Audio(a) = &clip.content else {
            return None;
        };
        let at = self.view.x_to_tick(p.x).0 + grab;
        let end = clip.start.0 + clip.length.0;
        let (now, reach, other) = if out {
            (a.fade_out, end - at, a.fade_in.length.0)
        } else {
            (a.fade_in, at - clip.start.0, a.fade_out.length.0)
        };
        let room = (clip.length.0 - other.min(clip.length.0)).max(0);
        let fade = Fade::new(TickTime(reach.clamp(0, room)), now.shape);
        if fade == now {
            return None;
        }
        let field = if out {
            ClipField::FadeOut(fade)
        } else {
            ClipField::FadeIn(fade)
        };
        Some(Box::new(SetClip::new(id, field)))
    }

    /// The edit that sets the gain a drag from `y`, where it was `db`, has
    /// reached at `p`, or `None` when the clip has that gain already.
    fn gain_edit(
        project: &Project,
        p: Pointer,
        id: ClipId,
        y: f64,
        db: f64,
    ) -> Option<Box<dyn Command>> {
        let ClipContent::Audio(a) = &project.clip(id)?.content else {
            return None;
        };
        let per_px = if p.shift {
            FINE_GAIN_DB_PER_PX
        } else {
            GAIN_DB_PER_PX
        };
        let to = (db + (y - p.y) * per_px).clamp(MIN_CLIP_GAIN_DB, MAX_CLIP_GAIN_DB);
        // Whole hundredths, so the shown value is the stored one.
        let to = (to * 100.0).round() / 100.0;
        (to != a.gain_db)
            .then(|| Box::new(SetClip::new(id, ClipField::AudioGainDb(to))) as Box<dyn Command>)
    }

    /// The edit that trims a clip, whose extent was `from` at the press on
    /// `tick`, to the pointer at `p`, or `None` when that is its extent
    /// already.
    #[allow(clippy::too_many_arguments)]
    fn trim_edit(
        &mut self,
        project: &Project,
        env: &Env,
        p: Pointer,
        id: ClipId,
        zone: Zone,
        tick: TickTime,
        from: (TickTime, TickTime, Option<SampleTime>),
    ) -> Option<Box<dyn Command>> {
        let (start, length, offset) = from;
        let clip = project.clip(id)?;
        let file_end = self.file_end_tick(env, clip, start, offset);
        let rate = match &clip.content {
            ClipContent::Audio(a) => self.file_rate(env, &a.source),
            ClipContent::Midi(_) => env.sample_rate,
        };
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
        let to = match zone {
            Zone::End => {
                let mut new_end = snap(TickTime(end + moved.0))
                    .0
                    .max(start.0 + MIN_CLIP_TICKS);
                if let Some(limit) = file_end {
                    new_end = new_end.min(limit.max(start.0 + MIN_CLIP_TICKS));
                }
                (start, TickTime(new_end - start.0), offset)
            }
            Zone::Start | Zone::Body | Zone::FadeIn | Zone::FadeOut | Zone::Gain => {
                let mut new_start = snap(TickTime(start.0 + moved.0))
                    .0
                    .min(end - MIN_CLIP_TICKS);
                new_start = new_start.max(0);
                let new_offset = match offset {
                    Some(o) => {
                        // Can't start before the beginning of the file.
                        let o_engine = env.file_to_engine(o.0, rate);
                        let earliest = env
                            .samples_to_ticks(SampleTime(env.ticks_to_samples(start).0 - o_engine));
                        new_start = new_start.max(earliest.0);
                        let shift = env.ticks_to_samples(TickTime(new_start)).0
                            - env.ticks_to_samples(start).0;
                        Some(SampleTime((o.0 + env.engine_to_file(shift, rate)).max(0)))
                    }
                    None => {
                        // Don't cut notes off the front of a MIDI clip.
                        // Notes keep their timeline position through a trim,
                        // so the first one's is the same all through a drag.
                        if let ClipContent::Midi(m) = &clip.content
                            && let Some(first) = m.notes.first()
                        {
                            new_start = new_start.min(clip.start.0 + first.start.0);
                        }
                        None
                    }
                };
                (TickTime(new_start), TickTime(end - new_start), new_offset)
            }
        };
        let now = (
            clip.start,
            clip.length,
            match &clip.content {
                ClipContent::Audio(a) => Some(a.offset),
                ClipContent::Midi(_) => None,
            },
        );
        (to != now).then(|| Box::new(TrimClip::new(id, to.0, to.1, to.2)) as Box<dyn Command>)
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
        let rate = f64::from(cache.sample_rate.max(1));
        let left = env.file_to_engine(cache.frames as i64 - o.0, rate).max(0);
        let start_s = env.ticks_to_samples(start).0;
        Some(env.samples_to_ticks(SampleTime(start_s + left)).0)
    }

    /// The sample rate of an audio file, which its clips' offsets count in.
    /// The engine rate if the file can't be read (it then plays silent).
    fn file_rate(&mut self, env: &Env, source: &Path) -> f64 {
        let path = env.resolve(source);
        if let Peaks::Ready(cache) = self.peaks.get(&path) {
            return f64::from(cache.sample_rate.max(1));
        }
        if let Some(&rate) = self.rates.get(&path) {
            return rate;
        }
        match daw_media::Decoder::open(&path) {
            Ok(d) => {
                let rate = f64::from(d.info().sample_rate.max(1));
                self.rates.insert(path, rate);
                rate
            }
            Err(_) => env.sample_rate,
        }
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

    /// Whether any clip is selected. [`selection`](Self::selection) can
    /// also hold clips that are gone (an undone duplicate, say, which
    /// redo selects again), so its being empty doesn't tell.
    pub fn clips_selected(&self, project: &Project) -> bool {
        !self.selection.is_empty()
            && project
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .any(|c| self.selection.contains(&c.id))
    }

    pub fn select_all(&mut self, project: &Project) -> Outcome {
        self.selection = project
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter().map(|c| c.id))
            .collect();
        // Clips and points aren't selected together: Delete takes the
        // points first, and would leave every clip.
        self.lanes.clear_selection();
        Outcome::view()
    }

    pub fn delete_selection(&mut self, doc: &mut Document) -> Outcome {
        self.end_drag(doc);
        if self.lanes.points_selected(doc.project()) {
            return self.delete_points(doc);
        }
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
        let r = doc.execute(Batch::new(label, items)).map(|_| ());
        doc.seal();
        self.selection.clear();
        Outcome::from_result(r)
    }

    /// Copies the selection to just after itself and selects the copies.
    pub fn duplicate_selection(&mut self, doc: &mut Document) -> Outcome {
        self.end_drag(doc);
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
        let r = doc.execute(Batch::new("Duplicate", items)).map(|_| ());
        doc.seal();
        if r.is_ok() {
            self.selection = new_ids;
        }
        Outcome::from_result(r)
    }

    /// Splits every selected clip under `at` in two (or, with nothing
    /// selected, every clip under it on the selected track).
    pub fn split_at(&mut self, doc: &mut Document, env: &Env, at: TickTime) -> Outcome {
        self.end_drag(doc);
        let project = doc.project();
        let selected = self.clips_selected(project);
        let targets: Vec<(TrackId, Arc<Clip>)> = project
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter().map(move |c| (t, c)))
            .filter(|(t, c)| {
                let inside = c.start < at && at.0 < c.start.0 + c.length.0;
                let chosen = if selected {
                    self.selection.contains(&c.id)
                } else {
                    self.selected_track == Some(t.id)
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
                    let rate = self.file_rate(env, &a.source);
                    a.offset = SampleTime(a.offset.0 + env.engine_to_file(shift, rate));
                    // The fade-in stays with the left half and the fade-out
                    // goes with the right one, cut short if the split is
                    // inside it (the cut-off part is lost, as in most DAWs).
                    let (_, fo) =
                        daw_model::fit_fades(a.fade_in.length.0, a.fade_out.length.0, c.length.0);
                    a.fade_in = Fade::default();
                    a.fade_out.length = TickTime(fo.min(right.length.0));
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
            if let ClipContent::Audio(a) = &c.content {
                let (fi, _) =
                    daw_model::fit_fades(a.fade_in.length.0, a.fade_out.length.0, c.length.0);
                if fi.min(left_len) != a.fade_in.length.0 {
                    items.push(Box::new(SetClip::new(
                        c.id,
                        ClipField::FadeIn(Fade::new(TickTime(fi.min(left_len)), a.fade_in.shape)),
                    )));
                }
                if !a.fade_out.is_none() {
                    items.push(Box::new(SetClip::new(
                        c.id,
                        ClipField::FadeOut(Fade::default()),
                    )));
                }
            }
            items.push(Box::new(SetClip::new(
                c.id,
                ClipField::Length(TickTime(left_len)),
            )));
            right_ids.push(right.id);
            items.push(Box::new(AddClip::new(*track, right)));
        }
        let r = doc.execute(Batch::new("Split", items)).map(|_| ());
        doc.seal();
        if r.is_ok() {
            self.selection.extend(right_ids);
        }
        Outcome::from_result(r)
    }

    /// Sets the shape of both fades on every selected audio clip.
    pub fn set_fade_shape(&mut self, doc: &mut Document, shape: FadeShape) -> Outcome {
        self.edit_selected_audio(doc, "Change Fade Shape", |a| {
            vec![
                ClipField::FadeIn(Fade::new(a.fade_in.length, shape)),
                ClipField::FadeOut(Fade::new(a.fade_out.length, shape)),
            ]
        })
    }

    /// Sets every selected audio clip back to 0 dB.
    pub fn reset_clip_gain(&mut self, doc: &mut Document) -> Outcome {
        self.edit_selected_audio(doc, "Reset Clip Gain", |_| {
            vec![ClipField::AudioGainDb(0.0)]
        })
    }

    /// Removes both fades from every selected audio clip.
    pub fn remove_fades(&mut self, doc: &mut Document) -> Outcome {
        self.edit_selected_audio(doc, "Remove Fades", |_| {
            vec![
                ClipField::FadeIn(Fade::default()),
                ClipField::FadeOut(Fade::default()),
            ]
        })
    }

    /// One undo step setting `fields(clip)` on each selected audio clip.
    fn edit_selected_audio(
        &mut self,
        doc: &mut Document,
        label: &str,
        fields: impl Fn(&AudioClip) -> Vec<ClipField>,
    ) -> Outcome {
        self.end_drag(doc);
        let mut items: Vec<Box<dyn Command>> = Vec::new();
        for (_, c) in self.selected_clips(doc.project()) {
            if let ClipContent::Audio(a) = &c.content {
                for f in fields(a) {
                    items.push(Box::new(SetClip::new(c.id, f)));
                }
            }
        }
        if items.is_empty() {
            return Outcome::default();
        }
        let r = doc.execute(Batch::new(label, items)).map(|_| ());
        doc.seal();
        Outcome::from_result(r)
    }

    /// Whether the selection holds an audio clip (for enabling clip gain and
    /// fade actions).
    pub fn audio_selected(&self, project: &Project) -> bool {
        self.selected_clips(project)
            .iter()
            .any(|(_, c)| matches!(c.content, ClipContent::Audio(_)))
    }

    /// Moves the selection one grid step left (`-1`) or right (`1`).
    pub fn nudge(&mut self, doc: &mut Document, direction: i64) -> Outcome {
        self.end_drag(doc);
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
                Box::new(MoveClip::new(c.id, t.id, TickTime(c.start.0 + delta))) as Box<dyn Command>
            })
            .collect();
        let r = doc.execute(Batch::new("Nudge", items)).map(|_| ());
        doc.seal();
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
        self.end_drag(doc);
        let project = doc.project();
        let at = TickTime(at.0.max(0));
        let name = file
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.names.audio.clone());
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
            ClipContent::Audio(AudioClip::new(source, SampleTime(0))),
        );
        let id = clip.id;
        items.push(Box::new(AddClip::new(target, clip)));
        doc.execute(Batch::new("Import Audio", items))?;
        doc.seal();
        self.selection = BTreeSet::from([id]);
        self.selected_track = Some(target);
        Ok(id)
    }

    /// The tick a drop at view `x` lands on, snapped.
    pub fn drop_tick(&self, project: &Project, x: f64) -> TickTime {
        let t = self.view.x_to_tick(x).max(TickTime(0));
        self.snap
            .apply(t, self.view.px_per_tick(), &self.meter(project))
    }

    /// The track a drop at view `y` lands on.
    pub fn drop_track(&mut self, project: &Project, y: f64) -> Option<TrackId> {
        self.relayout(project);
        self.row_at(y, project.tracks.len())
            .map(|r| project.tracks[r].id)
    }

    pub fn meter(&self, project: &Project) -> Meter {
        snap::meter(&project.tempo)
    }

    /// The display list for the current state. Starts loading the peaks of
    /// any visible audio clip that has none yet.
    pub fn scene(&mut self, project: &Project, env: &Env) -> Scene {
        self.relayout(project);
        draw::scene(self, project, env)
    }

    pub(crate) fn rubber_band(&self) -> Option<Rect> {
        match &self.drag {
            Some(Drag::Rubber { x0, y0, x1, y1, .. }) => Some(Rect::spanning(*x0, *y0, *x1, *y1)),
            _ => None,
        }
    }
}
