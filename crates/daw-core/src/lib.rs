//! Contract types and real-time safe building blocks shared by every crate.
//!
//! The public signatures in [`time`], [`event`], [`node`] and [`plugin`] are
//! the contracts the other crates build against: changing one needs a note
//! in the architecture doc. Adding a helper or a variant does not.
//!
//! The rest are real-time safe tools: [`queue`] for crossing threads, [`exclusive`] for
//! sharing one object between the audio thread and the main thread, [`split`] for the
//! audio-thread half of a plugin built on it,
//! [`dispose`] for freeing memory off the audio thread, [`denormals`] for the
//! floating-point mode of audio threads and [`meter`] for peak meters.
//! [`logging`] is where every crate's diagnostics go.
//!
//! Nothing in this crate depends on Qt or any other UI toolkit.

pub mod denormals;
pub mod dispose;
pub mod event;
pub mod exclusive;
pub mod logging;
pub mod meter;
pub mod node;
pub mod plugin;
pub mod queue;
pub mod split;
mod sync;
pub mod time;

pub use event::{Event, EventKind, EventList, NoteId, ParamId};
pub use node::{Node, PortLayout, ProcessContext, TransportLoop, TransportState};
pub use plugin::{
    BundleScan, ParamInfo, PluginDescriptor, PluginError, PluginFormat, PluginFormatKind,
    PluginInstance, PluginProcessor, ScanBuilder,
};
pub use time::{MusicalPosition, PPQ, SampleTime, SegmentTempoMap, TempoMap, TickTime, Timeline};
