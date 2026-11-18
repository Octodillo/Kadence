//! What a frontend sees of a painted surface that takes pointer input: the
//! arrange view here, the piano roll in `daw-pianoroll`.
//!
//! A frontend turns its toolkit's events into [`Input`] in one place and
//! hands each to the surface it landed on, so the surfaces need no code of
//! their own there for telling a release from a double click.

use daw_model::Document;

use crate::{Arrange, Env, Outcome, Pointer};

/// One piece of pointer input, in view coordinates.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Input {
    Press(Pointer),
    /// The pointer moved with a button held.
    Motion(Pointer),
    Release(Pointer),
    DoubleClick(Pointer),
    /// The drag in progress is called off (Escape, or the pointer was
    /// taken away).
    Cancel,
    /// A scroll wheel or touchpad at `at`: `dx`/`dy` in pixels, positive
    /// when the content should move right/down. The modifiers of `at`
    /// choose between scrolling and zooming.
    Wheel {
        at: Pointer,
        dx: f64,
        dy: f64,
    },
    /// A pinch: `scale` relative to the previous update, centred on `x`.
    Pinch {
        scale: f64,
        x: f64,
    },
}

/// A view of the document that is edited with the pointer.
pub trait Surface {
    /// Takes one piece of input, and says what it changed and asked for.
    fn input(&mut self, doc: &mut Document, env: &Env, input: Input) -> Outcome;

    /// Scrolls to content position (`x`, `y`), as a scroll bar asks.
    /// Negative positions are the start.
    fn scroll_to(&mut self, x: f64, y: f64);
}

impl Surface for Arrange {
    fn input(&mut self, doc: &mut Document, env: &Env, input: Input) -> Outcome {
        match input {
            Input::Press(p) => self.press(doc, p),
            Input::Motion(p) => self.motion(doc, env, p),
            Input::Release(p) => self.release(doc, p),
            Input::DoubleClick(p) => self.double_click(doc, p),
            Input::Cancel => self.cancel(doc),
            Input::Wheel { at, dx, dy } => self.wheel(doc.project(), at, dx, dy),
            Input::Pinch { scale, x } => self.pinch(doc.project(), scale, x),
        }
    }

    fn scroll_to(&mut self, x: f64, y: f64) {
        self.view.scroll_x = x.max(0.0);
        self.scroll_y = y.max(0.0);
    }
}
