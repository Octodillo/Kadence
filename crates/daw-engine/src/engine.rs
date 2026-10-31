use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use daw_core::dispose::{Collector, Retirer, deferred_drop};
use daw_core::event::{Event, EventList, ParamId};
use daw_core::queue::{Consumer, Producer, spsc};
use daw_core::time::{SampleTime, SegmentTempoMap, Timeline};

use crate::compile::{CompileError, CompiledGraph};
use crate::graph::{GraphDesc, LiveNodes, NodeId, NodeKey};
use crate::meter::NodeMeter;
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
    /// How many separate event sources
    /// [`EngineProcessor::process_sources`] takes (MIDI input ports, say),
    /// so that a node can get the events of one source only. Sources past
    /// this still reach nodes that take every source.
    pub event_sources: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48_000.0,
            max_frames: 1024,
            event_capacity: 512,
            command_capacity: 256,
            event_sources: 1,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Compile(#[from] CompileError),
    #[error("the command queue to the audio thread is full")]
    QueueFull,
    #[error("no node with key {0:?} is in the latest graph")]
    UnknownKey(NodeKey),
}

/// Why [`EngineController::send_graph`] gave a graph back.
pub enum SendError {
    /// The command queue is full; retry later.
    Full(Box<CompiledGraph>),
    /// The graph reuses nodes from a graph that is no longer the latest
    /// sent. Build it again.
    Stale(Box<CompiledGraph>),
    /// The graph was compiled for another sample rate or for blocks shorter
    /// than [`EngineConfig::max_frames`].
    Config(Box<CompiledGraph>),
}

impl std::fmt::Debug for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Full(_) => "SendError::Full",
            Self::Stale(_) => "SendError::Stale",
            Self::Config(_) => "SendError::Config",
        })
    }
}

enum Command {
    Graph(Box<CompiledGraph>),
    Transport(TransportCommand),
    Timeline(Arc<Timeline>),
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
    counting_in: AtomicBool,
    latency: AtomicU32,
    swaps: AtomicU64,
    dropped: AtomicU64,
    /// Timelines a dropped controller still held. The audio thread may
    /// still swap them in, so they live until the processor is dropped.
    orphans: Mutex<Vec<Arc<Timeline>>>,
}

impl EngineController {
    /// Creates the two halves of an engine. Move the processor into the audio
    /// callback; keep the controller on a normal thread.
    pub fn new(config: EngineConfig) -> (Self, EngineProcessor) {
        let (commands_tx, commands_rx) = spsc(config.command_capacity);
        let (garbage_tx, garbage_rx) = deferred_drop(config.command_capacity);
        let shared = Arc::new(Shared::default());
        let timeline = Arc::new(Timeline::default());
        let processor = EngineProcessor {
            graph: None,
            pending: None,
            retired: None,
            commands: commands_rx,
            garbage: garbage_tx,
            transport: Transport::with_timeline(config.sample_rate, timeline.clone()),
            // One more for sources past `event_sources`.
            external: (0..config.event_sources.max(1) + 1)
                .map(|_| EventList::with_capacity(config.event_capacity))
                .collect(),
            // Zero would never advance through a cycle. No graph compiles
            // with it, so the processor outputs silence.
            max_frames: config.max_frames.max(1) as usize,
            shared: shared.clone(),
        };
        let controller = Self {
            config,
            commands: commands_tx,
            garbage: garbage_rx,
            shared,
            live: Arc::default(),
            timelines: vec![timeline],
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
    /// Keyed nodes of the latest graph sent.
    live: Arc<LiveNodes>,
    /// Every timeline sent that the audio thread may still hold, latest
    /// last. Holding a reference here means the audio thread never frees
    /// one when it swaps in the next; `collect_garbage` drops the rest.
    timelines: Vec<Arc<Timeline>>,
}

impl EngineController {
    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    /// An empty graph that can [`reuse`](GraphDesc::reuse) the keyed nodes
    /// of the latest graph sent. Its master strip is the running one when the
    /// channel count matches.
    pub fn graph(&self, inputs: u16, outputs: u16) -> GraphDesc {
        GraphDesc::with_live(inputs, outputs, self.live.clone())
    }

    /// Whether the latest graph sent has a node with `key`.
    pub fn has_node(&self, key: NodeKey) -> bool {
        self.live.nodes.contains_key(&key)
    }

    /// The meter attached to the node with `key` in the latest graph sent.
    pub fn meter(&self, key: NodeKey) -> Option<Arc<NodeMeter>> {
        self.live.nodes.get(&key)?.meter.clone()
    }

    /// Where the node with `key` sits in the latest graph sent.
    pub fn node_id(&self, key: NodeKey) -> Option<NodeId> {
        self.live.nodes.get(&key).map(|n| n.id)
    }

    /// Compiles `desc`. Fails with [`CompileError::StaleGraph`] if it reuses
    /// nodes and another graph was sent after it was made.
    pub fn compile(&self, desc: GraphDesc) -> Result<CompiledGraph, CompileError> {
        if desc.live.generation != self.live.generation
            && desc.nodes.iter().any(|n| n.reuse.is_some())
        {
            return Err(CompileError::StaleGraph);
        }
        let c = &self.config;
        CompiledGraph::compile(desc, c.sample_rate, c.max_frames, c.event_capacity)
    }

    /// Compiles `desc` and hands it to the audio thread, which swaps it in at
    /// the start of its next cycle. The graph it replaces comes back here and
    /// is dropped by [`collect_garbage`](Self::collect_garbage).
    pub fn set_graph(&mut self, desc: GraphDesc) -> Result<(), EngineError> {
        let graph = Box::new(self.compile(desc)?);
        self.send_graph(graph).map_err(|e| match e {
            SendError::Full(_) => EngineError::QueueFull,
            SendError::Stale(_) => EngineError::Compile(CompileError::StaleGraph),
            SendError::Config(_) => EngineError::Compile(CompileError::InvalidConfig),
        })
    }

    /// Sends an already compiled graph.
    ///
    /// A graph that reuses nodes must be sent right after the graph it was
    /// built from (the latest sent when its [`GraphDesc`] was made);
    /// otherwise it is refused with [`SendError::Stale`]. On a full queue it
    /// comes back in [`SendError::Full`] so the caller can retry. A graph
    /// compiled for another sample rate, or for blocks shorter than the
    /// engine's, is refused with [`SendError::Config`].
    pub fn send_graph(&mut self, graph: Box<CompiledGraph>) -> Result<(), SendError> {
        self.collect_garbage();
        if graph.sample_rate() != self.config.sample_rate
            || graph.max_frames() < self.config.max_frames
        {
            return Err(SendError::Config(graph));
        }
        if graph.reuses_nodes() && graph.base != self.live.generation {
            return Err(SendError::Stale(graph));
        }
        let live = graph.live.clone();
        match self.commands.push(Command::Graph(graph)) {
            Ok(()) => {
                self.live = live;
                Ok(())
            }
            Err(Command::Graph(g)) => Err(SendError::Full(g)),
            Err(_) => unreachable!("pushed a graph command"),
        }
    }

    pub fn transport(&mut self, cmd: TransportCommand) -> Result<(), EngineError> {
        self.send(Command::Transport(cmd))
    }

    pub fn play(&mut self) -> Result<(), EngineError> {
        self.transport(TransportCommand::Play)
    }

    /// Plays after a count-in of `bars` bars; see
    /// [`TransportCommand::PlayWithCountIn`].
    pub fn play_with_count_in(&mut self, bars: u32) -> Result<(), EngineError> {
        self.transport(TransportCommand::PlayWithCountIn(bars))
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

    /// Sets one constant tempo, keeping the time signatures. A bpm that is
    /// not positive and finite is ignored.
    pub fn set_bpm(&mut self, bpm: f64) -> Result<(), EngineError> {
        if !(bpm.is_finite() && bpm > 0.0) {
            return Ok(());
        }
        let meter = self.timeline().meter.clone();
        self.set_timeline(Timeline::new(SegmentTempoMap::new(bpm), meter))
    }

    /// Hands the audio thread a new tempo map and time signatures. From its
    /// next cycle every node sees bars and beats from it; the play position
    /// stays at the same sample.
    pub fn set_timeline(&mut self, timeline: Timeline) -> Result<(), EngineError> {
        self.collect_garbage();
        let timeline = Arc::new(timeline);
        self.send(Command::Timeline(timeline.clone()))?;
        self.timelines.push(timeline);
        Ok(())
    }

    /// The latest timeline sent.
    pub fn timeline(&self) -> &Timeline {
        self.timelines.last().expect("there is always a timeline")
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

    /// Like [`set_param`](Self::set_param), addressed by key.
    pub fn set_param_by_key(
        &mut self,
        key: NodeKey,
        param: ParamId,
        value: f64,
    ) -> Result<(), EngineError> {
        let node = self.node_id(key).ok_or(EngineError::UnknownKey(key))?;
        self.set_param(node, param, value)
    }

    /// Drops graphs and timelines the audio thread has retired. Returns how
    /// many graphs.
    pub fn collect_garbage(&mut self) -> usize {
        // A timeline only this side still references is no longer queued or
        // running, so it is freed here rather than on the audio thread.
        let last = self.timelines.len() - 1;
        let mut i = 0;
        self.timelines.retain(|t| {
            i += 1;
            i - 1 == last || Arc::strong_count(t) > 1
        });
        self.garbage.collect()
    }

    pub fn position(&self) -> SampleTime {
        SampleTime(self.shared.position.load(Ordering::Relaxed))
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Relaxed)
    }

    /// Whether the audio thread last reported a count-in under way.
    pub fn is_counting_in(&self) -> bool {
        self.shared.counting_in.load(Ordering::Relaxed)
    }

    /// Latency of the graph currently running.
    pub fn latency(&self) -> u32 {
        self.shared.latency.load(Ordering::Relaxed)
    }

    /// How many graphs the audio thread has swapped in.
    pub fn graphs_swapped(&self) -> u64 {
        self.shared.swaps.load(Ordering::Relaxed)
    }

    /// Parameter changes dropped because a node's queue was full, the node
    /// was not in the running graph, or it has no event input.
    pub fn dropped_params(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }

    fn send(&mut self, cmd: Command) -> Result<(), EngineError> {
        self.commands.push(cmd).map_err(|_| EngineError::QueueFull)
    }
}

impl Drop for EngineController {
    fn drop(&mut self) {
        // Timelines still queued for the audio thread must not be freed when
        // it replaces them; the processor's `shared` keeps them alive.
        let timelines = std::mem::take(&mut self.timelines);
        self.shared
            .orphans
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(timelines);
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
    /// This block's external events, one list per source, then one for
    /// every source past [`EngineConfig::event_sources`].
    external: Vec<EventList>,
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
        self.process_sources(frames, audio_in, audio_out, &[events]);
    }

    /// Like [`process`](Self::process), with external events from several
    /// sources. Source `i` reaches the nodes routed with
    /// [`GraphDesc::route_external_events_from`] for `i`, and every source
    /// reaches the nodes routed with
    /// [`route_external_events`](GraphDesc::route_external_events).
    pub fn process_sources(
        &mut self,
        frames: usize,
        audio_in: &[&[f32]],
        audio_out: &mut [&mut [f32]],
        sources: &[&EventList],
    ) {
        self.drain_commands();
        let mut offset = 0;
        while offset < frames {
            let len = self
                .transport
                .block_len((frames - offset).min(self.max_frames));
            let last = offset + len == frames;
            for list in &mut self.external {
                list.clear();
            }
            let overflow = self.external.len() - 1;
            for (i, events) in sources.iter().enumerate() {
                let list = &mut self.external[i.min(overflow)];
                for e in events.iter() {
                    let f = e.frame as usize;
                    if f >= offset && (f < offset + len || last) {
                        let frame = (f - offset).min(len - 1) as u32;
                        let _ = list.push(Event { frame, ..*e });
                    }
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
        self.shared
            .counting_in
            .store(self.transport.is_counting_in(), Ordering::Relaxed);
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
                // The controller still holds the old timeline, so this
                // doesn't free it.
                Command::Timeline(t) => drop(self.transport.set_timeline(t)),
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

    fn install(&mut self, mut g: Box<CompiledGraph>) {
        if let Some(old) = self.graph.as_deref_mut() {
            g.adopt(old);
        }
        self.shared.latency.store(g.latency(), Ordering::Relaxed);
        self.retired = self.graph.replace(g);
        self.shared.swaps.fetch_add(1, Ordering::Relaxed);
        self.retire();
    }

    fn retire(&mut self) {
        if let Some(g) = self.retired.take()
            && let Err(g) = self.garbage.retire(g)
        {
            self.retired = Some(g);
        }
    }
}

// Lives here rather than in tests/no_alloc.rs because it pushes graphs onto
// the command queue directly: send_graph collects garbage first, so from the
// public API the garbage queue only fills when the two threads race.
#[cfg(all(test, debug_assertions))]
mod tests {
    use assert_no_alloc::{AllocDisabler, assert_no_alloc, reset_violation_count, violation_count};

    use super::*;
    use crate::nodes::Strip;

    #[global_allocator]
    static ALLOC: AllocDisabler = AllocDisabler;

    /// Queues a graph without collecting garbage first, as a controller
    /// whose sends race the audio thread's retirements would.
    fn push_graph(ctl: &mut EngineController, desc: GraphDesc) {
        let g = Box::new(ctl.compile(desc).unwrap());
        assert!(ctl.commands.push(Command::Graph(g)).is_ok());
    }

    fn process(proc: &mut EngineProcessor, out: &mut [f32]) -> u32 {
        let input = [1.0f32; 32];
        let events = EventList::with_capacity(0);
        reset_violation_count();
        assert_no_alloc(|| proc.process(32, &[&input], &mut [out], &events));
        violation_count()
    }

    #[test]
    fn swaps_with_a_full_garbage_queue_neither_free_nor_reorder() {
        const CAPACITY: usize = 4;
        let (mut ctl, mut proc) = EngineController::new(EngineConfig {
            max_frames: 32,
            command_capacity: CAPACITY,
            ..Default::default()
        });
        let mut out = [0.0f32; 32];
        let plain = || {
            let mut g = GraphDesc::new(1, 1);
            g.connect(g.input(), g.master());
            g
        };

        push_graph(&mut ctl, plain());
        assert_eq!(process(&mut proc, &mut out), 0);
        // Each swap retires the graph before it; these fill the garbage queue.
        for _ in 0..CAPACITY {
            push_graph(&mut ctl, plain());
        }
        assert_eq!(process(&mut proc, &mut out), 0);
        assert_eq!(ctl.graphs_swapped(), 1 + CAPACITY as u64);

        // The first swap below cannot retire its old graph, so the processor
        // holds it and `last` has to wait. The change for `track`, which
        // only exists in `last`, and the graph after it must wait behind
        // `last`: run early, the change would be dropped and the second
        // pending graph would free `last` on this thread.
        push_graph(&mut ctl, plain());
        let mut last = GraphDesc::new(1, 1);
        let track = last.add_track("track", 1);
        last.connect(last.input(), track);
        last.connect(track, last.master());
        push_graph(&mut ctl, last);
        ctl.set_param(track, Strip::MUTE, 1.0).unwrap();
        push_graph(&mut ctl, plain());
        for _ in 0..2 {
            assert_eq!(process(&mut proc, &mut out), 0);
            assert_eq!(ctl.graphs_swapped(), 2 + CAPACITY as u64);
            assert!(proc.retired.is_some() && proc.pending.is_some());
        }

        assert_eq!(ctl.collect_garbage(), CAPACITY);
        assert_eq!(process(&mut proc, &mut out), 0);
        assert_eq!(ctl.graphs_swapped(), 4 + CAPACITY as u64);
        assert_eq!(ctl.dropped_params(), 0, "the change reached `last`");
        assert!(proc.retired.is_none() && proc.pending.is_none());

        // Every replaced graph comes back to be freed here.
        assert_eq!(ctl.collect_garbage(), 3);
        drop(proc);
    }

    #[test]
    fn timelines_outlive_a_dropped_controller() {
        let (mut ctl, mut proc) = EngineController::new(EngineConfig {
            max_frames: 32,
            ..Default::default()
        });
        ctl.set_timeline(Timeline::constant(90.0, 3, 4)).unwrap();
        ctl.set_timeline(Timeline::constant(100.0, 5, 4)).unwrap();
        drop(ctl);
        let mut out = [0.0f32; 32];
        assert_eq!(process(&mut proc, &mut out), 0);
        assert_eq!(proc.transport().state().bpm, 100.0);
    }

    #[test]
    fn timeline_swaps_free_nothing_on_the_audio_thread() {
        let (mut ctl, mut proc) = EngineController::new(EngineConfig {
            max_frames: 32,
            ..Default::default()
        });
        let mut out = [0.0f32; 32];
        assert_eq!(process(&mut proc, &mut out), 0);
        // The first replaces the timeline the engine started with, which
        // only the controller and processor ever held.
        ctl.set_timeline(Timeline::constant(90.0, 3, 4)).unwrap();
        ctl.set_bpm(150.0).unwrap();
        ctl.set_bpm(f64::NAN).unwrap();
        assert_eq!(ctl.timelines.len(), 3);
        assert_eq!(process(&mut proc, &mut out), 0);
        let state = proc.transport().state();
        assert_eq!(state.bpm, 150.0);
        assert_eq!(state.music.numerator, 3, "set_bpm keeps the meter");
        assert_eq!(ctl.timeline(), proc.transport().timeline());

        // Only the running timeline is left to keep.
        ctl.collect_garbage();
        assert_eq!(ctl.timelines.len(), 1);
        assert_eq!(std::sync::Arc::strong_count(&ctl.timelines[0]), 2);
    }
}
