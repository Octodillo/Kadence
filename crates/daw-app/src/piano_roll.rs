//! `PianoRoll`: the painted note editor on the Edit page, a custom
//! QQuickPaintedItem built like [`ArrangeView`](crate::arrange_view). Input
//! from QML goes to `daw_pianoroll::PianoRoll`; the display list it returns
//! is painted by the same C++ painter.
//!
//! The scene is built on the GUI thread (in the invokables). `paint` runs on
//! the render thread while the GUI thread waits, and only reads the copy
//! stored in this object.

use std::pin::Pin;

use cxx_qt::CxxQtType;
use cxx_qt_lib::{QColor, QList, QString, QVariant};
use daw_arrange::Input;
use daw_midi::edit::{Quantize, QuantizeEnds};
use daw_pianoroll::{Accidentals, PianoRoll as Roll};

use crate::automation_bridge::qobject::PointShape;
use crate::automation_bridge::{shape, shape_index};
use crate::paint::{self, DisplayList, cursor_shape, pointer, route, to_qcolor};
use crate::session_mixer::strip_row;
use crate::state;
use crate::tracks::HeaderRow;
use crate::tracks_bridge::row_list;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!(<QtQuick/QQuickPaintedItem>);
        type QQuickPaintedItem;
        include!("cxx-qt-lib/qpainter.h");
        type QPainter = cxx_qt_lib::QPainter;
        include!("cxx-qt-lib/qcolor.h");
        type QColor = cxx_qt_lib::QColor;
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qlist.h");
        type QList_QVariant = cxx_qt_lib::QList<cxx_qt_lib::QVariant>;
    }

    #[auto_cxx_name]
    unsafe extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[base = QQuickPaintedItem]
        #[qproperty(QColor, background_color)]
        #[qproperty(QColor, alternate_background_color)]
        #[qproperty(QColor, text_color)]
        #[qproperty(QColor, disabled_text_color)]
        #[qproperty(QColor, highlight_color)]
        #[qproperty(QColor, highlighted_text_color)]
        #[qproperty(QColor, negative_color)]
        #[qproperty(f64, ruler_height)]
        #[qproperty(f64, keyboard_width)]
        #[qproperty(f64, velocity_height)]
        /// Whether the clip's track's automation lanes show, and the
        /// height of each (set by QML).
        #[qproperty(bool, lanes_shown)]
        #[qproperty(f64, lane_height)]
        /// Whether note names spell the black keys as flats (set by QML).
        #[qproperty(bool, flat_note_names)]
        /// The height the lane rows take, and the clip's track's row in
        /// the project and its number of lanes, for the lane headers.
        #[qproperty(f64, lanes_height)]
        #[qproperty(i32, track_index)]
        #[qproperty(i32, lane_count)]
        /// Those lanes as the `lanes` role of `Tracks` describes them, for
        /// their labels.
        #[qproperty(QList_QVariant, lanes)]
        /// Scroll offsets and content size, for the scroll bars.
        #[qproperty(f64, scroll_x)]
        #[qproperty(f64, scroll_y)]
        #[qproperty(f64, content_height)]
        /// Whether a MIDI clip is open.
        #[qproperty(bool, has_clip)]
        #[qproperty(QString, clip_name)]
        /// The `Mixer` row of the open clip's track; -1 without a clip.
        #[qproperty(i32, strip_row)]
        #[qproperty(bool, has_selection)]
        /// Automation points are selected (Delete deletes them first).
        #[qproperty(bool, has_points)]
        /// A Qt::CursorShape for the pointer's position.
        #[qproperty(i32, cursor_shape)]
        type PianoRoll = super::PianoRollRust;

        #[cxx_override]
        unsafe fn paint(self: Pin<&mut PianoRoll>, painter: *mut QPainter);
        /// Builds the picture, if it is stale and the roll shows: once a
        /// frame at most, however many changes there were.
        #[cxx_override]
        #[cxx_name = "updatePolish"]
        fn update_polish(self: Pin<&mut PianoRoll>);

        /// Shows the current state: the properties at once, the picture on
        /// the next frame.
        #[qinvokable]
        fn refresh(self: Pin<&mut PianoRoll>);

        #[qinvokable]
        fn press(self: Pin<&mut PianoRoll>, x: f64, y: f64, button: i32, modifiers: i32);
        #[qinvokable]
        fn motion(self: Pin<&mut PianoRoll>, x: f64, y: f64, modifiers: i32);
        #[qinvokable]
        fn release(self: Pin<&mut PianoRoll>, x: f64, y: f64, modifiers: i32);
        #[qinvokable]
        fn double_click(self: Pin<&mut PianoRoll>, x: f64, y: f64, modifiers: i32);
        #[qinvokable]
        fn cancel_drag(self: Pin<&mut PianoRoll>);
        #[qinvokable]
        fn hover(self: Pin<&mut PianoRoll>, x: f64, y: f64);
        #[qinvokable]
        fn wheel(self: Pin<&mut PianoRoll>, x: f64, y: f64, dx: f64, dy: f64, modifiers: i32);
        #[qinvokable]
        fn pinch(self: Pin<&mut PianoRoll>, scale: f64, x: f64);
        /// Whether a one-finger touch drag from here should scroll the view
        /// instead of editing (see `PianoRoll::touch_pans`).
        #[qinvokable]
        fn touch_pans(self: &PianoRoll, x: f64, y: f64) -> bool;
        /// Whether a right click here opens a context menu, so that a
        /// held finger should (see `PianoRoll::has_menu_at`).
        #[qinvokable]
        fn has_menu_at(self: &PianoRoll, x: f64, y: f64) -> bool;
        #[qinvokable]
        fn scroll_to(self: Pin<&mut PianoRoll>, x: f64, y: f64);

        /// Editing actions on the selection.
        #[qinvokable]
        fn quantize(self: Pin<&mut PianoRoll>, strength: f64, ends: bool);
        #[qinvokable]
        fn transpose(self: Pin<&mut PianoRoll>, semitones: i32);
        #[qinvokable]
        fn nudge(self: Pin<&mut PianoRoll>, steps: i32);
        #[qinvokable]
        fn set_velocity(self: Pin<&mut PianoRoll>, velocity: f64);
        #[qinvokable]
        fn scale_velocity(self: Pin<&mut PianoRoll>, factor: f64);
        #[qinvokable]
        fn legato(self: Pin<&mut PianoRoll>);
        #[qinvokable]
        fn glue(self: Pin<&mut PianoRoll>);
        #[qinvokable]
        fn fix_overlaps(self: Pin<&mut PianoRoll>);
        #[qinvokable]
        fn humanize(self: Pin<&mut PianoRoll>);
        #[qinvokable]
        fn reverse(self: Pin<&mut PianoRoll>);
        #[qinvokable]
        fn duplicate(self: Pin<&mut PianoRoll>);
        #[qinvokable]
        fn delete_selection(self: Pin<&mut PianoRoll>);
        #[qinvokable]
        fn split_at_playhead(self: Pin<&mut PianoRoll>);
        #[qinvokable]
        fn select_all(self: Pin<&mut PianoRoll>);
        #[qinvokable]
        fn zoom(self: Pin<&mut PianoRoll>, factor: f64);
        /// The selected automation points' shape, an
        /// `Automation.PointShape`, or -1.
        #[qinvokable]
        fn point_shape(self: &PianoRoll) -> i32;
        /// Gives the selected automation points an `Automation.PointShape`
        /// (an int here: the enum's type belongs to `Automation`).
        #[qinvokable]
        fn set_point_shape(self: Pin<&mut PianoRoll>, shape: i32);
        /// Deletes the selected automation points (never notes).
        #[qinvokable]
        fn delete_points(self: Pin<&mut PianoRoll>);

        /// A context menu was asked for over a note.
        #[qsignal]
        fn context_menu_requested(self: Pin<&mut PianoRoll>, x: f64, y: f64);
        /// A context menu was asked for over an automation lane, on a
        /// point or between points.
        #[qsignal]
        fn lane_menu_requested(self: Pin<&mut PianoRoll>, x: f64, y: f64, on_point: bool);
    }

    unsafe extern "RustQt" {
        #[inherit]
        fn update(self: Pin<&mut PianoRoll>);
        #[inherit]
        fn polish(self: Pin<&mut PianoRoll>);
        /// The item was shown or hidden (itself or with what holds it).
        #[inherit]
        #[qsignal]
        #[cxx_name = "visibleChanged"]
        fn visible_changed(self: Pin<&mut PianoRoll>);
        #[inherit]
        #[cxx_name = "isVisible"]
        fn is_visible(self: &PianoRoll) -> bool;
        #[inherit]
        fn width(self: &PianoRoll) -> f64;
        #[inherit]
        fn height(self: &PianoRoll) -> f64;
    }

    impl cxx_qt::Constructor<()> for PianoRoll {}
}

pub struct PianoRollRust {
    background_color: QColor,
    alternate_background_color: QColor,
    text_color: QColor,
    disabled_text_color: QColor,
    highlight_color: QColor,
    highlighted_text_color: QColor,
    negative_color: QColor,
    ruler_height: f64,
    keyboard_width: f64,
    velocity_height: f64,
    lanes_shown: bool,
    lane_height: f64,
    flat_note_names: bool,
    lanes_height: f64,
    track_index: i32,
    lane_count: i32,
    lanes: QList<QVariant>,
    /// What `lanes` was made from.
    lane_rows: Vec<HeaderRow>,
    /// The view revision and track row `lane_rows` were read at: they are
    /// read again only when either moves, not on every playing frame.
    lanes_at: Option<(u64, Option<usize>)>,
    scroll_x: f64,
    scroll_y: f64,
    content_height: f64,
    has_clip: bool,
    clip_name: QString,
    strip_row: i32,
    has_selection: bool,
    has_points: bool,
    cursor_shape: i32,
    scene: DisplayList,
    last_press: (f64, f64),
}

impl Default for PianoRollRust {
    fn default() -> Self {
        let p = daw_arrange::Palette::default();
        let d = daw_pianoroll::PianoRoll::new();
        Self {
            background_color: to_qcolor(p.background),
            alternate_background_color: to_qcolor(p.alternate_background),
            text_color: to_qcolor(p.text),
            disabled_text_color: to_qcolor(p.disabled_text),
            highlight_color: to_qcolor(p.highlight),
            highlighted_text_color: to_qcolor(p.highlighted_text),
            negative_color: to_qcolor(p.negative),
            ruler_height: d.ruler_height,
            keyboard_width: d.keyboard_width,
            velocity_height: d.velocity_height,
            lanes_shown: d.lanes_shown,
            lane_height: d.lane_height,
            flat_note_names: d.accidentals == Accidentals::Flats,
            lanes_height: 0.0,
            track_index: -1,
            lane_count: 0,
            lanes: QList::default(),
            lane_rows: Vec::new(),
            lanes_at: None,
            scroll_x: 0.0,
            scroll_y: 0.0,
            content_height: 0.0,
            has_clip: false,
            clip_name: QString::default(),
            strip_row: -1,
            has_selection: false,
            has_points: false,
            cursor_shape: 0,
            scene: DisplayList::default(),
            last_press: (0.0, 0.0),
        }
    }
}

impl cxx_qt::Constructor<()> for qobject::PianoRoll {
    type NewArguments = ();
    type BaseArguments = ();
    type InitializeArguments = ();

    fn route_arguments(_: ()) -> ((), (), ()) {
        ((), (), ())
    }

    fn new(_: ()) -> PianoRollRust {
        PianoRollRust::default()
    }

    fn initialize(self: Pin<&mut Self>, _: ()) {
        // A hidden item keeps a stale picture (see `update_polish`): draw
        // it afresh when the item shows again.
        self.on_visible_changed(|item| item.polish()).release();
    }
}

impl qobject::PianoRoll {
    /// # Safety
    /// Called by Qt with a valid painter for the duration of the call.
    unsafe fn paint(self: Pin<&mut Self>, painter: *mut cxx_qt_lib::QPainter) {
        // SAFETY: Qt passes a live, active painter.
        unsafe { self.scene.paint(painter) };
    }

    fn refresh(mut self: Pin<&mut Self>) {
        // What the properties show, read while the state is borrowed.
        struct Shown {
            scroll_x: f64,
            scroll_y: f64,
            content_height: f64,
            has_clip: bool,
            clip_name: String,
            has_selection: bool,
            has_points: bool,
            strip_row: i32,
            lanes_height: f64,
            track_index: i32,
            lane_count: i32,
            /// The lane rows, when something they show may have changed.
            lanes: Option<Vec<HeaderRow>>,
            lanes_at: (u64, Option<usize>),
        }
        let (w, h) = (self.width(), self.height());
        let palette = paint::theme_palette!(self);
        let (rh, kw, vh) = (self.ruler_height, self.keyboard_width, self.velocity_height);
        let (lanes_shown, lh) = (self.lanes_shown, self.lane_height);
        let accidentals = if self.flat_note_names {
            Accidentals::Flats
        } else {
            Accidentals::Sharps
        };
        let lanes_seen = self.lanes_at;
        let shown = state::with(|s| {
            s.piano.palette = palette;
            s.piano.ruler_height = rh;
            s.piano.keyboard_width = kw;
            s.piano.velocity_height = vh;
            s.piano.lanes_shown = lanes_shown;
            s.piano.lane_height = lh;
            s.piano.accidentals = accidentals;
            s.piano.resize(w, h);
            s.piano.sync(s.doc.project());
            s.piano.fit_if_pending(s.doc.project());
            let p = &s.piano;
            let project = s.doc.project();
            let track = p.track(project);
            let row = p.track_row(project);
            let lanes_at = (s.view_revision, row);
            Shown {
                scroll_x: p.view.scroll_x,
                scroll_y: p.scroll_y,
                content_height: 128.0 * p.key_height,
                has_clip: p.has_clip(project),
                clip_name: p.clip_name(project).unwrap_or_default(),
                has_selection: !p.selection.is_empty(),
                has_points: p.lanes.points_selected(project),
                strip_row: track
                    .and_then(|t| strip_row(project, t))
                    .map_or(-1, |row| row as i32),
                lanes_height: p.lanes_height(),
                track_index: row.map_or(-1, |i| i as i32),
                lane_count: p.lane_count() as i32,
                lanes: (lanes_seen != Some(lanes_at))
                    .then(|| row.map(|row| s.lane_rows(row)).unwrap_or_default()),
                lanes_at,
            }
        });
        self.as_mut().set_scroll_x(shown.scroll_x);
        self.as_mut().set_scroll_y(shown.scroll_y);
        self.as_mut().set_content_height(shown.content_height);
        self.as_mut().set_has_clip(shown.has_clip);
        self.as_mut()
            .set_clip_name(QString::from(shown.clip_name.as_str()));
        self.as_mut().set_has_selection(shown.has_selection);
        self.as_mut().set_has_points(shown.has_points);
        self.as_mut().set_strip_row(shown.strip_row);
        self.as_mut().set_lanes_height(shown.lanes_height);
        self.as_mut().set_track_index(shown.track_index);
        self.as_mut().set_lane_count(shown.lane_count);
        if let Some(lanes) = shown.lanes {
            self.as_mut().rust_mut().lanes_at = Some(shown.lanes_at);
            if lanes != self.lane_rows {
                self.as_mut().set_lanes(row_list(&lanes));
                self.as_mut().rust_mut().lane_rows = lanes;
            }
        }
        self.as_mut().rust_mut().scene.invalidate();
        self.polish();
    }

    fn update_polish(mut self: Pin<&mut Self>) {
        if !self.scene.wants_scene(self.is_visible()) {
            return;
        }
        let scene =
            state::with(|s| s.with_view(|p: &mut Roll, doc, env| p.scene(doc.project(), env)));
        self.as_mut().rust_mut().scene.store(scene);
        self.update();
    }

    /// Applies an outcome, then redraws and tells QML.
    fn after(mut self: Pin<&mut Self>, out: daw_arrange::Outcome) {
        let any = out.document || out.view || out.locate.is_some() || out.set_loop.is_some();
        let (menu, lane_menu) = state::with(|s| {
            s.apply(out);
            s.sync_audition();
            (
                s.piano.take_menu_request(),
                s.piano.take_lane_menu_request(),
            )
        });
        if any {
            self.as_mut().refresh();
        }
        let (px, py) = self.rust().last_press;
        if menu {
            self.as_mut().context_menu_requested(px, py);
        }
        if let Some(on_point) = lane_menu {
            self.lane_menu_requested(px, py, on_point);
        }
    }

    /// Hands pointer input to the piano roll and shows what came of it.
    fn input(self: Pin<&mut Self>, input: Input) {
        self.after(route::<Roll>(input));
    }

    fn press(mut self: Pin<&mut Self>, x: f64, y: f64, button: i32, modifiers: i32) {
        self.as_mut().rust_mut().last_press = (x, y);
        self.input(Input::Press(pointer(x, y, button, modifiers)));
    }

    fn motion(self: Pin<&mut Self>, x: f64, y: f64, modifiers: i32) {
        self.input(Input::Motion(pointer(x, y, 1, modifiers)));
    }

    fn release(self: Pin<&mut Self>, x: f64, y: f64, modifiers: i32) {
        self.input(Input::Release(pointer(x, y, 1, modifiers)));
    }

    fn double_click(self: Pin<&mut Self>, x: f64, y: f64, modifiers: i32) {
        self.input(Input::DoubleClick(pointer(x, y, 1, modifiers)));
    }

    fn cancel_drag(self: Pin<&mut Self>) {
        self.input(Input::Cancel);
    }

    fn hover(mut self: Pin<&mut Self>, x: f64, y: f64) {
        let c = state::with(|s| s.piano.cursor(s.doc.project(), x, y));
        self.as_mut().set_cursor_shape(cursor_shape(c));
    }

    fn wheel(self: Pin<&mut Self>, x: f64, y: f64, dx: f64, dy: f64, modifiers: i32) {
        let at = pointer(x, y, 0, modifiers);
        self.input(Input::Wheel { at, dx, dy });
    }

    fn touch_pans(&self, x: f64, y: f64) -> bool {
        paint::touch_pans::<Roll>(x, y)
    }

    fn has_menu_at(&self, x: f64, y: f64) -> bool {
        state::with(|s| s.piano.has_menu_at(x, y))
    }

    fn pinch(self: Pin<&mut Self>, scale: f64, x: f64) {
        self.input(Input::Pinch { scale, x });
    }

    fn scroll_to(self: Pin<&mut Self>, x: f64, y: f64) {
        paint::scroll_to::<Roll>(x, y);
        self.refresh();
    }

    fn act(self: Pin<&mut Self>, f: impl FnOnce(&mut state::AppState) -> daw_arrange::Outcome) {
        let out = state::with(f);
        self.after(out);
    }

    fn quantize(self: Pin<&mut Self>, strength: f64, ends: bool) {
        let q = Quantize {
            strength,
            ends: if ends {
                QuantizeEnds::Snap
            } else {
                QuantizeEnds::KeepLength
            },
        };
        self.act(|s| s.piano.quantize(&mut s.doc, q));
    }

    fn transpose(self: Pin<&mut Self>, semitones: i32) {
        self.act(|s| s.piano.transpose(&mut s.doc, semitones));
    }

    fn nudge(self: Pin<&mut Self>, steps: i32) {
        self.act(|s| s.piano.nudge(&mut s.doc, steps));
    }

    fn set_velocity(self: Pin<&mut Self>, velocity: f64) {
        self.act(|s| s.piano.set_velocity(&mut s.doc, velocity));
    }

    fn scale_velocity(self: Pin<&mut Self>, factor: f64) {
        self.act(|s| s.piano.scale_velocity(&mut s.doc, factor));
    }

    fn legato(self: Pin<&mut Self>) {
        self.act(|s| s.piano.legato(&mut s.doc));
    }

    fn glue(self: Pin<&mut Self>) {
        self.act(|s| s.piano.glue(&mut s.doc));
    }

    fn fix_overlaps(self: Pin<&mut Self>) {
        self.act(|s| s.piano.fix_overlaps(&mut s.doc));
    }

    fn humanize(self: Pin<&mut Self>) {
        // A new seed each time, so repeating it varies; redo replays the
        // same notes because the command stores them.
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        self.act(|s| s.piano.humanize(&mut s.doc, seed));
    }

    fn reverse(self: Pin<&mut Self>) {
        self.act(|s| s.piano.reverse(&mut s.doc));
    }

    fn duplicate(self: Pin<&mut Self>) {
        self.act(|s| s.piano.duplicate(&mut s.doc));
    }

    fn delete_selection(self: Pin<&mut Self>) {
        self.act(|s| s.piano.delete_selection(&mut s.doc));
    }

    fn split_at_playhead(self: Pin<&mut Self>) {
        self.act(|s| {
            let at = s.playhead();
            s.piano.split_at(&mut s.doc, at)
        });
    }

    fn select_all(self: Pin<&mut Self>) {
        self.act(|s| s.piano.select_all(s.doc.project()));
    }

    fn point_shape(&self) -> i32 {
        state::with(|s| s.piano.point_shape(s.doc.project()).map_or(-1, shape_index))
    }

    fn set_point_shape(self: Pin<&mut Self>, i: i32) {
        let to = shape(PointShape { repr: i });
        self.act(|s| s.piano.set_point_shape(&mut s.doc, to));
    }

    fn delete_points(self: Pin<&mut Self>) {
        self.act(|s| s.piano.delete_points(&mut s.doc));
    }

    fn zoom(self: Pin<&mut Self>, factor: f64) {
        self.act(|s| {
            let x = (s.piano.width - s.piano.keyboard_width) / 2.0;
            s.piano.view.zoom_at(factor, x);
            daw_arrange::Outcome {
                view: true,
                ..Default::default()
            }
        });
    }
}
