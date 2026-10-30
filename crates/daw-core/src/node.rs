//! The `Node` contract: one unit of audio processing in the graph.

use crate::event::EventList;
use crate::time::{MusicalPosition, SampleTime, Timeline};

/// Where the transport is at the start of the current buffer.
///
/// The engine fills in every field from the project's [`Timeline`] once per
/// block, so the metronome and every plugin host see the same bars and
/// beats. Read the musical position from `music`; don't derive it from
/// `position` and `bpm`, which ignores tempo changes and time signatures.
#[derive(Copy, Clone, Debug, Default)]
pub struct TransportState {
    pub playing: bool,
    pub position: SampleTime,
    /// The tempo at `position`. Zero means unknown.
    pub bpm: f64,
    /// Zero means unknown; nodes then use the rate they were prepared with.
    pub sample_rate: f64,
    /// Bars and beats at `position`.
    pub music: MusicalPosition,
}

impl TransportState {
    /// The state at `position` on `timeline`.
    pub fn at(timeline: &Timeline, playing: bool, position: SampleTime, sample_rate: f64) -> Self {
        Self {
            playing,
            position,
            bpm: timeline.bpm_at(position, sample_rate),
            sample_rate,
            music: timeline.position(position, sample_rate),
        }
    }

    /// Quarter notes per sample at `position`, or zero when the tempo or
    /// sample rate is unknown. Nodes use it to place beats inside the block;
    /// the next block starts from the engine's exact position again.
    pub fn beats_per_sample(&self) -> f64 {
        if self.bpm > 0.0 && self.sample_rate > 0.0 {
            self.bpm / 60.0 / self.sample_rate
        } else {
            0.0
        }
    }

    /// The state `frames` samples into the block, for a node that splits
    /// its block. While playing, the position and beats move on at this
    /// block's tempo and bars roll over assuming the time signature holds.
    /// A stopped transport stays where it is.
    pub fn advanced(&self, frames: u32) -> Self {
        let mut t = *self;
        if !self.playing || frames == 0 {
            return t;
        }
        t.position = SampleTime(self.position.0 + i64::from(frames));
        let m = &mut t.music;
        m.beats += f64::from(frames) * self.beats_per_sample();
        let full = f64::from(m.numerator.max(1)) * m.beat_len();
        let end = m.bar_start + m.bar_len;
        if m.bar_len > 0.0 && m.beats >= end {
            let more = ((m.beats - end) / full).floor();
            #[allow(clippy::cast_possible_truncation)]
            {
                m.bar += 1 + more as i64;
            }
            m.bar_start = end + more * full;
            m.bar_len = full;
        }
        t
    }
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
    fn advanced_moves_beats_and_rolls_bars() {
        use crate::time::Timeline;
        let tl = Timeline::constant(120.0, 3, 4);
        // Half a beat before the end of bar 0 (quarter 2.5).
        let t = TransportState::at(&tl, true, SampleTime(60_000), 48_000.0);
        assert_eq!((t.music.beats, t.music.bar), (2.5, 0));
        let later = t.advanced(48_000);
        assert_eq!(later.position, SampleTime(108_000));
        assert_eq!(later.music.beats, 4.5);
        assert_eq!((later.music.bar, later.music.bar_start), (1, 3.0));
        let exact = TransportState::at(&tl, true, SampleTime(108_000), 48_000.0);
        assert_eq!(later.music, exact.music);
        // Several bars at once.
        let far = t.advanced(48_000 * 5);
        assert_eq!((far.music.bar, far.music.bar_start), (4, 12.0));

        let stopped = TransportState {
            playing: false,
            ..t
        };
        assert_eq!(stopped.advanced(512).position, t.position);
        // Unknown tempo: the position moves, the beats can't.
        let unknown = TransportState::default();
        let u = TransportState {
            playing: true,
            ..unknown
        }
        .advanced(64);
        assert_eq!((u.position, u.music.beats), (SampleTime(64), 0.0));
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
