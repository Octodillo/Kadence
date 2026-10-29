//! The audio engine: turns a [`GraphDesc`] into a [`CompiledGraph`] and runs
//! it on the real-time thread.
//!
//! The split is the usual one for a real-time audio engine:
//!
//! * [`EngineController`] lives on a normal thread. It compiles graphs (which
//!   allocates), sends them and transport commands to the RT side through a
//!   lock-free queue, and drops retired graphs that come back.
//! * [`EngineProcessor`] lives on the audio thread. [`EngineProcessor::process`]
//!   never allocates, locks or frees.
//!
//! [`render`] and [`render_to_wav`] drive the same processor offline.

mod compile;
mod delay;
mod engine;
mod graph;
pub mod nodes;
mod render;
mod transport;

pub use compile::{CompileError, CompiledGraph, MAX_CHANNELS};
pub use engine::{EngineConfig, EngineController, EngineError, EngineProcessor};
pub use graph::{AudioEdge, EventEdge, GraphDesc, NodeId, NodeKind};
pub use render::{RenderError, RenderOptions, render, render_to_wav};
pub use transport::{LoopRange, Transport, TransportCommand};
