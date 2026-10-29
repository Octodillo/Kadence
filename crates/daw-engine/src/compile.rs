use std::cmp::Reverse;
use std::collections::BinaryHeap;

use daw_core::event::{Event, EventList};
use daw_core::node::{Node, ProcessContext, TransportState};

use crate::delay::DelayLine;
use crate::graph::{GraphDesc, NodeId, NodeKind};

/// Most audio channels one node may have on either side.
pub const MAX_CHANNELS: usize = 64;

/// Parameter changes queued for one node between two cycles.
const PARAM_QUEUE: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompileError {
    #[error("an edge refers to node {0:?}, which is not in the graph")]
    UnknownNode(NodeId),
    #[error("node {node:?} has no audio output channel {channel}")]
    NoSuchOutput { node: NodeId, channel: u16 },
    #[error("node {node:?} has no audio input channel {channel}")]
    NoSuchInput { node: NodeId, channel: u16 },
    #[error("node {0:?} has no event output")]
    NoEventOutput(NodeId),
    #[error("node {0:?} has no event input")]
    NoEventInput(NodeId),
    #[error("the graph has a cycle through node {0:?}")]
    Cycle(NodeId),
    #[error("node {node:?} has {channels} channels; the limit is {MAX_CHANNELS}")]
    TooManyChannels { node: NodeId, channels: u16 },
    #[error("sample rate and block size must be positive")]
    InvalidConfig,
}

/// One summing input of a step: a source channel, optionally through a
/// latency compensation delay.
struct Source {
    dst: usize,
    src_step: usize,
    src: usize,
    delay: Option<usize>,
}

pub(crate) struct Step {
    id: NodeId,
    kind: NodeKind,
    node: Box<dyn Node>,
    n_in: usize,
    n_out: usize,
    /// `n_in` channels of `max_frames` samples each, back to back.
    inputs: Vec<f32>,
    outputs: Vec<f32>,
    sources: Vec<Source>,
    event_sources: Vec<usize>,
    external_events: bool,
    events_in: EventList,
    events_out: EventList,
    params: EventList,
    latency: u32,
}

/// A graph ready to run on the RT thread: nodes in process order, every
/// buffer and delay line preallocated.
pub struct CompiledGraph {
    steps: Vec<Step>,
    delays: Vec<DelayLine>,
    /// `NodeId` to step index.
    index: Vec<usize>,
    master: usize,
    max_frames: usize,
    sample_rate: f64,
    latency: u32,
}

impl CompiledGraph {
    /// Checks a graph without consuming it, so a caller can keep its nodes
    /// when the graph is invalid.
    pub fn validate(desc: &GraphDesc) -> Result<(), CompileError> {
        let n = desc.nodes.len();
        let ports: Vec<_> = desc.nodes.iter().map(|e| e.node.ports()).collect();
        let check = |id: NodeId| {
            if (id.0 as usize) < n {
                Ok(id.0 as usize)
            } else {
                Err(CompileError::UnknownNode(id))
            }
        };
        for (i, p) in ports.iter().enumerate() {
            let channels = p.audio_in.max(p.audio_out);
            if channels as usize > MAX_CHANNELS {
                return Err(CompileError::TooManyChannels {
                    node: NodeId(i as u32),
                    channels,
                });
            }
        }
        for e in &desc.audio_edges {
            if e.from_channel >= ports[check(e.from)?].audio_out {
                return Err(CompileError::NoSuchOutput {
                    node: e.from,
                    channel: e.from_channel,
                });
            }
            if e.to_channel >= ports[check(e.to)?].audio_in {
                return Err(CompileError::NoSuchInput {
                    node: e.to,
                    channel: e.to_channel,
                });
            }
        }
        for e in &desc.event_edges {
            if !ports[check(e.from)?].event_out {
                return Err(CompileError::NoEventOutput(e.from));
            }
            if !ports[check(e.to)?].event_in {
                return Err(CompileError::NoEventInput(e.to));
            }
        }
        for &id in &desc.external_events {
            if !ports[check(id)?].event_in {
                return Err(CompileError::NoEventInput(id));
            }
        }
        topo_order(desc).map(|_| ())
    }

    /// Validates `desc`, calls `prepare` on every node, sorts the nodes,
    /// allocates buffers and inserts latency compensation delays.
    ///
    /// Runs off the RT thread; it allocates freely.
    pub fn compile(
        desc: GraphDesc,
        sample_rate: f64,
        max_frames: u32,
        event_capacity: usize,
    ) -> Result<Self, CompileError> {
        if sample_rate.is_nan() || sample_rate <= 0.0 || max_frames == 0 {
            return Err(CompileError::InvalidConfig);
        }
        Self::validate(&desc)?;
        let order = topo_order(&desc)?;
        let mf = max_frames as usize;

        let mut index = vec![0; desc.nodes.len()];
        for (step, &node) in order.iter().enumerate() {
            index[node] = step;
        }

        let GraphDesc {
            nodes,
            audio_edges,
            event_edges,
            external_events,
            master,
            ..
        } = desc;
        let mut slots: Vec<Option<_>> = nodes.into_iter().map(Some).collect();
        let mut steps: Vec<Step> = order
            .iter()
            .map(|&i| {
                let mut entry = slots[i]
                    .take()
                    .expect("each node appears once in the order");
                entry.node.prepare(sample_rate, max_frames);
                let ports = entry.node.ports();
                let (n_in, n_out) = (ports.audio_in as usize, ports.audio_out as usize);
                Step {
                    id: NodeId(i as u32),
                    kind: entry.kind,
                    latency: entry.node.latency(),
                    node: entry.node,
                    n_in,
                    n_out,
                    inputs: vec![0.0; n_in * mf],
                    outputs: vec![0.0; n_out * mf],
                    sources: Vec::new(),
                    event_sources: Vec::new(),
                    external_events: external_events.iter().any(|e| e.0 as usize == i),
                    events_in: EventList::with_capacity(if ports.event_in {
                        event_capacity
                    } else {
                        0
                    }),
                    events_out: EventList::with_capacity(if ports.event_out {
                        event_capacity
                    } else {
                        0
                    }),
                    params: EventList::with_capacity(PARAM_QUEUE),
                }
            })
            .collect();

        // Latency in topological order: a node's output is late by the
        // latest of its audio inputs plus its own latency. Every edge that
        // arrives early is delayed to match the latest one.
        let mut out_latency = vec![0u32; steps.len()];
        let mut in_latency = vec![0u32; steps.len()];
        for s in 0..steps.len() {
            let latest = audio_edges
                .iter()
                .filter(|e| index[e.to.0 as usize] == s)
                .map(|e| out_latency[index[e.from.0 as usize]])
                .max()
                .unwrap_or(0);
            in_latency[s] = latest;
            out_latency[s] = latest.saturating_add(steps[s].latency);
        }

        let mut delays = Vec::new();
        for e in &audio_edges {
            let (src_step, dst_step) = (index[e.from.0 as usize], index[e.to.0 as usize]);
            let lag = in_latency[dst_step] - out_latency[src_step];
            let delay = (lag > 0).then(|| {
                delays.push(DelayLine::new(lag as usize));
                delays.len() - 1
            });
            steps[dst_step].sources.push(Source {
                dst: e.to_channel as usize * mf,
                src_step,
                src: e.from_channel as usize * mf,
                delay,
            });
        }
        for e in &event_edges {
            let src = index[e.from.0 as usize];
            steps[index[e.to.0 as usize]].event_sources.push(src);
        }

        let master = index[master.0 as usize];
        Ok(Self {
            latency: out_latency[master],
            steps,
            delays,
            index,
            master,
            max_frames: mf,
            sample_rate,
        })
    }

    /// Samples from the earliest input to the master output.
    pub fn latency(&self) -> u32 {
        self.latency
    }

    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    pub fn max_frames(&self) -> u32 {
        self.max_frames as u32
    }

    /// Node ids in the order they are processed.
    pub fn order(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.steps.iter().map(|s| s.id)
    }

    /// Number of latency compensation delay lines inserted.
    pub fn delay_lines(&self) -> usize {
        self.delays.len()
    }

    pub fn output_channels(&self) -> usize {
        self.steps[self.master].n_out
    }

    /// Calls `reset` on every node and clears every delay line.
    pub fn reset(&mut self) {
        for s in &mut self.steps {
            s.node.reset();
        }
        for d in &mut self.delays {
            d.reset();
        }
    }

    /// Queues a parameter change for `node`, applied at the start of the next
    /// cycle. RT-safe. Returns `false` if the node is unknown or its queue is
    /// full.
    pub(crate) fn queue_param(&mut self, node: NodeId, param: u32, value: f64) -> bool {
        let Some(&step) = self.index.get(node.0 as usize) else {
            return false;
        };
        let kind = daw_core::event::EventKind::ParamValue { param, value };
        self.steps[step]
            .params
            .push(Event { frame: 0, kind })
            .is_ok()
    }

    /// The master's output for the last processed block.
    pub(crate) fn output(&self, channel: usize, frames: usize) -> &[f32] {
        let start = channel * self.max_frames;
        &self.steps[self.master].outputs[start..start + frames]
    }

    /// Runs every node once for `frames` samples (at most `max_frames`).
    /// `audio_in[ch][offset..offset + frames]` feeds the input node.
    /// RT-safe.
    pub(crate) fn process(
        &mut self,
        transport: &TransportState,
        frames: usize,
        audio_in: &[&[f32]],
        offset: usize,
        external: &EventList,
    ) {
        debug_assert!(frames <= self.max_frames);
        let mf = self.max_frames;
        let Self { steps, delays, .. } = self;
        for i in 0..steps.len() {
            let (done, rest) = steps.split_at_mut(i);
            let step = &mut rest[0];
            step.events_out.clear();

            if step.kind == NodeKind::Input {
                for (ch, out) in step.outputs.chunks_exact_mut(mf).enumerate() {
                    match audio_in
                        .get(ch)
                        .and_then(|c| c.get(offset..offset + frames))
                    {
                        Some(src) => out[..frames].copy_from_slice(src),
                        None => out[..frames].fill(0.0),
                    }
                }
                continue;
            }

            for ch in step.inputs.chunks_exact_mut(mf) {
                ch[..frames].fill(0.0);
            }
            for s in &step.sources {
                let src = &done[s.src_step].outputs[s.src..s.src + frames];
                let dst = &mut step.inputs[s.dst..s.dst + frames];
                match s.delay {
                    Some(d) => delays[d].mix_into(src, dst),
                    None => {
                        for (d, x) in dst.iter_mut().zip(src) {
                            *d += *x;
                        }
                    }
                }
            }

            // Events that don't fit are dropped: a full list must not
            // allocate.
            step.events_in.clear();
            for e in step.params.iter() {
                let _ = step.events_in.push(*e);
            }
            step.params.clear();
            if step.external_events {
                for e in external.iter() {
                    let _ = step.events_in.push(*e);
                }
            }
            for &src in &step.event_sources {
                for e in done[src].events_out.iter() {
                    let _ = step.events_in.push(*e);
                }
            }

            let mut ins: [&[f32]; MAX_CHANNELS] = [&[]; MAX_CHANNELS];
            for (slot, ch) in ins.iter_mut().zip(step.inputs.chunks_exact(mf)) {
                *slot = &ch[..frames];
            }
            let mut outs: [&mut [f32]; MAX_CHANNELS] =
                std::array::from_fn(|_| <&mut [f32]>::default());
            for (slot, ch) in outs.iter_mut().zip(step.outputs.chunks_exact_mut(mf)) {
                let ch = &mut ch[..frames];
                ch.fill(0.0);
                *slot = ch;
            }
            let mut ctx = ProcessContext {
                frames: frames as u32,
                transport,
                audio_in: &ins[..step.n_in],
                audio_out: &mut outs[..step.n_out],
                events_in: &step.events_in,
                events_out: &mut step.events_out,
            };
            step.node.process(&mut ctx);
        }
    }
}

/// Kahn's algorithm over audio and event edges. Ties go to the lowest
/// `NodeId`, so the order (and therefore summing order) is deterministic.
fn topo_order(desc: &GraphDesc) -> Result<Vec<usize>, CompileError> {
    let n = desc.nodes.len();
    let mut indegree = vec![0usize; n];
    let mut next: Vec<Vec<usize>> = vec![Vec::new(); n];
    let edges = desc
        .audio_edges
        .iter()
        .map(|e| (e.from, e.to))
        .chain(desc.event_edges.iter().map(|e| (e.from, e.to)));
    for (from, to) in edges {
        let (from, to) = (from.0 as usize, to.0 as usize);
        if from >= n || to >= n {
            return Err(CompileError::UnknownNode(NodeId(from.max(to) as u32)));
        }
        next[from].push(to);
        indegree[to] += 1;
    }
    let mut ready: BinaryHeap<Reverse<usize>> =
        (0..n).filter(|&i| indegree[i] == 0).map(Reverse).collect();
    let mut order = Vec::with_capacity(n);
    while let Some(Reverse(i)) = ready.pop() {
        order.push(i);
        for &j in &next[i] {
            indegree[j] -= 1;
            if indegree[j] == 0 {
                ready.push(Reverse(j));
            }
        }
    }
    if order.len() < n {
        let stuck = (0..n).find(|&i| indegree[i] > 0).unwrap_or(0);
        return Err(CompileError::Cycle(NodeId(stuck as u32)));
    }
    Ok(order)
}
