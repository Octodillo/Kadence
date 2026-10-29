//! Contract types and real-time safe building blocks shared by every crate.
//!
//! The public signatures in [`time`], [`event`], [`node`] and [`plugin`] are
//! the Milestone 0 contracts that the parallel work units build against.
//! Changing one needs a note in the architecture doc and a heads-up to the
//! threads that depend on it. Adding a helper or a variant does not.
//!
//! Nothing in this crate depends on Qt or any other UI toolkit.

pub mod event;
pub mod node;
pub mod plugin;
pub mod time;

pub use event::{Event, EventKind, EventList, NoteId, ParamId};
pub use node::{Node, PortLayout, ProcessContext, TransportState};
pub use plugin::{
    ParamInfo, PluginDescriptor, PluginError, PluginFormat, PluginFormatKind, PluginInstance,
};
pub use time::{PPQ, SampleTime, TempoMap, TickTime};
