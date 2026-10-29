use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};

use daw_core::dispose::{Collector, Retirer, deferred_drop};
use daw_core::event::{Event, EventList, ParamId};
use daw_core::queue::{Consumer, Producer, spsc};
use daw_core::time::SampleTime;

use crate::compile::{CompileError, CompiledGraph};
use crate::graph::{GraphDesc, NodeId};
use crate::transport::{LoopRange, Transport, TransportCommand};

#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub sample_rate: f64,
    /// Largest block a node is ever asked to process. Longer calls to
    /// [`EngineProcessor::process`] are split.
    pub max_frames: u32,
    /// Capacity of each node's event lists.
    pub event_capacity: usize,
    /// Capacity of the controller-to-processor command queue.
    pub command_capacity: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48_000.0,
            max_frames: 1024,
            event_capacity: 512,
            command_capacity: 256,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Compile(#[from] CompileError),
    #[error("the command queue to the audio thread is full")]
    QueueFull,
}

enum Command {
    Graph(Box<CompiledGraph>),
    Transport(TransportCommand),
    Param {
        node: NodeId,
        param: ParamId,
        value: f64,
    },
}

/// State the RT side publishes for the UI.
#[derive(Default)]
struct Shared {
    position: AtomicI64,
    playing: AtomicBool,
    latency: AtomicU32,
    swaps: AtomicU64,
    dropped: AtomicU64,
}

impl EngineController {
    /// Creates the two halves of an engine. Move the processor into the audio
    /// callback; keep the controller on a normal thread.
    pub fn new(config: EngineConfig) -> (Self, EngineProcessor) {
        let (commands_tx, commands_rx) = spsc(config.command_capacity);
        let (garbage_tx, garbage_rx) = deferred_drop(config.command_capacity);
        let shared = Arc::new(Shared::default());
        let processor = EngineProcessor {
            graph: None,
            pending: None,
            retired: None,
            commands: commands_rx,
            garbage: garbage_tx,
            transport: Transport::new(config.sample_rate),
            external: EventList::with_capacity(config.event_capacity),
            max_frames: config.max_frames as usize,
            shared: shared.clone(),
        };
        let controller = Self {
            config,
            commands: commands_tx,
            garbage: garbage_rx,
            shared,
        };
        (controller, processor)
    }
}

/// The non-RT half: compiles graphs, sends commands, and frees what the RT
/// side retires.
pub struct EngineController {
    config: EngineConfig,
    commands: Producer<Command>,
    garbage: Collector<Box<CompiledGraph>>,
    shared: Arc<Shared>,
}

impl EngineController {
    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    pub fn compile(&self, desc: GraphDesc) -> Result<CompiledGraph, CompileError> {
        let c = &self.config;
        CompiledGraph::compile(desc, c.sample_rate, c.max_frames, c.event_capacity)
    }

    /// Compiles `desc` and hands it to the audio thread, which swaps it in at
    /// the start of its next cycle. The graph it replaces comes back here and
    /// is dropped by [`collect_garbage`](Self::collect_garbage).
    pub fn set_graph(&mut self, desc: GraphDesc) -> Result<(), EngineError> {
        let graph = Box::new(self.compile(desc)?);
        self.send_graph(graph).map_err(|_| EngineError::QueueFull)
    }

    /// Sends an already compiled graph. On a full queue the graph is given
    /// back so the caller can retry.
    pub fn send_graph(&mut self, graph: Box<CompiledGraph>) -> Result<(), Box<CompiledGraph>> {
        self.collect_garbage();
        match self.commands.push(Command::Graph(graph)) {
            Ok(()) => Ok(()),
            Err(Command::Graph(g)) => Err(g),
            Err(_) => unreachable!("pushed a graph command"),
        }
    }

    pub fn transport(&mut self, cmd: TransportCommand) -> Result<(), EngineError> {
        self.send(Command::Transport(cmd))
    }

    pub fn play(&mut self) -> Result<(), EngineError> {
        self.transport(TransportCommand::Play)
    }

    pub fn stop(&mut self) -> Result<(), EngineError> {
        self.transport(TransportCommand::Stop)
    }

    pub fn locate(&mut self, position: SampleTime) -> Result<(), EngineError> {
        self.transport(TransportCommand::Locate(position))
    }

    pub fn set_loop(&mut self, range: Option<LoopRange>) -> Result<(), EngineError> {
        self.transport(TransportCommand::SetLoop(range))
    }

    pub fn set_bpm(&mut self, bpm: f64) -> Result<(), EngineError> {
        self.transport(TransportCommand::SetBpm(bpm))
    }

    /// Sends a `ParamValue` event to `node` in the current graph, delivered
    /// at frame 0 of the next cycle.
    pub fn set_param(
        &mut self,
        node: NodeId,
        param: ParamId,
        value: f64,
    ) -> Result<(), EngineError> {
        self.send(Command::Param { node, param, value })
    }

    /// Drops graphs the audio thread has retired. Returns how many.
    pub fn collect_garbage(&mut self) -> usize {
        self.garbage.collect()
    }

    pub fn position(&self) -> SampleTime {
        SampleTime(self.shared.position.load(Ordering::Relaxed))
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Relaxed)
    }

    /// Latency of the graph currently running.
    pub fn latency(&self) -> u32 {
        self.shared.latency.load(Ordering::Relaxed)
    }

    /// How many graphs the audio thread has swapped in.
    pub fn graphs_swapped(&self) -> u64 {
        self.shared.swaps.load(Ordering::Relaxed)
    }

    /// Parameter changes dropped because a node's queue was full or the node
    /// was not in the running graph.
    pub fn dropped_params(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }

    fn send(&mut self, cmd: Command) -> Result<(), EngineError> {
        self.commands.push(cmd).map_err(|_| EngineError::QueueFull)
    }
}

/// The RT half. [`process`](Self::process) does not allocate, lock or free.
///
/// Drop it off the audio thread: dropping frees the running graph.
pub struct EngineProcessor {
    graph: Option<Box<CompiledGraph>>,
    /// A received graph waiting for `retired` to empty.
    pending: Option<Box<CompiledGraph>>,
    /// A replaced graph waiting for room in the garbage queue.
    retired: Option<Box<CompiledGraph>>,
    commands: Consumer<Command>,
    garbage: Retirer<Box<CompiledGraph>>,
    transport: Transport,
    external: EventList,
    max_frames: usize,
    shared: Arc<Shared>,
}

impl EngineProcessor {
    /// Runs one backend cycle of `frames` samples.
    ///
    /// `audio_in` channels feed the graph's input node, master channels are
    /// written to `audio_out` (extra output channels are zeroed), and
    /// `events` go to the nodes the graph routes external events to. Every
    /// slice must hold at least `frames` samples.
    pub fn process(
        &mut self,
        frames: usize,
        audio_in: &[&[f32]],
        audio_out: &mut [&mut [f32]],
        events: &EventList,
    ) {
        self.drain_commands();
        let mut offset = 0;
        while offset < frames {
            let len = self
                .transport
                .block_len((frames - offset).min(self.max_frames));
            let last = offset + len == frames;
            self.external.clear();
            for e in events.iter() {
                let f = e.frame as usize;
                if f >= offset && (f < offset + len || last) {
                    let frame = (f - offset).min(len - 1) as u32;
                    let _ = self.external.push(Event { frame, ..*e });
                }
            }
            let state = self.transport.state();
            match self.graph.as_deref_mut() {
                Some(g) => {
                    g.process(&state, len, audio_in, offset, &self.external);
                    let channels = g.output_channels();
                    for (ch, out) in audio_out.iter_mut().enumerate() {
                        let out = &mut out[offset..offset + len];
                        if ch < channels {
                            out.copy_from_slice(g.output(ch, len));
                        } else {
                            out.fill(0.0);
                        }
                    }
                }
                None => {
                    for out in audio_out.iter_mut() {
                        out[offset..offset + len].fill(0.0);
                    }
                }
            }
            self.transport.advance(len);
            offset += len;
        }
        self.shared
            .position
            .store(self.transport.position().0, Ordering::Relaxed);
        self.shared
            .playing
            .store(self.transport.is_playing(), Ordering::Relaxed);
    }

    pub fn transport(&self) -> &Transport {
        &self.transport
    }

    pub fn graph(&self) -> Option<&CompiledGraph> {
        self.graph.as_deref()
    }

    fn drain_commands(&mut self) {
        self.retire();
        // Never hold two retired graphs: a graph that arrives while one is
        // still waiting to go back stays pending, and so does every command
        // after it, so commands keep their order.
        if self.pending.is_some() {
            if self.retired.is_some() {
                return;
            }
            let g = self.pending.take().expect("checked above");
            self.install(g);
        }
        while let Some(cmd) = self.commands.pop() {
            match cmd {
                Command::Graph(g) if self.retired.is_some() => {
                    self.pending = Some(g);
                    return;
                }
                Command::Graph(g) => self.install(g),
                Command::Transport(t) => self.transport.apply(t),
                Command::Param { node, param, value } => {
                    let queued = self
                        .graph
                        .as_deref_mut()
                        .is_some_and(|g| g.queue_param(node, param, value));
                    if !queued {
                        self.shared.dropped.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }
    }

    fn install(&mut self, g: Box<CompiledGraph>) {
        self.shared.latency.store(g.latency(), Ordering::Relaxed);
        self.retired = self.graph.replace(g);
        self.shared.swaps.fetch_add(1, Ordering::Relaxed);
        self.retire();
    }

    fn retire(&mut self) {
        if let Some(g) = self.retired.take() {
            if let Err(g) = self.garbage.retire(g) {
                self.retired = Some(g);
            }
        }
    }
}
