//! What the painted items (the arrange view and the piano roll) share: the
//! flat form of a `daw_arrange::Scene` that C++ paints with QPainter (see
//! `cpp/scenepainter.cpp`), kept between paints in a [`DisplayList`], the
//! theme colours they take from QML, and how Qt's pointer events and
//! cursors map to `daw_arrange`'s.

use std::pin::Pin;

use cxx_qt_lib::QColor;
use daw_arrange::{Button, Cursor, Pointer, Prim, Rgba, Scene, scene::Align};

#[cxx::bridge(namespace = "kadence")]
mod ffi {
    /// One primitive. Which fields matter depends on `kind`; see
    /// `scenepainter.cpp`.
    #[derive(Clone, Debug, Default)]
    struct PaintOp {
        kind: u8,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        radius: f64,
        fill: u32,
        stroke: u32,
        width: f64,
        /// Bit 0: bold text. Bit 1: centred text.
        flags: u8,
        /// Index into the text list.
        text: u32,
        /// Range of `(min, max)` pairs in the wave list.
        wave_start: u32,
        wave_len: u32,
    }

    unsafe extern "C++" {
        include!("daw-app/src/cpp/scenepainter.h");
        include!("cxx-qt-lib/qpainter.h");
        #[namespace = ""]
        type QPainter = cxx_qt_lib::QPainter;

        #[cxx_name = "paintScene"]
        fn paint_scene(
            painter: Pin<&mut QPainter>,
            ops: &[PaintOp],
            texts: &[String],
            waves: &[f32],
        );
    }
}

pub use ffi::{PaintOp, paint_scene};

pub const RECT: u8 = 0;
pub const ROUND_RECT: u8 = 1;
pub const LINE: u8 = 2;
pub const TEXT: u8 = 3;
pub const WAVE: u8 = 4;
pub const PUSH_CLIP: u8 = 5;
pub const POP_CLIP: u8 = 6;

/// Flattens one primitive, appending its text or wave data to the lists.
pub fn op(p: Prim, texts: &mut Vec<String>, waves: &mut Vec<f32>) -> PaintOp {
    let rect = |kind: u8, r: daw_arrange::Rect| PaintOp {
        kind,
        x: r.x,
        y: r.y,
        w: r.w,
        h: r.h,
        ..Default::default()
    };
    match p {
        Prim::Rect { r, fill } => PaintOp {
            fill: fill.0,
            ..rect(RECT, r)
        },
        Prim::RoundRect {
            r,
            radius,
            fill,
            stroke,
            stroke_width,
        } => PaintOp {
            radius,
            fill: fill.0,
            stroke: stroke.0,
            width: stroke_width,
            ..rect(ROUND_RECT, r)
        },
        Prim::Line {
            x0,
            y0,
            x1,
            y1,
            color,
            width,
        } => PaintOp {
            kind: LINE,
            x: x0,
            y: y0,
            w: x1,
            h: y1,
            stroke: color.0,
            width,
            ..Default::default()
        },
        Prim::Text {
            r,
            text,
            color,
            bold,
            align,
        } => {
            texts.push(text);
            PaintOp {
                fill: color.0,
                flags: u8::from(bold) | if align == Align::Center { 2 } else { 0 },
                text: (texts.len() - 1) as u32,
                ..rect(TEXT, r)
            }
        }
        Prim::Wave { r, columns, color } => {
            // Counted in (min, max) pairs.
            let start = (waves.len() / 2) as u32;
            for (lo, hi) in &columns {
                waves.push(*lo);
                waves.push(*hi);
            }
            PaintOp {
                fill: color.0,
                wave_start: start,
                wave_len: columns.len() as u32,
                ..rect(WAVE, r)
            }
        }
        Prim::PushClip(r) => rect(PUSH_CLIP, r),
        Prim::PopClip => PaintOp {
            kind: POP_CLIP,
            ..Default::default()
        },
    }
}

/// The last scene, flattened for the painter and kept until the next one.
#[derive(Default)]
pub struct DisplayList {
    ops: Vec<PaintOp>,
    texts: Vec<String>,
    waves: Vec<f32>,
}

impl DisplayList {
    /// Replaces what is shown with `scene`, reusing the lists' storage.
    pub fn store(&mut self, scene: Scene) {
        self.ops.clear();
        self.texts.clear();
        self.waves.clear();
        for p in scene.prims {
            self.ops.push(op(p, &mut self.texts, &mut self.waves));
        }
    }

    /// Paints the stored scene. For a `QQuickPaintedItem`'s `paint`.
    ///
    /// # Safety
    /// `painter` is null or a live, active painter, as Qt passes one for
    /// the duration of `paint`.
    pub unsafe fn paint(&self, painter: *mut cxx_qt_lib::QPainter) {
        // SAFETY: the caller passes null or a live painter.
        let Some(painter) = (unsafe { painter.as_mut() }) else {
            return;
        };
        // SAFETY: QPainter is not moved while we hold it.
        let painter = unsafe { Pin::new_unchecked(painter) };
        paint_scene(painter, &self.ops, &self.texts, &self.waves);
    }
}

pub fn to_qcolor(c: Rgba) -> QColor {
    QColor::from_rgba(
        i32::from(c.r()),
        i32::from(c.g()),
        i32::from(c.b()),
        i32::from(c.a()),
    )
}

pub fn to_rgba(c: &QColor) -> Rgba {
    let ch = |v: i32| v.clamp(0, 255) as u32;
    Rgba(ch(c.alpha()) << 24 | ch(c.red()) << 16 | ch(c.green()) << 8 | ch(c.blue()))
}

// Qt::KeyboardModifier and Qt::MouseButton values.
const SHIFT: i32 = 0x0200_0000;
const CTRL: i32 = 0x0400_0000;
const ALT: i32 = 0x0800_0000;

/// A pointer event as QML passes it: `button` a Qt::MouseButton and
/// `modifiers` Qt::KeyboardModifiers.
pub fn pointer(x: f64, y: f64, button: i32, modifiers: i32) -> Pointer {
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
pub fn cursor_shape(c: Cursor) -> i32 {
    match c {
        Cursor::Arrow => 0,
        Cursor::Move => 17,
        Cursor::ResizeHorizontal => 6,
        Cursor::ResizeVertical => 5,
        Cursor::Pointing => 13,
        Cursor::Crosshair => 2,
    }
}

/// The [`Palette`](daw_arrange::Palette) of a painted item's seven
/// theme-colour properties (`background_color` and so on, which QML binds
/// to the Kirigami theme).
macro_rules! theme_palette {
    ($item:expr) => {
        daw_arrange::Palette {
            background: $crate::paint::to_rgba(&$item.background_color),
            alternate_background: $crate::paint::to_rgba(&$item.alternate_background_color),
            text: $crate::paint::to_rgba(&$item.text_color),
            disabled_text: $crate::paint::to_rgba(&$item.disabled_text_color),
            highlight: $crate::paint::to_rgba(&$item.highlight_color),
            highlighted_text: $crate::paint::to_rgba(&$item.highlighted_text_color),
            negative: $crate::paint::to_rgba(&$item.negative_color),
        }
    };
}
pub(crate) use theme_palette;
