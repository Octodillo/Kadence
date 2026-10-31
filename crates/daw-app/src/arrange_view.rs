//! `ArrangeView`: the painted timeline, a custom QQuickPaintedItem. Input from QML handlers goes to
//! `daw_arrange::Arrange`; the display list it returns is flattened for the
//! C++ painter in `cpp/scenepainter.cpp`.
//!
//! The scene is built on the GUI thread (in the invokables). `paint` runs on
//! the render thread while the GUI thread waits, and only reads the copy
//! stored in this object.

use std::pin::Pin;

use cxx_qt::CxxQtType;
use cxx_qt_lib::{QColor, QString, QUrl};
use daw_arrange::{Button, Cursor, Hit, Pointer, Rgba, Scene};

use crate::message::{Context, Message};
use crate::paint::{self, PaintOp};
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
        include!("cxx-qt-lib/qurl.h");
        type QUrl = cxx_qt_lib::QUrl;
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
        /// Height of the bar ruler across the top.
        #[qproperty(f64, ruler_height)]
        #[qproperty(f64, track_height)]
        /// Height of one automation lane under a track; set from QML.
        #[qproperty(f64, lane_height)]
        /// Scroll offsets and content size, for scroll bars and the track
        /// header list.
        #[qproperty(f64, scroll_x)]
        #[qproperty(f64, scroll_y)]
        #[qproperty(f64, content_width)]
        #[qproperty(f64, content_height)]
        /// A Qt::CursorShape for the pointer's position.
        #[qproperty(i32, cursor_shape)]
        /// While the gain handle of a clip is dragged: true, with the
        /// clip's gain in dB, for QML to show by the pointer.
        #[qproperty(bool, gain_dragging)]
        #[qproperty(f64, dragged_gain)]
        type ArrangeView = super::ArrangeViewRust;

        #[cxx_override]
        unsafe fn paint(self: Pin<&mut ArrangeView>, painter: *mut QPainter);

        /// Rebuilds the picture from the current state.
        #[qinvokable]
        fn refresh(self: Pin<&mut ArrangeView>);

        /// `button` and `modifiers` are Qt.MouseButton and
        /// Qt.KeyboardModifiers values.
        #[qinvokable]
        fn press(self: Pin<&mut ArrangeView>, x: f64, y: f64, button: i32, modifiers: i32);
        #[qinvokable]
        fn motion(self: Pin<&mut ArrangeView>, x: f64, y: f64, modifiers: i32);
        #[qinvokable]
        fn release(self: Pin<&mut ArrangeView>, x: f64, y: f64, modifiers: i32);
        #[qinvokable]
        fn double_click(self: Pin<&mut ArrangeView>, x: f64, y: f64, modifiers: i32);
        #[qinvokable]
        fn cancel_drag(self: Pin<&mut ArrangeView>);
        /// Names for a new MIDI clip, for audio from a file without a name
        /// and for a recorded clip, in the user's language.
        #[qinvokable]
        fn set_default_names(
            self: Pin<&mut ArrangeView>,
            midi_clip: &QString,
            audio: &QString,
            recording: &QString,
        );
        #[qinvokable]
        fn hover(self: Pin<&mut ArrangeView>, x: f64, y: f64);
        /// Pixel deltas, positive when the content should move right/down.
        #[qinvokable]
        fn wheel(self: Pin<&mut ArrangeView>, x: f64, y: f64, dx: f64, dy: f64, modifiers: i32);
        #[qinvokable]
        fn pinch(self: Pin<&mut ArrangeView>, scale: f64, x: f64);
        /// From the scroll bars.
        #[qinvokable]
        fn scroll_to(self: Pin<&mut ArrangeView>, x: f64, y: f64);
        /// Adds a dropped audio file at view position (x, y). Returns false
        /// if it could not; `Project.status` says why.
        #[qinvokable]
        fn drop_file(self: Pin<&mut ArrangeView>, url: &QUrl, x: f64, y: f64) -> bool;

        /// The height of track row `row`: the track and its shown lanes.
        #[qinvokable]
        fn row_height(self: &ArrangeView, row: i32) -> f64;
        /// The row a track header dragged to content-y `y` lands on.
        #[qinvokable]
        fn row_at_content(self: &ArrangeView, y: f64) -> i32;

        /// A context menu was asked for; `on_clip` tells which one.
        #[qsignal]
        fn context_menu_requested(self: Pin<&mut ArrangeView>, x: f64, y: f64, on_clip: bool);
        /// A context menu was asked for on an automation lane; `on_point`
        /// when over a point (it is then selected).
        #[qsignal]
        fn automation_menu_requested(self: Pin<&mut ArrangeView>, x: f64, y: f64, on_point: bool);
        /// A clip was double-clicked: open it in the editor.
        #[qsignal]
        fn clip_activated(self: Pin<&mut ArrangeView>);
        /// The document or view changed through this item.
        #[qsignal]
        fn edited(self: Pin<&mut ArrangeView>);
    }

    unsafe extern "RustQt" {
        #[inherit]
        fn update(self: Pin<&mut ArrangeView>);
        #[inherit]
        fn width(self: &ArrangeView) -> f64;
        #[inherit]
        fn height(self: &ArrangeView) -> f64;
    }

    impl cxx_qt::Constructor<()> for ArrangeView {}
}

pub struct ArrangeViewRust {
    background_color: QColor,
    alternate_background_color: QColor,
    text_color: QColor,
    disabled_text_color: QColor,
    highlight_color: QColor,
    highlighted_text_color: QColor,
    negative_color: QColor,
    ruler_height: f64,
    track_height: f64,
    lane_height: f64,
    scroll_x: f64,
    scroll_y: f64,
    content_width: f64,
    content_height: f64,
    cursor_shape: i32,
    gain_dragging: bool,
    dragged_gain: f64,
    /// The last scene, flattened for the painter.
    ops: Vec<PaintOp>,
    texts: Vec<String>,
    waves: Vec<f32>,
    /// Where the last press was, for placing a context menu.
    last_press: (f64, f64),
}

impl Default for ArrangeViewRust {
    fn default() -> Self {
        let p = daw_arrange::Palette::default();
        Self {
            background_color: to_qcolor(p.background),
            alternate_background_color: to_qcolor(p.alternate_background),
            text_color: to_qcolor(p.text),
            disabled_text_color: to_qcolor(p.disabled_text),
            highlight_color: to_qcolor(p.highlight),
            highlighted_text_color: to_qcolor(p.highlighted_text),
            negative_color: to_qcolor(p.negative),
            ruler_height: 24.0,
            track_height: 64.0,
            lane_height: 40.0,
            scroll_x: 0.0,
            scroll_y: 0.0,
            content_width: 0.0,
            content_height: 0.0,
            cursor_shape: 0,
            gain_dragging: false,
            dragged_gain: 0.0,
            ops: Vec::new(),
            texts: Vec::new(),
            waves: Vec::new(),
            last_press: (0.0, 0.0),
        }
    }
}

impl cxx_qt::Constructor<()> for qobject::ArrangeView {
    type NewArguments = ();
    type BaseArguments = ();
    type InitializeArguments = ();

    fn route_arguments(_: ()) -> ((), (), ()) {
        ((), (), ())
    }

    fn new(_: ()) -> ArrangeViewRust {
        ArrangeViewRust::default()
    }
}

pub(crate) fn to_qcolor(c: Rgba) -> QColor {
    QColor::from_rgba(
        i32::from(c.r()),
        i32::from(c.g()),
        i32::from(c.b()),
        i32::from(c.a()),
    )
}

pub(crate) fn to_rgba(c: &QColor) -> Rgba {
    let ch = |v: i32| v.clamp(0, 255) as u32;
    Rgba(ch(c.alpha()) << 24 | ch(c.red()) << 16 | ch(c.green()) << 8 | ch(c.blue()))
}

// Qt::KeyboardModifier and Qt::MouseButton values.
const SHIFT: i32 = 0x0200_0000;
const CTRL: i32 = 0x0400_0000;
const ALT: i32 = 0x0800_0000;

pub(crate) fn pointer(x: f64, y: f64, button: i32, modifiers: i32) -> Pointer {
    Pointer {
        x,
        y,
        button: match button {
            2 => Button::Right,
            4 => Button::Middle,
            _ => Button::Left,
        },
        shift: modifiers & SHIFT != 0,
        ctrl: modifiers & CTRL != 0,
        alt: modifiers & ALT != 0,
    }
}

/// Qt::CursorShape values.
pub(crate) fn cursor_shape(c: Cursor) -> i32 {
    match c {
        Cursor::Arrow => 0,
        Cursor::Move => 17,
        Cursor::ResizeHorizontal => 6,
        Cursor::ResizeVertical => 5,
        Cursor::Pointing => 13,
        Cursor::Crosshair => 2,
    }
}

impl qobject::ArrangeView {
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
        let (w, h) = (self.width(), self.height());
        let palette = self.palette();
        let ruler = self.ruler_height;
        let lane_height = self.lane_height;
        let (scene, geometry) = state::with(|s| {
            s.arrange.palette = palette;
            s.arrange.ruler_height = ruler;
            // Unclamped (but never zero), so rows stay level with the lane
            // headers, which use the property as it is.
            s.arrange.lane_height = lane_height.max(1.0);
            let project = s.doc.project().clone();
            s.arrange.resize(&project, w, h);
            s.arrange.peaks.poll();
            let scene = s.with_arrange(|a, doc, env| a.scene(doc.project(), env));
            let a = &s.arrange;
            let geometry = (
                a.view.scroll_x,
                a.scroll_y,
                a.content_width(&project),
                a.content_height(&project),
                a.track_height,
            );
            (scene, geometry)
        });
        self.as_mut().store(scene);
        let (sx, sy, cw, ch, th) = geometry;
        self.as_mut().set_scroll_x(sx);
        self.as_mut().set_scroll_y(sy);
        self.as_mut().set_content_width(cw);
        self.as_mut().set_content_height(ch);
        self.as_mut().set_track_height(th);
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
        let menu = out.context_menu;
        let open = out.open_clip.is_some();
        let any = out.document || out.view || out.locate.is_some() || out.set_loop.is_some();
        let gain = state::with(|s| {
            s.apply(out);
            s.arrange.dragged_gain(s.doc.project())
        });
        self.as_mut().set_gain_dragging(gain.is_some());
        if let Some(db) = gain {
            self.as_mut().set_dragged_gain(db);
        }
        if any {
            self.as_mut().refresh();
            self.as_mut().edited();
        }
        if open {
            self.as_mut().clip_activated();
        }
        if let Some(hit) = menu {
            let (px, py) = self.rust().last_press;
            match hit {
                Hit::Automation {
                    lane: Some(_),
                    point,
                    ..
                } => {
                    self.automation_menu_requested(px, py, point.is_some());
                }
                // The empty row of a track without lanes: the headers add them.
                Hit::Automation { lane: None, .. } => {}
                _ => {
                    let on_clip = matches!(hit, Hit::Clip { .. });
                    self.context_menu_requested(px, py, on_clip);
                }
            }
        }
    }

    fn press(mut self: Pin<&mut Self>, x: f64, y: f64, button: i32, modifiers: i32) {
        self.as_mut().rust_mut().last_press = (x, y);
        let p = pointer(x, y, button, modifiers);
        let out = state::with(|s| s.arrange.press(&mut s.doc, p));
        self.after(out);
    }

    fn motion(self: Pin<&mut Self>, x: f64, y: f64, modifiers: i32) {
        let p = pointer(x, y, 1, modifiers);
        let out = state::with(|s| s.with_arrange(|a, doc, env| a.motion(doc, env, p)));
        self.after(out);
    }

    fn release(self: Pin<&mut Self>, x: f64, y: f64, modifiers: i32) {
        let p = pointer(x, y, 1, modifiers);
        let out = state::with(|s| s.arrange.release(&mut s.doc, p));
        self.after(out);
    }

    fn double_click(self: Pin<&mut Self>, x: f64, y: f64, modifiers: i32) {
        let p = pointer(x, y, 1, modifiers);
        let out = state::with(|s| s.arrange.double_click(&mut s.doc, p));
        self.after(out);
    }

    fn set_default_names(
        self: Pin<&mut Self>,
        midi_clip: &QString,
        audio: &QString,
        recording: &QString,
    ) {
        state::with(|s| {
            s.arrange.names = daw_arrange::DefaultNames {
                midi_clip: midi_clip.to_string(),
                audio: audio.to_string(),
                recording: recording.to_string(),
            };
        });
    }

    fn cancel_drag(self: Pin<&mut Self>) {
        let out = state::with(|s| s.arrange.cancel(&mut s.doc));
        self.after(out);
    }

    fn hover(mut self: Pin<&mut Self>, x: f64, y: f64) {
        let c = state::with(|s| s.arrange.cursor(s.doc.project(), x, y));
        self.as_mut().set_cursor_shape(cursor_shape(c));
    }

    fn wheel(self: Pin<&mut Self>, x: f64, y: f64, dx: f64, dy: f64, modifiers: i32) {
        let p = pointer(x, y, 0, modifiers);
        let out = state::with(|s| {
            let project = s.doc.project().clone();
            s.arrange.wheel(&project, p, dx, dy)
        });
        self.after(out);
    }

    fn pinch(self: Pin<&mut Self>, scale: f64, x: f64) {
        let out = state::with(|s| {
            let project = s.doc.project().clone();
            s.arrange.pinch(&project, scale, x)
        });
        self.after(out);
    }

    fn scroll_to(self: Pin<&mut Self>, x: f64, y: f64) {
        state::with(|s| {
            s.arrange.view.scroll_x = x.max(0.0);
            s.arrange.scroll_y = y.max(0.0);
            s.touch();
        });
        self.refresh();
    }

    fn row_height(&self, row: i32) -> f64 {
        let Ok(row) = usize::try_from(row) else {
            return self.track_height;
        };
        state::with(|s| {
            let project = s.doc.project().clone();
            s.arrange.relayout(&project);
            if row < project.tracks.len() {
                s.arrange.row_height(row)
            } else {
                s.arrange.track_height
            }
        })
    }

    fn row_at_content(&self, y: f64) -> i32 {
        state::with(|s| {
            let project = s.doc.project().clone();
            s.arrange.relayout(&project);
            s.arrange.row_at_content(y, project.tracks.len()) as i32
        })
    }

    fn drop_file(mut self: Pin<&mut Self>, url: &QUrl, x: f64, y: f64) -> bool {
        let ok = state::with(|s| {
            let Some(path) = url.to_local_file() else {
                s.report(Message::NotLocalFile(Context::Import));
                return false;
            };
            let path = std::path::PathBuf::from(path.to_string());
            let project = s.doc.project();
            let track = s.arrange.drop_track(project, y);
            let at = s.arrange.drop_tick(project, x);
            match s.import_audio(&path, track, at) {
                Ok(()) => true,
                Err(m) => {
                    s.report(m);
                    false
                }
            }
        });
        self.as_mut().refresh();
        self.as_mut().edited();
        ok
    }
}
