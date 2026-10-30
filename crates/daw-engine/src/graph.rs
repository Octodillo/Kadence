use std::collections::HashMap;
use std::sync::Arc;

use daw_core::event::ParamId;
use daw_core::node::{Node, PortLayout, ProcessContext};

use crate::automation::AutomationLane;
use crate::meter::NodeMeter;
use crate::nodes::Strip;

/// Index of a node inside one [`GraphDesc`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub u32);

/// A caller-chosen id that names a node across graphs, so a node can be
/// carried from the running graph into the next one with
/// [`GraphDesc::reuse`]. Wide enough to hold a ULID or UUID.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeKey(pub u128);

impl NodeKey {
    /// The key [`EngineController::graph`](crate::EngineController::graph)
    /// gives the master strip, so its state survives graph swaps.
    pub const MASTER: NodeKey = NodeKey(u128::MAX);
}

/// What a node is for. The compiler treats `Input` and `Master` specially;
/// the rest are tags for introspection and UI.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum NodeKind {
    /// External audio in: its outputs are the backend's input channels.
    Input,
    /// An instrument, effect or plugin.
    Device,
    /// A track's channel strip.
    Track,
    /// A bus (group or return) channel strip.
    Bus,
    /// The gain stage of a send between a track and a bus.
    Send,
    /// The master strip: its outputs are the engine's outputs.
    Master,
}

/// One channel of one node's audio output feeding one channel of another
/// node's audio input. Several edges into the same input channel are summed
/// in the order they were added.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct AudioEdge {
    pub from: NodeId,
    pub from_channel: u16,
    pub to: NodeId,
    pub to_channel: u16,
}

/// All events a node outputs are merged into another node's event input.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct EventEdge {
    pub from: NodeId,
    pub to: NodeId,
}

pub(crate) struct NodeEntry {
    pub kind: NodeKind,
    pub name: String,
    pub node: Box<dyn Node>,
    pub key: Option<NodeKey>,
    /// Step index in the running graph of the node to adopt in place of
    /// `node`, which is then only a placeholder.
    pub reuse: Option<usize>,
    pub meter: Option<Arc<NodeMeter>>,
    pub automation: Vec<(ParamId, Arc<AutomationLane>)>,
}

/// What the controller remembers about a keyed node in the graph it sent
/// last.
#[derive(Clone)]
pub(crate) struct LiveNode {
    pub id: NodeId,
    pub step: usize,
    pub kind: NodeKind,
    pub name: String,
    pub ports: PortLayout,
    pub latency: u32,
    pub meter: Option<Arc<NodeMeter>>,
}

/// The keyed nodes of one sent graph.
#[derive(Default)]
pub(crate) struct LiveNodes {
    pub generation: u64,
    pub nodes: HashMap<NodeKey, LiveNode>,
}

/// A description of an audio graph, built on a normal thread and handed to
/// [`EngineController::set_graph`](crate::EngineController::set_graph) or
/// [`render`](crate::render). Validation happens when it is compiled.
pub struct GraphDesc {
    pub(crate) nodes: Vec<NodeEntry>,
    pub(crate) audio_edges: Vec<AudioEdge>,
    pub(crate) event_edges: Vec<EventEdge>,
    pub(crate) external_events: Vec<NodeId>,
    pub(crate) input: NodeId,
    pub(crate) master: NodeId,
    pub(crate) live: Arc<LiveNodes>,
}

impl GraphDesc {
    /// A graph with `inputs` external input channels and a master strip with
    /// `outputs` channels.
    pub fn new(inputs: u16, outputs: u16) -> Self {
        Self::with_live(inputs, outputs, Arc::default())
    }

    /// Like [`new`](Self::new), but nodes of the graph `live` describes can
    /// be reused, and the master strip is when its channel count matches.
    pub(crate) fn with_live(inputs: u16, outputs: u16, live: Arc<LiveNodes>) -> Self {
        let mut g = Self {
            nodes: Vec::new(),
            audio_edges: Vec::new(),
            event_edges: Vec::new(),
            external_events: Vec::new(),
            input: NodeId(0),
            master: NodeId(0),
            live,
        };
        g.input = g.add_node(NodeKind::Input, "input", Box::new(InputNode(inputs)));
        let reusable = g
            .live
            .nodes
            .get(&NodeKey::MASTER)
            .is_some_and(|m| m.kind == NodeKind::Master && m.ports.audio_out == outputs);
        g.master = if reusable {
            g.reuse(NodeKey::MASTER).expect("checked above")
        } else {
            let id = g.add_node(NodeKind::Master, "master", Box::new(Strip::master(outputs)));
            g.set_key(id, NodeKey::MASTER);
            id
        };
        g
    }

    pub fn input(&self) -> NodeId {
        self.input
    }

    pub fn master(&self) -> NodeId {
        self.master
    }

    pub fn add_node(
        &mut self,
        kind: NodeKind,
        name: impl Into<String>,
        node: Box<dyn Node>,
    ) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(NodeEntry {
            kind,
            name: name.into(),
            node,
            key: None,
            reuse: None,
            meter: None,
            automation: Vec::new(),
        });
        id
    }

    /// Adds the node that has `key` in the running graph, with its state,
    /// ports, latency, kind, name and meter. The audio thread moves it over
    /// when it swaps this graph in, without preparing it again. Edges are
    /// not carried over: connect it like a new node (a reused track or bus
    /// is not routed to master on its own). Neither are automation lanes:
    /// attach them again with [`automate`](Self::automate).
    ///
    /// The node's latency is the value it reported when it was first
    /// compiled, and it is not prepared again. Don't reuse plugins: add a
    /// fresh [`PluginHandle::node`](crate::PluginHandle::node) to each graph,
    /// so compensation follows their latency changes.
    ///
    /// Returns `None` if the graph was not made by
    /// [`EngineController::graph`](crate::EngineController::graph), no node
    /// with `key` is running, or this graph already has a node with `key`.
    pub fn reuse(&mut self, key: NodeKey) -> Option<NodeId> {
        if self.nodes.iter().any(|n| n.key == Some(key)) {
            return None;
        }
        let live = self.live.nodes.get(&key)?.clone();
        let id = self.add_node(
            live.kind,
            live.name,
            Box::new(Reused {
                ports: live.ports,
                latency: live.latency,
            }),
        );
        let entry = &mut self.nodes[id.0 as usize];
        entry.key = Some(key);
        entry.reuse = Some(live.step);
        entry.meter = live.meter;
        Some(id)
    }

    /// Whether [`reuse`](Self::reuse) would succeed for `key`.
    pub fn can_reuse(&self, key: NodeKey) -> bool {
        self.live.nodes.contains_key(&key) && !self.nodes.iter().any(|n| n.key == Some(key))
    }

    /// Names `id` so a later graph can [`reuse`](Self::reuse) it. Keys must
    /// be unique within a graph; compiling fails otherwise.
    pub fn set_key(&mut self, id: NodeId, key: NodeKey) {
        if let Some(n) = self.nodes.get_mut(id.0 as usize) {
            n.key = Some(key);
        }
    }

    pub fn key(&self, id: NodeId) -> Option<NodeKey> {
        self.nodes.get(id.0 as usize).and_then(|n| n.key)
    }

    pub fn rename(&mut self, id: NodeId, name: impl Into<String>) {
        if let Some(n) = self.nodes.get_mut(id.0 as usize) {
            n.name = name.into();
        }
    }

    /// Meters the audio output of `id` after every block. A reused node keeps
    /// its meter unless this replaces it.
    pub fn set_meter(&mut self, id: NodeId, meter: Arc<NodeMeter>) {
        if let Some(n) = self.nodes.get_mut(id.0 as usize) {
            n.meter = Some(meter);
        }
    }

    pub fn meter(&self, id: NodeId) -> Option<&Arc<NodeMeter>> {
        self.nodes.get(id.0 as usize)?.meter.as_ref()
    }

    /// Plays `lane` into parameter `param` of `id` while the transport runs,
    /// as `ParamValue` events at exact frames. The node needs an event input.
    ///
    /// While playing, the lane sends the current value at the start of
    /// every block, so it overrides manual changes to the same parameter
    /// (automation "read" mode). To let a user take over, leave the lane out
    /// of the next graph.
    pub fn automate(&mut self, id: NodeId, param: ParamId, lane: impl Into<Arc<AutomationLane>>) {
        if let Some(n) = self.nodes.get_mut(id.0 as usize) {
            n.automation.retain(|(p, _)| *p != param);
            n.automation.push((param, lane.into()));
        }
    }

    pub fn add_device(&mut self, name: impl Into<String>, node: Box<dyn Node>) -> NodeId {
        self.add_node(NodeKind::Device, name, node)
    }

    /// A track strip, routed to master.
    pub fn add_track(&mut self, name: impl Into<String>, channels: u16) -> NodeId {
        let id = self.add_node(NodeKind::Track, name, Box::new(Strip::track(channels)));
        self.connect(id, self.master);
        id
    }

    /// A bus strip, routed to master.
    pub fn add_bus(&mut self, name: impl Into<String>, channels: u16) -> NodeId {
        let id = self.add_node(NodeKind::Bus, name, Box::new(Strip::bus(channels)));
        self.connect(id, self.master);
        id
    }

    /// A send from `from` into `to` at linear `gain`. Returns the send's node,
    /// whose gain can be changed with a `ParamValue` event on
    /// [`Strip::GAIN`].
    pub fn add_send(&mut self, from: NodeId, to: NodeId, gain: f32) -> NodeId {
        let channels = self.channels_between(from, to);
        let id = self.add_node(
            NodeKind::Send,
            "send",
            Box::new(Strip::send(channels, gain)),
        );
        self.connect(from, id);
        self.connect(id, to);
        id
    }

    pub fn connect_audio(&mut self, from: NodeId, from_channel: u16, to: NodeId, to_channel: u16) {
        self.audio_edges.push(AudioEdge {
            from,
            from_channel,
            to,
            to_channel,
        });
    }

    /// Connects channel `i` of `from` to channel `i` of `to` for every channel
    /// both have.
    pub fn connect(&mut self, from: NodeId, to: NodeId) {
        for ch in 0..self.channels_between(from, to) {
            self.connect_audio(from, ch, to, ch);
        }
    }

    pub fn connect_events(&mut self, from: NodeId, to: NodeId) {
        self.event_edges.push(EventEdge { from, to });
    }

    /// Delivers the events passed to
    /// [`EngineProcessor::process`](crate::EngineProcessor::process) (backend
    /// MIDI, UI parameter changes) to `to`.
    pub fn route_external_events(&mut self, to: NodeId) {
        self.external_events.push(to);
    }

    pub fn kind(&self, id: NodeId) -> Option<NodeKind> {
        self.nodes.get(id.0 as usize).map(|n| n.kind)
    }

    pub fn name(&self, id: NodeId) -> Option<&str> {
        self.nodes.get(id.0 as usize).map(|n| n.name.as_str())
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn audio_edges(&self) -> &[AudioEdge] {
        &self.audio_edges
    }

    pub fn event_edges(&self) -> &[EventEdge] {
        &self.event_edges
    }

    fn channels_between(&self, from: NodeId, to: NodeId) -> u16 {
        let outs = self
            .nodes
            .get(from.0 as usize)
            .map_or(0, |n| n.node.ports().audio_out);
        let ins = self
            .nodes
            .get(to.0 as usize)
            .map_or(0, |n| n.node.ports().audio_in);
        outs.min(ins)
    }
}

/// Stands in for a reused node until the audio thread swaps the real one in.
struct Reused {
    ports: PortLayout,
    latency: u32,
}

impl Node for Reused {
    fn ports(&self) -> PortLayout {
        self.ports.clone()
    }

    fn prepare(&mut self, _sample_rate: f64, _max_frames: u32) {}

    fn process(&mut self, _ctx: &mut ProcessContext) {}

    fn latency(&self) -> u32 {
        self.latency
    }
}

/// Placeholder node for external input. The processor writes the backend's
/// input channels into its output buffers instead of calling `process`.
struct InputNode(u16);

impl Node for InputNode {
    fn ports(&self) -> PortLayout {
        PortLayout {
            audio_out: self.0,
            ..Default::default()
        }
    }

    fn prepare(&mut self, _sample_rate: f64, _max_frames: u32) {}

    fn process(&mut self, _ctx: &mut ProcessContext) {}
}
