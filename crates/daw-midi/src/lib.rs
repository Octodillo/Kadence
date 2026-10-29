//! MIDI editing for Kadence, without any UI toolkit.
//!
//! - [`grid`]: the snap grid in PPQ ticks, anchored on bars so odd meters
//!   work. The arrange view snaps with it too.

pub mod grid;

pub use grid::{Bar, BarBeat, Division, Feel, Grid, GridLine, LineKind, Meter, MeterPoint};
