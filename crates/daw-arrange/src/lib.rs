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
pub mod lane_editor;
pub mod peaks;
pub mod scene;
pub mod snap;
pub mod view;

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use daw_core::time::{PPQ, SampleTime, SegmentTempoMap, TempoMap, TickTime};
use daw_midi::grid::Meter;
use daw_model::commands::{AddClip, AddTrack, ClipField, RemoveClip, SetClip};
use daw_model::{
    AudioClip, Clip, ClipContent, ClipId, Command, Document, Fade, FadeShape, LaneId, MidiClip,
    ModelError, Project, Track, TrackId, TrackKind,
};

pub use automation::{LaneScale, ParamCatalog, PointShape};
use commands::{Group, PlaceClip, TrimClip};
pub use draw::track_color;
pub use lane_editor::{LaneEditor, LaneFrame};
pub use peaks::{PeakPool, Peaks};
pub use scene::{Palette, Prim, Rect, Rgba, Scene};
pub use snap::Snap;
pub use view::TimelineView;

/// Smallest and largest track height in logical pixels.
pub const MIN_TRACK_HEIGHT: f64 = 24.0;
pub const MAX_TRACK_HEIGHT: f64 = 400.0;
/// A new timeline's track height.
pub const DEFAULT_TRACK_HEIGHT: f64 = 64.0;

/// A pointer must move this far before a press becomes a drag.
const DRAG_THRESHOLD: f64 = 3.0;

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
        (frames as f64 * self.sample_rate / rate).round() as i64
    }

    fn engine_to_file(&self, samples: i64, rate: f64) -> i64 {
        (samples as f64 * rate / self.sample_rate).round() as i64
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
    /// An automation lane under a track (see [`Arrange::expanded`]).
    /// `lane` is `None` on the empty row an expanded track without lanes
    /// shows; `point` is the index of the point under the pointer.
    Automation {
        track: TrackId,
        row: usize,
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
                error: Some(Refusal::Edit(e)),
                ..Default::default()
            },
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
    Move {
        /// Tick and row under the pointer at the press.
        tick: TickTime,
        row: usize,
        /// The clip that was grabbed; its start is what snaps.
        lead: ClipId,
        /// Each selected clip's original row and start.
        clips: Vec<(ClipId, usize, TickTime)>,
        /// Whether the drag has changed the document, so cancelling it has
        /// something of its own to undo.
        edited: bool,
    },
    Trim {
        clip: ClipId,
        zone: Zone,
        tick: TickTime,
        start: TickTime,
        length: TickTime,
        offset: Option<SampleTime>,
        edited: bool,
    },
    Fade {
        clip: ClipId,
        out: bool,
        /// Ticks from the press to the fade's edge, so grabbing the handle
        /// anywhere doesn't make the fade jump.
        grab: i64,
        edited: bool,
    },
    Gain {
        clip: ClipId,
        /// Pointer y and clip gain at the press.
        y: f64,
        db: f64,
        edited: bool,
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
    /// Tracks whose automation lanes are shown under them.
    pub expanded: BTreeSet<TrackId>,
    /// Height of one automation lane.
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
            lane_height: 40.0,
            lanes: LaneEditor::default(),
            params: None,
            peaks: PeakPool::new(),
            names: DefaultNames::default(),
            drag: None,
            rates: HashMap::new(),
            tops: Vec::new(),
        }
    }

    // -----------------------------------------------------------------------
    // Geometry

    /// Works out the row heights again. Call it after the project or
    /// [`expanded`](Self::expanded) changed and before asking for geometry;
    /// every input method and [`scene`](Self::scene) call it themselves.
    pub fn relayout(&mut self, project: &Project) {
        self.lanes.check(project);
        self.tops.clear();
        self.tops.reserve(project.tracks.len() + 1);
        let mut y = 0.0;
        self.tops.push(y);
        for t in &project.tracks {
            y += self.track_height + self.lanes_shown(t) as f64 * self.lane_height;
            self.tops.push(y);
        }
        self.expanded
            .retain(|id| project.tracks.iter().any(|t| t.id == *id));
    }

    /// How many lane rows `track` shows under it: none when collapsed, and
    /// at least one (an empty one to add lanes from) when expanded.
    pub fn lanes_shown(&self, track: &Track) -> usize {
        if self.expanded.contains(&track.id) {
            track.automation.len().max(1)
        } else {
            0
        }
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
            return self.lane_hit(project, track, row, x, y, tick);
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
        match &self.drag {
            Some(Drag::Move { .. }) => return Cursor::Move,
            Some(Drag::Trim { .. } | Drag::Fade { .. }) => return Cursor::ResizeHorizontal,
            Some(Drag::Gain { .. }) => return Cursor::ResizeVertical,
            _ => {}
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
            Hit::Automation { point: Some(_), .. } => Cursor::Move,
            Hit::Automation { lane: Some(_), .. } => Cursor::Crosshair,
            _ => Cursor::Arrow,
        }
    }

    /// Whether a drag is in progress (the front end should grab the pointer).
    pub fn dragging(&self) -> bool {
        self.drag.is_some()
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
        Some(Handles {
            fade_in: Rect::new(in_x, r.y, h, h),
            fade_out: Rect::new(out_x - h, r.y, h, h),
            gain: Rect::new(r.x + (r.w - h) / 2.0, r.y, h, h),
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
        let Some(Drag::Gain { clip, .. }) = &self.drag else {
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
        self.relayout(doc.project());
        if let Some(lane) = self.lanes.dragging() {
            let frame = self.lane_frame(doc.project(), lane);
            return self.lanes.drag(doc, frame.as_ref(), p);
        }
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
                ..
            } => {
                let o = self.drag_move(doc, p, tick, row, lead, &clips);
                self.mark_edited(&o);
                o
            }
            Drag::Trim {
                clip,
                zone,
                tick,
                start,
                length,
                offset,
                ..
            } => {
                let o = self.drag_trim(doc, env, p, clip, zone, tick, start, length, offset);
                self.mark_edited(&o);
                o
            }
            Drag::Fade {
                clip, out, grab, ..
            } => {
                let o = self.drag_fade(doc, p, clip, out, grab);
                self.mark_edited(&o);
                o
            }
            Drag::Gain { clip, y, db, .. } => {
                let per_px = if p.shift {
                    FINE_GAIN_DB_PER_PX
                } else {
                    GAIN_DB_PER_PX
                };
                let to = (db + (y - p.y) * per_px).clamp(MIN_CLIP_GAIN_DB, MAX_CLIP_GAIN_DB);
                // Whole hundredths, so the shown value is the stored one.
                let to = (to * 100.0).round() / 100.0;
                let r = doc
                    .execute(SetClip::new(clip, ClipField::AudioGainDb(to)))
                    .map(|_| ());
                let o = Outcome::from_result(r);
                self.mark_edited(&o);
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
            Some(Drag::Move { .. } | Drag::Trim { .. } | Drag::Fade { .. } | Drag::Gain { .. }) => {
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
        }
    }

    /// Drops the drag in progress (Escape, or the pointer was taken away).
    pub fn cancel(&mut self, doc: &mut Document) -> Outcome {
        if let Some(o) = self.lanes.cancel(doc) {
            return o;
        }
        match self.drag.take() {
            Some(
                Drag::Move { edited: true, .. }
                | Drag::Trim { edited: true, .. }
                | Drag::Fade { edited: true, .. }
                | Drag::Gain { edited: true, .. },
            ) => {
                // The drag is one merged undo step: undo it.
                doc.seal();
                let _ = doc.undo();
                Outcome::document()
            }
            Some(_) => Outcome::view(),
            None => Outcome::default(),
        }
    }

    /// Ends the drag in progress, keeping what it did. Call it before any
    /// other edit (a shortcut, undo) while a button may still be held, so the
    /// drag neither merges with that edit nor works on clips it removed.
    pub fn end_drag(&mut self, doc: &mut Document) {
        self.lanes.end_drag(doc);
        if let Some(Drag::Move { .. } | Drag::Trim { .. } | Drag::Fade { .. } | Drag::Gain { .. }) =
            self.drag.take()
        {
            doc.seal();
        }
    }

    fn mark_edited(&mut self, o: &Outcome) {
        if o.document
            && let Some(
                Drag::Move { edited, .. }
                | Drag::Trim { edited, .. }
                | Drag::Fade { edited, .. }
                | Drag::Gain { edited, .. },
            ) = &mut self.drag
        {
            *edited = true;
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
            } => self.lanes.delete_point(doc, lane, i),
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
                    edited: false,
                })
            }
            Hit::Clip {
                clip,
                zone: zone @ (Zone::FadeIn | Zone::FadeOut | Zone::Gain),
                ..
            } => {
                let Some(c) = project.clip(clip) else {
                    self.drag = None;
                    return;
                };
                let ClipContent::Audio(a) = &c.content else {
                    self.drag = None;
                    return;
                };
                let db = a.gain_db;
                let (fi, fo) =
                    daw_model::fit_fades(a.fade_in.length.0, a.fade_out.length.0, c.length.0);
                let edge = if zone == Zone::FadeOut {
                    c.start.0 + c.length.0 - fo
                } else {
                    c.start.0 + fi
                };
                self.selection = BTreeSet::from([clip]);
                doc.seal();
                Some(match zone {
                    Zone::Gain => Drag::Gain {
                        clip,
                        y: start.y,
                        db,
                        edited: false,
                    },
                    _ => Drag::Fade {
                        clip,
                        out: zone == Zone::FadeOut,
                        grab: edge - tick.0,
                        edited: false,
                    },
                })
            }
            Hit::Clip { clip, zone, .. } => {
                // The clip can be gone if a shortcut deleted it while the
                // button was down.
                let Some(c) = project.clip(clip) else {
                    self.drag = None;
                    return;
                };
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
                    edited: false,
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
            Hit::Ruler(_) | Hit::Automation { .. } => None,
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
        if clips
            .iter()
            .any(|&(id, r, _)| r >= rows || project.clip(id).is_none())
        {
            // Tracks or clips went away under the drag.
            self.end_drag(doc);
            return Outcome::default();
        }
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

        let moves: Vec<Box<dyn Command>> = clips
            .iter()
            .map(|&(id, r, start)| {
                let to = (r as i64 + drow) as usize;
                Box::new(PlaceClip::new(
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

    /// Sets a fade's length to reach the pointer, leaving the other fade
    /// room. Not snapped: fades are usually set by ear.
    fn drag_fade(
        &mut self,
        doc: &mut Document,
        p: Pointer,
        id: ClipId,
        out: bool,
        grab: i64,
    ) -> Outcome {
        let Some(clip) = doc.project().clip(id) else {
            self.drag = None;
            return Outcome::default();
        };
        let ClipContent::Audio(a) = &clip.content else {
            return Outcome::default();
        };
        let at = self.view.x_to_tick(p.x).0 + grab;
        let end = clip.start.0 + clip.length.0;
        let (fade, other) = if out {
            (end - at, a.fade_in.length.0)
        } else {
            (at - clip.start.0, a.fade_out.length.0)
        };
        let room = (clip.length.0 - other.min(clip.length.0)).max(0);
        let length = TickTime(fade.clamp(0, room));
        let field = if out {
            ClipField::FadeOut(Fade::new(length, a.fade_out.shape))
        } else {
            ClipField::FadeIn(Fade::new(length, a.fade_in.shape))
        };
        let r = doc.execute(SetClip::new(id, field)).map(|_| ());
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
        let cmd = match zone {
            Zone::End => {
                let mut new_end = snap(TickTime(end + moved.0))
                    .0
                    .max(start.0 + MIN_CLIP_TICKS);
                if let Some(limit) = file_end {
                    new_end = new_end.min(limit.max(start.0 + MIN_CLIP_TICKS));
                }
                TrimClip::new(id, start, TickTime(new_end - start.0), offset)
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
                TrimClip::new(
                    id,
                    TickTime(new_start),
                    TickTime(end - new_start),
                    new_offset,
                )
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

    pub fn select_all(&mut self, project: &Project) -> Outcome {
        self.selection = project
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter().map(|c| c.id))
            .collect();
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
        let r = doc.execute(Group::new(label, ids, items)).map(|_| ());
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
        self.end_drag(doc);
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
        let key = targets.iter().map(|(_, c)| c.id).collect();
        let r = doc.execute(Group::new("Split", key, items)).map(|_| ());
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
        let mut ids = Vec::new();
        let mut items: Vec<Box<dyn Command>> = Vec::new();
        for (_, c) in self.selected_clips(doc.project()) {
            if let ClipContent::Audio(a) = &c.content {
                ids.push(c.id);
                for f in fields(a) {
                    items.push(Box::new(SetClip::new(c.id, f)));
                }
            }
        }
        if items.is_empty() {
            return Outcome::default();
        }
        let r = doc.execute(Group::new(label, ids, items)).map(|_| ());
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
                Box::new(PlaceClip::new(c.id, t.id, TickTime(c.start.0 + delta)))
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
        doc.execute(Group::new("Import Audio", vec![id], items))?;
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
