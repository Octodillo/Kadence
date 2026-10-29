use daw_core::node::TransportState;
use daw_core::time::SampleTime;

/// A loop from `start` (inclusive) to `end` (exclusive).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct LoopRange {
    pub start: SampleTime,
    pub end: SampleTime,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum TransportCommand {
    Play,
    Stop,
    Locate(SampleTime),
    /// `None`, or a range with `end <= start`, turns looping off.
    SetLoop(Option<LoopRange>),
    SetBpm(f64),
}

/// Play position and loop state, owned by the RT side. The processor asks it
/// how long the next block may be (so a loop end always falls on a block
/// boundary) and then advances it.
#[derive(Clone, Debug)]
pub struct Transport {
    playing: bool,
    position: i64,
    loop_range: Option<LoopRange>,
    bpm: f64,
    sample_rate: f64,
}

impl Transport {
    pub fn new(sample_rate: f64) -> Self {
        Self {
            playing: false,
            position: 0,
            loop_range: None,
            bpm: 120.0,
            sample_rate,
        }
    }

    pub fn apply(&mut self, cmd: TransportCommand) {
        match cmd {
            TransportCommand::Play => self.playing = true,
            TransportCommand::Stop => self.playing = false,
            TransportCommand::Locate(t) => self.position = t.0,
            TransportCommand::SetLoop(r) => self.loop_range = r.filter(|r| r.end > r.start),
            TransportCommand::SetBpm(bpm) if bpm > 0.0 => self.bpm = bpm,
            TransportCommand::SetBpm(_) => {}
        }
    }

    pub fn state(&self) -> TransportState {
        TransportState {
            playing: self.playing,
            position: SampleTime(self.position),
            bpm: self.bpm,
            sample_rate: self.sample_rate,
        }
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn position(&self) -> SampleTime {
        SampleTime(self.position)
    }

    pub fn loop_range(&self) -> Option<LoopRange> {
        self.loop_range
    }

    /// How many of the next `frames` samples run before the position jumps.
    pub fn block_len(&self, frames: usize) -> usize {
        match self.loop_range {
            Some(r) if self.playing && self.position < r.end.0 => {
                frames.min((r.end.0 - self.position) as usize)
            }
            _ => frames,
        }
    }

    /// Moves the play position on by `frames` (when playing), wrapping at
    /// the loop end.
    pub fn advance(&mut self, frames: usize) {
        if !self.playing {
            return;
        }
        let old = self.position;
        self.position += frames as i64;
        match self.loop_range {
            Some(r) if old < r.end.0 && self.position >= r.end.0 => {
                self.position = r.start.0 + (self.position - r.end.0);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loop_wraps_on_block_boundary() {
        let mut t = Transport::new(48_000.0);
        t.apply(TransportCommand::SetLoop(Some(LoopRange {
            start: SampleTime(100),
            end: SampleTime(200),
        })));
        t.apply(TransportCommand::Locate(SampleTime(150)));
        t.apply(TransportCommand::Play);
        assert_eq!(t.block_len(64), 50);
        t.advance(50);
        assert_eq!(t.position(), SampleTime(100));
        assert_eq!(t.block_len(64), 64);
    }

    #[test]
    fn stopped_transport_holds_position() {
        let mut t = Transport::new(48_000.0);
        t.apply(TransportCommand::Locate(SampleTime(10)));
        t.advance(64);
        assert_eq!(t.position(), SampleTime(10));
        assert!(!t.state().playing);
    }

    #[test]
    fn past_loop_end_plays_on() {
        let mut t = Transport::new(48_000.0);
        t.apply(TransportCommand::SetLoop(Some(LoopRange {
            start: SampleTime(0),
            end: SampleTime(100),
        })));
        t.apply(TransportCommand::Locate(SampleTime(500)));
        t.apply(TransportCommand::Play);
        assert_eq!(t.block_len(64), 64);
        t.advance(64);
        assert_eq!(t.position(), SampleTime(564));
    }

    #[test]
    fn invalid_loop_and_bpm_are_ignored() {
        let mut t = Transport::new(48_000.0);
        t.apply(TransportCommand::SetLoop(Some(LoopRange {
            start: SampleTime(10),
            end: SampleTime(10),
        })));
        t.apply(TransportCommand::SetBpm(-3.0));
        assert_eq!(t.loop_range(), None);
        assert_eq!(t.state().bpm, 120.0);
    }
}
