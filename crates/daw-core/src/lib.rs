//! Contract types and real-time safe building blocks shared by every crate.
//!
//! The public signatures in [`time`], [`event`], [`node`] and [`plugin`] are
//! the Milestone 0 contracts that the parallel work units build against.
//! Changing one needs a note in the architecture doc and a heads-up to the
//! threads that depend on it. Adding a helper or a variant does not.
//!
//! The rest are real-time safe tools: [`queue`] for crossing threads,
//! [`dispose`] for freeing memory off the audio thread, [`rtlog`] for logging
//! from it and [`meter`] for peak meters.
//!
//! Nothing in this crate depends on Qt or any other UI toolkit.

pub mod dispose;
pub mod event;
pub mod meter;
pub mod node;
pub mod plugin;
pub mod queue;
pub mod rtlog;
mod sync;
pub mod time;

pub use event::{Event, EventKind, EventList, NoteId, ParamId};
pub use node::{Node, PortLayout, ProcessContext, TransportState};
pub use plugin::{
    ParamInfo, PluginDescriptor, PluginError, PluginFormat, PluginFormatKind, PluginInstance,
};
pub use time::{PPQ, SampleTime, SegmentTempoMap, TempoMap, TickTime};
