//! The `Node` contract: one unit of audio processing in the graph.

use crate::event::EventList;
use crate::time::SampleTime;

/// Where the transport is at the start of the current buffer.
#[derive(Copy, Clone, Debug, Default)]
pub struct TransportState {
    pub playing: bool,
    pub position: SampleTime,
    pub bpm: f64,
    pub sample_rate: f64,
}

/// How many audio channels a node takes and produces, and whether it uses
/// events. Fixed between [`Node::prepare`] calls.
#[derive(Clone, Debug, Default)]
pub struct PortLayout {
    pub audio_in: u16,
    pub audio_out: u16,
    pub event_in: bool,
    pub event_out: bool,
}

/// Everything a node sees for one buffer. Every audio slice is exactly
/// `frames` long; there is one slice per channel in [`PortLayout`].
pub struct ProcessContext<'a> {
    pub frames: u32,
    pub transport: &'a TransportState,
    pub audio_in: &'a [&'a [f32]],
    pub audio_out: &'a mut [&'a mut [f32]],
    pub events_in: &'a EventList,
    pub events_out: &'a mut EventList,
}

/// A unit of audio processing: track, bus, built-in device or plugin.
///
/// `ports`, `prepare` and `latency` run off the audio thread and may
/// allocate. `process` and `reset` run on the audio thread and must not
/// allocate, lock, log or do I/O.
pub trait Node: Send {
    fn ports(&self) -> PortLayout;
    /// May allocate.
    fn prepare(&mut self, sample_rate: f64, max_frames: u32);
    /// Real-time safe.
    fn process(&mut self, ctx: &mut ProcessContext);
    /// Latency this node adds, in frames, for delay compensation.
    fn latency(&self) -> u32 {
        0
    }
    /// Clear internal state (delay lines, envelopes) without reallocating.
    fn reset(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Gain(f32);

    impl Node for Gain {
        fn ports(&self) -> PortLayout {
            PortLayout {
                audio_in: 1,
                audio_out: 1,
                ..PortLayout::default()
            }
        }
        fn prepare(&mut self, _sample_rate: f64, _max_frames: u32) {}
        fn process(&mut self, ctx: &mut ProcessContext) {
            for (input, output) in ctx.audio_in.iter().zip(ctx.audio_out.iter_mut()) {
                for (i, o) in input.iter().zip(output.iter_mut()) {
                    *o = i * self.0;
                }
            }
        }
    }

    #[test]
    fn the_contract_is_implementable_and_callable() {
        let mut node: Box<dyn Node> = Box::new(Gain(0.5));
        node.prepare(48_000.0, 4);
        let input = [1.0, -1.0, 0.5, 0.0];
        let mut output = [0.0; 4];
        let events_in = EventList::with_capacity(0);
        let mut events_out = EventList::with_capacity(0);
        let transport = TransportState::default();
        let audio_in = [&input[..]];
        let mut audio_out = [&mut output[..]];
        node.process(&mut ProcessContext {
            frames: 4,
            transport: &transport,
            audio_in: &audio_in,
            audio_out: &mut audio_out,
            events_in: &events_in,
            events_out: &mut events_out,
        });
        assert_eq!(output, [0.5, -0.5, 0.25, 0.0]);
    }
}
