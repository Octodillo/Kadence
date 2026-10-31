//! The piano roll, without a toolkit.
//!
//! [`PianoRoll`] holds the editor state for one MIDI clip (zoom, scroll,
//! selection, the gesture in progress), turns pointer input and menu actions
//! into `daw-model` commands on a [`Document`], and produces a
//! [`Scene`] that a front end paints, the same display list the arrange view
//! uses.
//!
//! Layout, in logical pixels inside the item: a keyboard strip down the left
//! (`keyboard_width`), a bar ruler across the top (`ruler_height`), the
//! velocity lane along the bottom (`velocity_height`) and the note grid in
//! between. Keys run from 127 at the top to 0 at the bottom.
//!
//! Gestures, with the same modifiers as the arrange view:
//! - click a note to select only it, Ctrl+click toggles it, Shift+click
//!   adds it; drag it to move the selection, drag an edge to resize; Alt
//!   disables snapping;
//! - drag on empty space for a rubber band (Shift or Ctrl adds to the
//!   selection);
//! - double-click empty space to add a note, double-click a note to delete it;
//! - click a key to select every note on it (Shift adds, Ctrl toggles);
//! - drag in the velocity lane to draw velocities;
//! - click the ruler to move the playhead.
//!
//! Pressing a key or a note, and dragging a note up or down, auditions its
//! pitch: [`PianoRoll::audition`] says which note should sound, and the
//! front end plays it through the track's instrument.

mod draw;

use std::collections::BTreeSet;

use daw_arrange::{
    Button, Cursor, Env, Outcome, Palette, Pointer, Rect, Refusal, Scene, Snap, TimelineView,
};
use daw_core::time::{PPQ, TickTime};
use daw_midi::edit::{self, Quantize, Snap as ClipSnap};
use daw_midi::grid::{Grid, Meter};
use daw_midi::model::{self as glue, NoteEdit};
use daw_midi::player::Preview;
use daw_midi::view::{Drag, DragKind, NotePart, PianoRollView, VelocityLane};
use daw_midi::{EditNote, NoteData};
use daw_model::commands::EditNotes;
use daw_model::{Clip, ClipContent, ClipId, Command, Document, MidiClip, Note, NoteUid, Project};

/// Smallest and largest key row height in logical pixels.
pub const MIN_KEY_HEIGHT: f64 = 6.0;
pub const MAX_KEY_HEIGHT: f64 = 48.0;
/// Row height a clip opens at when its notes fit.
pub const DEFAULT_KEY_HEIGHT: f64 = 14.0;

/// A pointer must move this far before a press becomes a drag.
const DRAG_THRESHOLD: f64 = 3.0;
/// Width of a note's resize zones.
const EDGE_PX: f64 = 6.0;
/// How close to a stem a velocity press must be: a target as wide as a
/// fingertip, for touch screens too.
const STEM_TOLERANCE: f64 = 10.0;
/// Space above the tallest velocity stem.
const VELOCITY_INSET: f64 = 4.0;

/// Which part of the item a point is in.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Area {
    Ruler,
    Keyboard,
    Notes,
    Velocity,
    Corner,
}

#[derive(Clone, Debug)]
enum Gesture {
    /// Pressed, not yet moved past the threshold.
    Pending {
        press: Pointer,
        area: Area,
        hit: Option<(NoteUid, NotePart)>,
    },
    /// Moving or resizing the selection by `grabbed`.
    Notes {
        drag: Drag,
        ids: Vec<NoteUid>,
        grabbed: NoteUid,
    },
    /// Holding the pointer down on the keyboard strip.
    Keys,
    /// Rubber-band selection.
    Band {
        x0: f64,
        y0: f64,
        x1: f64,
        y1: f64,
        base: BTreeSet<NoteUid>,
    },
    /// Drawing velocities; `ids` are the notes it may change. `grabbed` is
    /// the note whose stem was pressed: it follows the pointer up and down
    /// until the pointer moves sideways away from it.
    Velocity {
        last: (f64, f64),
        ids: Vec<NoteUid>,
        grabbed: Option<NoteUid>,
    },
}

/// The piano roll's state. See the crate docs.
#[derive(Clone, Debug)]
pub struct PianoRoll {
    /// The clip being edited.
    pub clip: Option<ClipId>,
    pub view: TimelineView,
    /// Pixels from the top of key 127's row to the top of the note grid.
    pub scroll_y: f64,
    pub key_height: f64,
    pub width: f64,
    pub height: f64,
    pub keyboard_width: f64,
    pub ruler_height: f64,
    pub velocity_height: f64,
    pub palette: Palette,
    pub snap: Snap,
    pub selection: BTreeSet<NoteUid>,
    /// Notes of a take being recorded into the open clip, drawn over
    /// the clip's own notes. Set by the front end.
    pub live: Vec<NoteData>,
    /// Velocity of drawn notes, 0 to 1.
    pub new_velocity: f64,
    /// Length of drawn notes in ticks; `None` is one grid step.
    pub new_length: Option<i64>,
    gesture: Option<Gesture>,
    /// The note the pointer is sounding, if any.
    audition: Option<Preview>,
    menu_requested: bool,
    /// Fit the view to the clip's notes once the item has a size.
    fit_pending: bool,
    /// Whether `resize` has given the real item size yet.
    sized: bool,
}

impl Default for PianoRoll {
    fn default() -> Self {
        Self::new()
    }
}

fn outcome_view() -> Outcome {
    Outcome {
        view: true,
        ..Outcome::default()
    }
}

fn outcome_document() -> Outcome {
    Outcome {
        document: true,
        view: true,
        ..Outcome::default()
    }
}

fn outcome_error(e: Refusal) -> Outcome {
    Outcome {
        view: true,
        error: Some(e),
        ..Outcome::default()
    }
}

impl PianoRoll {
    pub fn new() -> Self {
        Self {
            clip: None,
            view: TimelineView {
                px_per_beat: 96.0,
                scroll_x: 0.0,
            },
            scroll_y: 0.0,
            key_height: DEFAULT_KEY_HEIGHT,
            width: 800.0,
            height: 600.0,
            keyboard_width: 56.0,
            ruler_height: 24.0,
            velocity_height: 72.0,
            palette: Palette::default(),
            snap: Snap::Adaptive,
            selection: BTreeSet::new(),
            new_velocity: 0.8,
            new_length: None,
            gesture: None,
            audition: None,
            fit_pending: false,
            sized: false,
            live: Vec::new(),
            menu_requested: false,
        }
    }

    // -----------------------------------------------------------------------
    // Geometry

    /// The note grid.
    pub fn notes_rect(&self) -> Rect {
        Rect::new(
            self.keyboard_width,
            self.ruler_height,
            (self.width - self.keyboard_width).max(0.0),
            (self.height - self.ruler_height - self.velocity_height).max(0.0),
        )
    }

    pub fn velocity_rect(&self) -> Rect {
        let n = self.notes_rect();
        Rect::new(
            n.x,
            n.bottom(),
            n.w,
            self.velocity_height.min(self.height - n.bottom()).max(0.0),
        )
    }

    pub fn area(&self, x: f64, y: f64) -> Area {
        let n = self.notes_rect();
        match (x < n.x, y < n.y, y >= n.bottom()) {
            (true, true, _) | (true, _, true) => Area::Corner,
            (true, false, false) => Area::Keyboard,
            (false, true, _) => Area::Ruler,
            (false, false, true) => Area::Velocity,
            (false, false, false) => Area::Notes,
        }
    }

    fn content_height(&self) -> f64 {
        128.0 * self.key_height
    }

    fn clamp_scroll(&mut self) {
        let max = (self.content_height() - self.notes_rect().h).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, max);
        self.view.scroll_x = self.view.scroll_x.max(0.0);
    }

    /// The note-grid geometry for `clip`, in coordinates relative to the
    /// note grid's top-left corner.
    fn grid_view(&self, clip_start: TickTime) -> PianoRollView {
        PianoRollView {
            left: TickTime((self.view.scroll_x / self.view.px_per_tick()).round() as i64),
            px_per_beat: self.view.px_per_beat,
            scroll_y: self.scroll_y,
            key_height: self.key_height,
            clip_start,
        }
    }

    /// Item coordinates to note-grid coordinates.
    fn local(&self, x: f64, y: f64) -> (f64, f64) {
        let n = self.notes_rect();
        (x - n.x, y - n.y)
    }

    pub fn meter(&self, project: &Project) -> Meter {
        glue::meter(&project.tempo)
    }

    /// The grid edits snap to at this zoom, if any.
    pub fn grid(&self) -> Option<Grid> {
        self.snap.grid(self.view.px_per_tick())
    }

    fn clip<'a>(&self, project: &'a Project) -> Option<(&'a Clip, &'a MidiClip)> {
        let clip = project.clip(self.clip?)?;
        match &clip.content {
            ClipContent::Midi(m) => Some((clip, m)),
            ClipContent::Audio(_) => None,
        }
    }

    /// Whether a MIDI clip is open and still exists.
    pub fn has_clip(&self, project: &Project) -> bool {
        self.clip(project).is_some()
    }

    /// The open clip's name, for the page title.
    pub fn clip_name(&self, project: &Project) -> Option<String> {
        self.clip(project).map(|(c, _)| c.name.clone())
    }

    // -----------------------------------------------------------------------
    // Opening

    /// Opens `clip` for editing, scrolled to its start and its notes.
    pub fn open(&mut self, project: &Project, clip: ClipId) {
        self.clip = Some(clip);
        self.selection.clear();
        self.gesture = None;
        self.audition = None;
        self.live.clear();
        self.fit_pending = true;
        self.fit(project);
    }

    /// Scrolls to the clip start and fits its notes' key range, shrinking
    /// the rows if they do not fit. Waits for a size if there is none yet.
    fn fit(&mut self, project: &Project) {
        let Some((c, m)) = self.clip(project) else {
            self.fit_pending = false;
            return;
        };
        let h = self.notes_rect().h;
        if !self.sized || h <= 0.0 || self.width <= self.keyboard_width {
            return;
        }
        self.fit_pending = false;
        let keys = m.notes.iter().map(|n| n.key);
        let (lo, hi) = match (keys.clone().min(), keys.max()) {
            (Some(lo), Some(hi)) => (f64::from(lo), f64::from(hi)),
            _ => (60.0, 60.0),
        };
        // A row of room above and below.
        let needed = hi - lo + 3.0;
        self.key_height = DEFAULT_KEY_HEIGHT
            .min(h / needed)
            .clamp(MIN_KEY_HEIGHT, MAX_KEY_HEIGHT);
        // A little room before the clip start.
        self.view.scroll_x = (c.start.0 as f64 * self.view.px_per_tick() - 16.0).max(0.0);
        let center = (lo + hi) / 2.0;
        let rows = h / self.key_height;
        self.scroll_y = ((127.5 - center) - rows / 2.0) * self.key_height;
        self.clamp_scroll();
    }

    /// Fits the view to the item size.
    pub fn resize(&mut self, width: f64, height: f64) {
        self.width = width.max(0.0);
        self.height = height.max(0.0);
        self.sized = true;
        self.clamp_scroll();
    }

    /// Finishes fitting a clip that was opened before the item had a size.
    pub fn fit_if_pending(&mut self, project: &Project) {
        if self.fit_pending {
            self.fit(project);
        }
    }

    /// The note the pointer wants to hear: the key or note pressed, at the
    /// key a note has been dragged to. `None` once the pointer is up.
    pub fn audition(&self) -> Option<Preview> {
        self.audition
    }

    fn audition_key(&self, key: u8) -> Preview {
        Preview {
            key,
            channel: 0,
            velocity: self.new_velocity as f32,
        }
    }

    fn audition_note(n: &Note) -> Preview {
        Preview {
            key: n.key,
            channel: n.channel,
            velocity: n.velocity as f32,
        }
    }

    /// Whether the last input asked for a context menu (cleared on read).
    pub fn take_menu_request(&mut self) -> bool {
        std::mem::take(&mut self.menu_requested)
    }

    // -----------------------------------------------------------------------
    // Pointer input

    fn hit(&self, project: &Project, x: f64, y: f64) -> Option<(NoteUid, NotePart)> {
        let (c, m) = self.clip(project)?;
        let (lx, ly) = self.local(x, y);
        let v = self.grid_view(c.start);
        v.hit_test(&m.notes, lx, ly, EDGE_PX)
            .map(|h| (m.notes[h.index].id, h.part))
    }

    /// The pointer shape for a position.
    pub fn cursor(&self, project: &Project, x: f64, y: f64) -> Cursor {
        match self.area(x, y) {
            Area::Ruler => Cursor::Pointing,
            Area::Notes => match self.hit(project, x, y) {
                Some((_, NotePart::Body)) => Cursor::Move,
                Some(_) => Cursor::ResizeHorizontal,
                None => Cursor::Arrow,
            },
            _ => Cursor::Arrow,
        }
    }

    pub fn press(&mut self, doc: &mut Document, env: &Env, p: Pointer) -> Outcome {
        doc.seal();
        self.gesture = None;
        self.audition = None;
        let project = doc.project();
        let area = self.area(p.x, p.y);
        if p.button == Button::Right {
            if area == Area::Notes {
                if let Some((id, _)) = self.hit(project, p.x, p.y)
                    && !self.selection.contains(&id)
                {
                    self.selection = BTreeSet::from([id]);
                }
                self.menu_requested = true;
            }
            return outcome_view();
        }
        if p.button != Button::Left {
            return Outcome::default();
        }
        match area {
            Area::Ruler => {
                let t = self.view.x_to_tick(p.x - self.keyboard_width);
                let t = match self.grid() {
                    Some(g) if !p.alt => g.snap(t, &self.meter(project)),
                    _ => t,
                };
                Outcome {
                    locate: Some(TickTime(t.0.max(0))),
                    ..outcome_view()
                }
            }
            Area::Keyboard => {
                let Some((c, m)) = self.clip(project) else {
                    return Outcome::default();
                };
                let key = self.grid_view(c.start).y_to_key(p.y - self.ruler_height);
                let row: Vec<NoteUid> = m
                    .notes
                    .iter()
                    .filter(|n| n.key == key)
                    .map(|n| n.id)
                    .collect();
                if p.ctrl && !row.is_empty() && row.iter().all(|id| self.selection.contains(id)) {
                    for id in &row {
                        self.selection.remove(id);
                    }
                } else {
                    if !p.shift && !p.ctrl {
                        self.selection.clear();
                    }
                    self.selection.extend(row);
                }
                self.audition = Some(self.audition_key(key));
                self.gesture = Some(Gesture::Keys);
                outcome_view()
            }
            Area::Notes => {
                let hit = self.hit(project, p.x, p.y);
                match hit {
                    Some((id, _)) if p.ctrl => {
                        if !self.selection.remove(&id) {
                            self.selection.insert(id);
                        }
                    }
                    Some((id, _)) if p.shift => {
                        self.selection.insert(id);
                    }
                    Some((id, _)) if !self.selection.contains(&id) => {
                        self.selection = BTreeSet::from([id]);
                    }
                    Some(_) => {}
                    None if !p.shift && !p.ctrl => self.selection.clear(),
                    None => {}
                }
                if let Some((id, _)) = hit
                    && let Some(n) = self.notes(project).iter().find(|n| n.id == id)
                {
                    self.audition = Some(Self::audition_note(n));
                }
                self.gesture = Some(Gesture::Pending {
                    press: p,
                    area,
                    hit,
                });
                outcome_view()
            }
            Area::Velocity => {
                let Some((c, m)) = self.clip(project) else {
                    return Outcome::default();
                };
                let mut ids: Vec<NoteUid> = m
                    .notes
                    .iter()
                    .filter(|n| self.selection.contains(&n.id))
                    .map(|n| n.id)
                    .collect();
                if ids.is_empty() {
                    ids = m.notes.iter().map(|n| n.id).collect();
                }
                let (lx, _) = self.local(p.x, p.y);
                let (top, lane) = self.velocity_lane();
                let ly = p.y - top;
                let v = self.grid_view(c.start);
                let targets: Vec<Note> = m
                    .notes
                    .iter()
                    .filter(|n| ids.contains(&n.id))
                    .cloned()
                    .collect();
                let hit = lane.hit_test(&v, &targets, lx, STEM_TOLERANCE);
                let out = match hit {
                    Some(i) => {
                        // Every note on the stem: a chord's stems coincide.
                        let x = v.note_rect(&targets[i].data()).x;
                        let mut all = targets.clone();
                        for n in &mut all {
                            if v.note_rect(&n.data()).x == x {
                                n.velocity = lane.y_to_velocity(ly);
                            }
                        }
                        self.execute_edit(doc, all)
                    }
                    None => outcome_view(),
                };
                self.gesture = Some(Gesture::Velocity {
                    last: (lx, ly),
                    ids,
                    grabbed: hit.map(|i| targets[i].id),
                });
                let _ = env;
                out
            }
            Area::Corner => Outcome::default(),
        }
    }

    pub fn motion(&mut self, doc: &mut Document, p: Pointer) -> Outcome {
        let Some(g) = self.gesture.clone() else {
            return Outcome::default();
        };
        match g {
            Gesture::Pending { press, area, hit } => {
                if (p.x - press.x).hypot(p.y - press.y) < DRAG_THRESHOLD {
                    return Outcome::default();
                }
                if area != Area::Notes {
                    return Outcome::default();
                }
                let project = doc.project();
                match hit {
                    Some((id, part)) => {
                        let Some((c, m)) = self.clip(project) else {
                            return Outcome::default();
                        };
                        let sel: Vec<Note> = m
                            .notes
                            .iter()
                            .filter(|n| self.selection.contains(&n.id))
                            .cloned()
                            .collect();
                        let Some(grabbed) = sel.iter().find(|n| n.id == id) else {
                            return Outcome::default();
                        };
                        let (lx, ly) = self.local(press.x, press.y);
                        let drag = Drag::begin(
                            DragKind::from(part),
                            &self.grid_view(c.start),
                            lx,
                            ly,
                            grabbed,
                            &sel,
                        );
                        self.gesture = Some(Gesture::Notes {
                            drag,
                            ids: sel.iter().map(|n| n.id).collect(),
                            grabbed: id,
                        });
                    }
                    None => {
                        self.gesture = Some(Gesture::Band {
                            x0: press.x,
                            y0: press.y,
                            x1: press.x,
                            y1: press.y,
                            base: self.selection.clone(),
                        });
                    }
                }
                self.motion(doc, p)
            }
            Gesture::Keys => {
                let Some((c, _)) = self.clip(doc.project()) else {
                    return Outcome::default();
                };
                // Sliding along the keys plays each one, glissando.
                let key = self.grid_view(c.start).y_to_key(p.y - self.ruler_height);
                if self.audition.is_none_or(|a| a.key != key) {
                    self.audition = Some(self.audition_key(key));
                    return outcome_view();
                }
                Outcome::default()
            }
            Gesture::Notes { drag, ids, grabbed } => {
                let project = doc.project();
                let Some((c, m)) = self.clip(project) else {
                    return Outcome::default();
                };
                let meter = self.meter(project);
                let grid = self.grid();
                let snap = match (&grid, p.alt) {
                    (Some(g), false) => Some(ClipSnap::new(g, &meter, c.start)),
                    _ => None,
                };
                let v = self.grid_view(c.start);
                let (lx, ly) = self.local(p.x, p.y);
                let data = drag.update(&v, lx, ly, snap.as_ref());
                let mut notes: Vec<Note> = Vec::with_capacity(ids.len());
                for (id, d) in ids.iter().zip(data) {
                    if let Some(n) = m.notes.iter().find(|n| n.id == *id) {
                        let mut n = n.clone();
                        n.set_data(d);
                        notes.push(n);
                    }
                }
                // Follow the grabbed note to its new key; moving in time
                // alone keeps the note sounding as it is.
                if let Some(n) = notes.iter().find(|n| n.id == grabbed)
                    && self.audition.is_some_and(|a| a.key != n.key)
                {
                    self.audition = Some(Self::audition_note(n));
                }
                self.execute_edit(doc, notes)
            }
            Gesture::Band { x0, y0, base, .. } => {
                self.gesture = Some(Gesture::Band {
                    x0,
                    y0,
                    x1: p.x,
                    y1: p.y,
                    base: base.clone(),
                });
                let project = doc.project();
                let Some((c, m)) = self.clip(project) else {
                    return outcome_view();
                };
                let (ax, ay) = self.local(x0, y0);
                let (bx, by) = self.local(p.x, p.y);
                let band = daw_midi::view::Rect::from_corners(ax, ay, bx, by);
                let v = self.grid_view(c.start);
                self.selection = base;
                self.selection.extend(
                    v.notes_in_rect(&m.notes, &band)
                        .into_iter()
                        .map(|i| m.notes[i].id),
                );
                outcome_view()
            }
            Gesture::Velocity { last, ids, grabbed } => {
                let project = doc.project();
                let Some((c, m)) = self.clip(project) else {
                    return Outcome::default();
                };
                let (lx, _) = self.local(p.x, p.y);
                let (top, lane) = self.velocity_lane();
                let ly = p.y - top;
                let v = self.grid_view(c.start);
                let mut targets: Vec<Note> = m
                    .notes
                    .iter()
                    .filter(|n| ids.contains(&n.id))
                    .cloned()
                    .collect();
                lane.draw_line(&v, &mut targets, last, (lx, ly));
                // The pressed stem follows the pointer even when it is not
                // exactly over it, while it stays within reach.
                // The whole stem moves, as for a chord.
                let stem = grabbed
                    .and_then(|id| targets.iter().find(|n| n.id == id))
                    .map(|n| v.note_rect(&n.data()).x)
                    .filter(|x| (x - lx).abs() <= STEM_TOLERANCE);
                if let Some(x) = stem {
                    for n in &mut targets {
                        if v.note_rect(&n.data()).x == x {
                            n.velocity = lane.y_to_velocity(ly);
                        }
                    }
                }
                let grabbed = grabbed.filter(|_| stem.is_some());
                self.gesture = Some(Gesture::Velocity {
                    last: (lx, ly),
                    ids,
                    grabbed,
                });
                self.execute_edit(doc, targets)
            }
        }
    }

    pub fn release(&mut self, doc: &mut Document, p: Pointer) -> Outcome {
        self.audition = None;
        let out = match self.gesture.take() {
            Some(Gesture::Band { .. }) => outcome_view(),
            // A click (no drag) on a note of a larger selection selects
            // only it, as in the arrange view.
            Some(Gesture::Pending {
                press,
                hit: Some((id, _)),
                ..
            }) if !press.shift && !press.ctrl && self.selection.len() > 1 => {
                self.selection = BTreeSet::from([id]);
                outcome_view()
            }
            // Redraw the key as released.
            Some(Gesture::Keys) => outcome_view(),
            Some(Gesture::Pending { .. }) | None => Outcome::default(),
            Some(g @ (Gesture::Notes { .. } | Gesture::Velocity { .. })) => {
                // Apply the final position, then close the undo step.
                self.gesture = Some(g);
                let out = self.motion(doc, p);
                self.gesture = None;
                out
            }
        };
        doc.seal();
        out
    }

    /// Escape: ends the gesture. A drag already applied stays (one undo
    /// step), like the arrange view.
    pub fn cancel(&mut self, doc: &mut Document) -> Outcome {
        self.gesture = None;
        self.audition = None;
        doc.seal();
        outcome_view()
    }

    pub fn double_click(&mut self, doc: &mut Document, p: Pointer) -> Outcome {
        if self.area(p.x, p.y) != Area::Notes {
            return Outcome::default();
        }
        self.gesture = None;
        // The second press auditioned the note this may delete.
        self.audition = None;
        let project = doc.project();
        let Some((c, _)) = self.clip(project) else {
            return Outcome::default();
        };
        let clip_id = c.id;
        if let Some((id, _)) = self.hit(project, p.x, p.y) {
            self.selection.remove(&id);
            return self.execute(doc, glue::delete_selection(clip_id, &[id]));
        }
        let meter = self.meter(project);
        let grid = self.grid();
        let snap = match (&grid, p.alt) {
            (Some(g), false) => Some(ClipSnap::new(g, &meter, c.start)),
            _ => None,
        };
        let (lx, ly) = self.local(p.x, p.y);
        let note = self.grid_view(c.start).new_note(
            lx,
            ly,
            snap.as_ref(),
            self.new_length,
            self.new_velocity,
        );
        if note.start >= c.length {
            return outcome_error(Refusal::NoteOutsideClip);
        }
        let e = NoteEdit::new(clip_id, "Add Note").add_notes([note]);
        let ids = e.added_ids();
        let out = self.execute(doc, e.into_command());
        self.selection = ids.into_iter().collect();
        out
    }

    /// Mouse wheel or touchpad scroll, as in the arrange view: Ctrl zooms
    /// time, Ctrl+Shift zooms keys, Shift scrolls sideways.
    pub fn wheel(&mut self, p: Pointer, dx: f64, dy: f64) -> Outcome {
        let ax = p.x - self.keyboard_width;
        if p.ctrl && p.shift {
            self.zoom_keys((dy / 240.0).exp2(), p.y);
        } else if p.ctrl {
            self.view.zoom_at((dy / 240.0).exp2(), ax);
        } else if p.shift {
            self.view.scroll_by(-(dx + dy));
        } else {
            self.view.scroll_by(-dx);
            self.scroll_y -= dy;
        }
        self.clamp_scroll();
        outcome_view()
    }

    pub fn pinch(&mut self, scale: f64, x: f64) -> Outcome {
        self.view.zoom_at(scale, x - self.keyboard_width);
        self.clamp_scroll();
        outcome_view()
    }

    pub fn zoom_keys(&mut self, factor: f64, anchor_y: f64) {
        let content_y = anchor_y - self.ruler_height + self.scroll_y;
        let rows = content_y / self.key_height;
        self.key_height = (self.key_height * factor).clamp(MIN_KEY_HEIGHT, MAX_KEY_HEIGHT);
        self.scroll_y = rows * self.key_height - (anchor_y - self.ruler_height);
        self.clamp_scroll();
    }

    // -----------------------------------------------------------------------
    // Commands

    fn execute(&mut self, doc: &mut Document, cmd: Option<Box<dyn Command>>) -> Outcome {
        match cmd {
            None => outcome_view(),
            Some(c) => {
                let out = match doc.execute_boxed(c) {
                    Ok(_) => outcome_document(),
                    Err(e) => outcome_error(Refusal::Edit(e)),
                };
                self.sync(doc.project());
                out
            }
        }
    }

    /// Drops selected IDs whose notes are gone (after an undo, a glue or a
    /// deleted clip). The front end calls it before reading the selection.
    pub fn sync(&mut self, project: &Project) {
        match self.clip(project) {
            Some((_, m)) => self
                .selection
                .retain(|s| m.notes.iter().any(|n| n.id == *s)),
            None => self.selection.clear(),
        }
    }

    /// The top of the velocity lane's drawing area and its geometry, shared
    /// by drawing and input so a click lands where the stem is drawn.
    pub(crate) fn velocity_lane(&self) -> (f64, VelocityLane) {
        let vel = self.velocity_rect();
        let inset = VELOCITY_INSET.min(vel.h);
        (
            vel.y + inset,
            VelocityLane {
                height: vel.h - inset,
            },
        )
    }

    /// Applies new values for existing notes as an [`EditNotes`] with a
    /// fixed set of IDs, so the moves of one drag merge into one undo step.
    fn execute_edit(&mut self, doc: &mut Document, notes: Vec<Note>) -> Outcome {
        let Some(clip) = self.clip else {
            return Outcome::default();
        };
        let Some((_, m)) = self.clip(doc.project()) else {
            return Outcome::default();
        };
        let unchanged = notes.iter().all(|n| m.notes.iter().any(|o| o == n));
        if notes.is_empty() || unchanged {
            return Outcome::default();
        }
        self.execute(doc, Some(Box::new(EditNotes { clip, notes })))
    }

    fn selected(&self, project: &Project) -> Option<(ClipId, MidiClip, Vec<NoteUid>, Clip)> {
        let (c, m) = self.clip(project)?;
        let ids: Vec<NoteUid> = self
            .selection
            .iter()
            .copied()
            .filter(|s| m.notes.iter().any(|n| n.id == *s))
            .collect();
        Some((c.id, m.clone(), ids, (*c).clone()))
    }

    /// Runs an in-place edit on the selection.
    fn edit_selected(
        &mut self,
        doc: &mut Document,
        label: &str,
        op: impl FnOnce(&mut [Note]),
    ) -> Outcome {
        let Some((id, m, sel, _)) = self.selected(doc.project()) else {
            return Outcome::default();
        };
        doc.seal();
        let out = self.execute(doc, glue::edit_selection(id, &m, &sel, label, op));
        doc.seal();
        out
    }

    pub fn select_all(&mut self, project: &Project) -> Outcome {
        if let Some((_, m)) = self.clip(project) {
            self.selection = m.notes.iter().map(|n| n.id).collect();
        }
        outcome_view()
    }

    pub fn select_none(&mut self) -> Outcome {
        self.selection.clear();
        outcome_view()
    }

    pub fn delete_selection(&mut self, doc: &mut Document) -> Outcome {
        let Some((id, _, sel, _)) = self.selected(doc.project()) else {
            return Outcome::default();
        };
        self.selection.clear();
        self.execute(doc, glue::delete_selection(id, &sel))
    }

    pub fn quantize(&mut self, doc: &mut Document, q: Quantize) -> Outcome {
        let Some((_, _, _, clip)) = self.selected(doc.project()) else {
            return Outcome::default();
        };
        let meter = self.meter(doc.project());
        let grid = self.grid().unwrap_or_else(|| Grid::note(16));
        let snap = ClipSnap::new(&grid, &meter, clip.start);
        self.edit_selected(doc, "Quantize", |n| edit::quantize(n, &snap, q))
    }

    pub fn transpose(&mut self, doc: &mut Document, semitones: i32) -> Outcome {
        self.edit_selected(doc, "Transpose", |n| {
            edit::transpose(n, semitones);
        })
    }

    /// Moves the selection one grid step (a beat with snapping off) left
    /// for `steps < 0` or right.
    pub fn nudge(&mut self, doc: &mut Document, steps: i32) -> Outcome {
        let Some((_, m, sel, clip)) = self.selected(doc.project()) else {
            return Outcome::default();
        };
        let first = glue::selected_notes(&m, &sel)
            .iter()
            .map(|n| n.start)
            .min()
            .unwrap_or_default();
        let meter = self.meter(doc.project());
        let step = match self.grid() {
            Some(g) => g.step_at(TickTime(clip.start.0 + first.0), &meter),
            None => PPQ,
        };
        self.edit_selected(doc, "Nudge", |n| {
            edit::nudge(n, step * i64::from(steps));
        })
    }

    pub fn set_velocity(&mut self, doc: &mut Document, velocity: f64) -> Outcome {
        self.edit_selected(doc, "Set Velocity", |n| edit::set_velocity(n, velocity))
    }

    pub fn scale_velocity(&mut self, doc: &mut Document, factor: f64) -> Outcome {
        self.edit_selected(doc, "Scale Velocity", |n| edit::scale_velocity(n, factor))
    }

    pub fn legato(&mut self, doc: &mut Document) -> Outcome {
        self.edit_selected(doc, "Legato", edit::legato)
    }

    pub fn humanize(&mut self, doc: &mut Document, seed: u64) -> Outcome {
        self.edit_selected(doc, "Humanize", |n| edit::humanize(n, PPQ / 32, 0.08, seed))
    }

    pub fn reverse(&mut self, doc: &mut Document) -> Outcome {
        self.edit_selected(doc, "Reverse", edit::reverse)
    }

    pub fn glue(&mut self, doc: &mut Document) -> Outcome {
        let Some((id, m, sel, _)) = self.selected(doc.project()) else {
            return Outcome::default();
        };
        self.execute(doc, glue::glue_selection(id, &m, &sel, 0))
    }

    pub fn fix_overlaps(&mut self, doc: &mut Document) -> Outcome {
        let Some((id, m, sel, _)) = self.selected(doc.project()) else {
            return Outcome::default();
        };
        self.execute(doc, glue::trim_overlaps_selection(id, &m, &sel))
    }

    /// Splits selected notes at the playhead.
    pub fn split_at(&mut self, doc: &mut Document, at: TickTime) -> Outcome {
        let Some((id, m, sel, clip)) = self.selected(doc.project()) else {
            return Outcome::default();
        };
        let rel = TickTime(at.0 - clip.start.0);
        self.execute(doc, glue::split_selection(id, &m, &sel, rel))
    }

    /// Duplicates the selection right after itself and selects the copies.
    pub fn duplicate(&mut self, doc: &mut Document) -> Outcome {
        let Some((id, m, sel, clip)) = self.selected(doc.project()) else {
            return Outcome::default();
        };
        let notes = glue::selected_notes(&m, &sel);
        let meter = self.meter(doc.project());
        let grid = self.grid();
        let snap = grid.as_ref().map(|g| ClipSnap::new(g, &meter, clip.start));
        let offset = edit::duplicate_offset(&notes, snap.as_ref());
        match glue::duplicate_selection(id, &m, &sel, offset) {
            Some((cmd, ids)) => {
                let out = self.execute(doc, Some(cmd));
                self.selection = ids.into_iter().collect();
                out
            }
            None => outcome_view(),
        }
    }

    /// Notes of the open clip, for tests and the front end.
    pub fn notes<'a>(&self, project: &'a Project) -> &'a [Note] {
        self.clip(project).map_or(&[], |(_, m)| &m.notes)
    }

    /// The selected notes' data, for the front end's status line.
    pub fn selected_data(&self, project: &Project) -> Vec<NoteData> {
        self.notes(project)
            .iter()
            .filter(|n| self.selection.contains(&n.id))
            .map(|n| n.data())
            .collect()
    }

    // -----------------------------------------------------------------------
    // Drawing

    /// The picture for the current state.
    pub fn scene(&self, project: &Project, env: &Env) -> Scene {
        draw::scene(self, project, env)
    }

    fn band(&self) -> Option<Rect> {
        match &self.gesture {
            Some(Gesture::Band { x0, y0, x1, y1, .. }) => Some(Rect::spanning(*x0, *y0, *x1, *y1)),
            _ => None,
        }
    }
}

/// Note names with C4 as middle C (key 60), as KDE and most DAWs show them.
pub fn note_name(key: u8) -> String {
    const NAMES: [&str; 12] = [
        "C", "C♯", "D", "D♯", "E", "F", "F♯", "G", "G♯", "A", "A♯", "B",
    ];
    format!("{}{}", NAMES[key as usize % 12], key as i32 / 12 - 1)
}

/// Whether `key` is a black key.
pub fn is_black(key: u8) -> bool {
    matches!(key % 12, 1 | 3 | 6 | 8 | 10)
}
