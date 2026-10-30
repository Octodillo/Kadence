//! MIDI editing for Kadence, without any UI toolkit.
//!
//! - [`grid`]: the snap grid in PPQ ticks, anchored on bars so odd meters
//!   work. The arrange view snaps with it too.
//! - [`note`]: [`NoteData`], the plain note value the editing code works on,
//!   and the [`EditNote`] trait that lets it edit the model's notes in place.
//! - [`edit`]: piano-roll operations: quantize, transpose, nudge, resize,
//!   velocity, legato, split, glue, duplicate, reverse, invert and humanize.
//! - [`view`]: piano-roll and velocity-lane geometry: ticks and keys to
//!   pixels, hit testing, rubber-band selection and drags with snapping.
//! - [`sequence`]: clip notes compiled to sample time for playback.
//! - [`player`]: [`MidiPlayer`](player::MidiPlayer), a real-time [`Node`]
//!   that plays a [`Sequence`](sequence::Sequence), takes live edits
//!   without a graph rebuild, and captures input for recording.
//! - [`record`]: turns captured input into notes and plans merging a take.
//! - [`model`]: glue to `daw-model`: notes as [`EditNote`], edits as one
//!   undoable command, the meter and tempo map, and compiling a track.
//!
//! [`Node`]: daw_core::node::Node

pub mod edit;
pub mod grid;
pub mod model;
pub mod note;
pub mod player;
pub mod record;
pub mod sequence;
pub mod view;

pub use grid::{Bar, BarBeat, Division, Feel, Grid, GridLine, LineKind, Meter, MeterPoint};
pub use note::{EditNote, NoteData};
