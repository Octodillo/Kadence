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
//!
//! Graphs are rebuilt whole on every edit. To keep a node's state (a synth's
//! voices, a plugin's internals, a strip's fader) across an edit, give it a
//! [`NodeKey`] and build the next graph with [`EngineController::graph`] and
//! [`GraphDesc::reuse`]: the audio thread moves the live node into the new
//! graph when it swaps it in.

mod automation;
mod compile;
mod delay;
mod engine;
mod graph;
mod meter;
pub mod mixer;
pub mod nodes;
mod render;
mod transport;

pub use automation::{AutomationLane, AutomationPoint, Curve, RAMP_INTERVAL};
pub use compile::{CompileError, CompiledGraph, MAX_CHANNELS};
pub use engine::{EngineConfig, EngineController, EngineError, EngineProcessor, SendError};
pub use graph::{AudioEdge, EventEdge, GraphDesc, NodeId, NodeKey, NodeKind};
pub use meter::NodeMeter;
pub use render::{RenderError, RenderOptions, render, render_to_wav};
pub use transport::{LoopRange, Transport, TransportCommand};
