use daw_core::node::Node;

use crate::nodes::Strip;

/// Index of a node inside one [`GraphDesc`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub u32);

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
}

impl GraphDesc {
    /// A graph with `inputs` external input channels and a master strip with
    /// `outputs` channels.
    pub fn new(inputs: u16, outputs: u16) -> Self {
        let mut g = Self {
            nodes: Vec::new(),
            audio_edges: Vec::new(),
            event_edges: Vec::new(),
            external_events: Vec::new(),
            input: NodeId(0),
            master: NodeId(0),
        };
        g.input = g.add_node(NodeKind::Input, "input", Box::new(InputNode(inputs)));
        g.master = g.add_node(NodeKind::Master, "master", Box::new(Strip::master(outputs)));
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
        });
        id
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

/// Placeholder node for external input. The processor writes the backend's
/// input channels into its output buffers instead of calling `process`.
struct InputNode(u16);

impl Node for InputNode {
    fn ports(&self) -> daw_core::node::PortLayout {
        daw_core::node::PortLayout {
            audio_out: self.0,
            ..Default::default()
        }
    }

    fn prepare(&mut self, _sample_rate: f64, _max_frames: u32) {}

    fn process(&mut self, _ctx: &mut daw_core::node::ProcessContext) {}
}
