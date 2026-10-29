//! The flat form of a `daw_arrange::Scene` that C++ paints with QPainter
//! (see `cpp/scenepainter.cpp`).

use daw_arrange::{Prim, scene::Align};

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
