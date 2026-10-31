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
use cxx_qt_lib::{QColor, QString};
use daw_arrange::Scene;
use daw_midi::edit::{Quantize, QuantizeEnds};

use crate::arrange_view::{cursor_shape, pointer, to_qcolor, to_rgba};
use crate::paint::{self, PaintOp};
use crate::session_mixer::strip_row;
use crate::state;

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
        /// A Qt::CursorShape for the pointer's position.
        #[qproperty(i32, cursor_shape)]
        type PianoRoll = super::PianoRollRust;

        #[cxx_override]
        unsafe fn paint(self: Pin<&mut PianoRoll>, painter: *mut QPainter);

        /// Rebuilds the picture from the current state.
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

        /// A context menu was asked for over a note.
        #[qsignal]
        fn context_menu_requested(self: Pin<&mut PianoRoll>, x: f64, y: f64);
        /// The document or view changed through this item.
        #[qsignal]
        fn edited(self: Pin<&mut PianoRoll>);
    }

    unsafe extern "RustQt" {
        #[inherit]
        fn update(self: Pin<&mut PianoRoll>);
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
    scroll_x: f64,
    scroll_y: f64,
    content_height: f64,
    has_clip: bool,
    clip_name: QString,
    strip_row: i32,
    has_selection: bool,
    cursor_shape: i32,
    ops: Vec<PaintOp>,
    texts: Vec<String>,
    waves: Vec<f32>,
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
            scroll_x: 0.0,
            scroll_y: 0.0,
            content_height: 0.0,
            has_clip: false,
            clip_name: QString::default(),
            strip_row: -1,
            has_selection: false,
            cursor_shape: 0,
            ops: Vec::new(),
            texts: Vec::new(),
            waves: Vec::new(),
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
}

impl qobject::PianoRoll {
    /// # Safety
    /// Called by Qt with a valid painter for the duration of the call.
    unsafe fn paint(self: Pin<&mut Self>, painter: *mut cxx_qt_lib::QPainter) {
        // SAFETY: Qt passes a live, active painter.
        let Some(painter) = (unsafe { painter.as_mut() }) else {
            return;
        };
        // SAFETY: QPainter is not moved while we hold it.
        let painter = unsafe { Pin::new_unchecked(painter) };
        paint::paint_scene(painter, &self.ops, &self.texts, &self.waves);
    }

    fn palette(&self) -> daw_arrange::Palette {
        daw_arrange::Palette {
            background: to_rgba(&self.background_color),
            alternate_background: to_rgba(&self.alternate_background_color),
            text: to_rgba(&self.text_color),
            disabled_text: to_rgba(&self.disabled_text_color),
            highlight: to_rgba(&self.highlight_color),
            highlighted_text: to_rgba(&self.highlighted_text_color),
            negative: to_rgba(&self.negative_color),
        }
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
            strip_row: i32,
        }
        let (w, h) = (self.width(), self.height());
        let palette = self.palette();
        let (rh, kw, vh) = (self.ruler_height, self.keyboard_width, self.velocity_height);
        let (scene, shown) = state::with(|s| {
            s.piano.palette = palette;
            s.piano.ruler_height = rh;
            s.piano.keyboard_width = kw;
            s.piano.velocity_height = vh;
            s.piano.resize(w, h);
            s.piano.sync(s.doc.project());
            s.piano.fit_if_pending(s.doc.project());
            let scene = s.with_piano(|p, doc, env| p.scene(doc.project(), env));
            let p = &s.piano;
            let project = s.doc.project();
            let shown = Shown {
                scroll_x: p.view.scroll_x,
                scroll_y: p.scroll_y,
                content_height: 128.0 * p.key_height,
                has_clip: p.has_clip(project),
                clip_name: p.clip_name(project).unwrap_or_default(),
                has_selection: !p.selection.is_empty(),
                strip_row: p
                    .track(project)
                    .and_then(|t| strip_row(project, t))
                    .map_or(-1, |row| row as i32),
            };
            (scene, shown)
        });
        self.as_mut().store(scene);
        self.as_mut().set_scroll_x(shown.scroll_x);
        self.as_mut().set_scroll_y(shown.scroll_y);
        self.as_mut().set_content_height(shown.content_height);
        self.as_mut().set_has_clip(shown.has_clip);
        self.as_mut()
            .set_clip_name(QString::from(shown.clip_name.as_str()));
        self.as_mut().set_has_selection(shown.has_selection);
        self.as_mut().set_strip_row(shown.strip_row);
        self.update();
    }

    fn store(self: Pin<&mut Self>, scene: Scene) {
        let mut r = self.rust_mut();
        let r = &mut *r;
        r.ops.clear();
        r.texts.clear();
        r.waves.clear();
        for p in scene.prims {
            r.ops.push(paint::op(p, &mut r.texts, &mut r.waves));
        }
    }

    /// Applies an outcome, then redraws and tells QML.
    fn after(mut self: Pin<&mut Self>, out: daw_arrange::Outcome) {
        let any = out.document || out.view || out.locate.is_some() || out.set_loop.is_some();
        let menu = state::with(|s| {
            s.apply(out);
            s.sync_audition();
            s.piano.take_menu_request()
        });
        if any {
            self.as_mut().refresh();
            self.as_mut().edited();
        }
        if menu {
            let (px, py) = self.rust().last_press;
            self.context_menu_requested(px, py);
        }
    }

    fn press(mut self: Pin<&mut Self>, x: f64, y: f64, button: i32, modifiers: i32) {
        self.as_mut().rust_mut().last_press = (x, y);
        let p = pointer(x, y, button, modifiers);
        let out = state::with(|s| s.with_piano(|pr, doc, env| pr.press(doc, env, p)));
        self.after(out);
    }

    fn motion(self: Pin<&mut Self>, x: f64, y: f64, modifiers: i32) {
        let p = pointer(x, y, 1, modifiers);
        let out = state::with(|s| s.piano.motion(&mut s.doc, p));
        self.after(out);
    }

    fn release(self: Pin<&mut Self>, x: f64, y: f64, modifiers: i32) {
        let p = pointer(x, y, 1, modifiers);
        let out = state::with(|s| s.piano.release(&mut s.doc, p));
        self.after(out);
    }

    fn double_click(self: Pin<&mut Self>, x: f64, y: f64, modifiers: i32) {
        let p = pointer(x, y, 1, modifiers);
        let out = state::with(|s| s.piano.double_click(&mut s.doc, p));
        self.after(out);
    }

    fn cancel_drag(self: Pin<&mut Self>) {
        let out = state::with(|s| s.piano.cancel(&mut s.doc));
        self.after(out);
    }

    fn hover(mut self: Pin<&mut Self>, x: f64, y: f64) {
        let c = state::with(|s| s.piano.cursor(s.doc.project(), x, y));
        self.as_mut().set_cursor_shape(cursor_shape(c));
    }

    fn wheel(self: Pin<&mut Self>, x: f64, y: f64, dx: f64, dy: f64, modifiers: i32) {
        let p = pointer(x, y, 0, modifiers);
        let out = state::with(|s| s.piano.wheel(p, dx, dy));
        self.after(out);
    }

    fn pinch(self: Pin<&mut Self>, scale: f64, x: f64) {
        let out = state::with(|s| s.piano.pinch(scale, x));
        self.after(out);
    }

    fn scroll_to(self: Pin<&mut Self>, x: f64, y: f64) {
        state::with(|s| {
            s.piano.view.scroll_x = x.max(0.0);
            s.piano.scroll_y = y.max(0.0);
            s.touch();
        });
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
