use std::sync::Arc;

use daw_core::node::{CountIn, TransportLoop, TransportState};
use daw_core::time::{SampleTime, Timeline};

/// A loop from `start` (inclusive) to `end` (exclusive).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct LoopRange {
    pub start: SampleTime,
    pub end: SampleTime,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum TransportCommand {
    Play,
    /// Plays after counting in this many bars of the time signature at the
    /// play position. The position holds still meanwhile; see
    /// [`TransportState::count_in`]. Zero bars plays straight away.
    PlayWithCountIn(u32),
    Stop,
    Locate(SampleTime),
    /// `None`, or a range with `end <= start`, turns looping off.
    SetLoop(Option<LoopRange>),
}

/// Play position, loop state and the timeline that turns the position into
/// bars and beats, owned by the RT side. The processor asks it how long the
/// next block may be (so a loop end always falls on a block boundary) and
/// then advances it.
#[derive(Clone, Debug)]
pub struct Transport {
    playing: bool,
    position: i64,
    loop_range: Option<LoopRange>,
    /// The count-in under way, if any. Never empty while set.
    count_in: Option<CountIn>,
    timeline: Arc<Timeline>,
    sample_rate: f64,
}

impl Transport {
    /// A transport on the default timeline (120 bpm, 4/4).
    pub fn new(sample_rate: f64) -> Self {
        Self::with_timeline(sample_rate, Arc::new(Timeline::default()))
    }

    pub fn with_timeline(sample_rate: f64, timeline: Arc<Timeline>) -> Self {
        Self {
            playing: false,
            position: 0,
            loop_range: None,
            count_in: None,
            timeline,
            sample_rate,
        }
    }

    /// Swaps in a new timeline and gives the old one back. On the audio
    /// thread, make sure dropping it doesn't free it: the engine keeps
    /// another reference on the controller side.
    pub fn set_timeline(&mut self, timeline: Arc<Timeline>) -> Arc<Timeline> {
        std::mem::replace(&mut self.timeline, timeline)
    }

    pub fn timeline(&self) -> &Timeline {
        &self.timeline
    }

    pub fn apply(&mut self, cmd: TransportCommand) {
        match cmd {
            TransportCommand::Play => self.playing = true,
            TransportCommand::PlayWithCountIn(bars) => {
                if !self.playing {
                    let here = TransportState::at(
                        &self.timeline,
                        false,
                        SampleTime(self.position),
                        self.sample_rate,
                    );
                    self.count_in = CountIn::bars(bars, &here.music, here.bpm, self.sample_rate)
                        .filter(|c| !c.is_empty());
                }
                self.playing = true;
            }
            TransportCommand::Stop => {
                self.playing = false;
                self.count_in = None;
            }
            TransportCommand::Locate(t) => {
                // A count-in counts towards where playback was to start.
                self.position = t.0;
                self.count_in = None;
            }
            TransportCommand::SetLoop(r) => self.loop_range = r.filter(|r| r.end > r.start),
        }
    }

    /// The state at the current position, with bars and beats. RT-safe.
    pub fn state(&self) -> TransportState {
        let position = SampleTime(self.position);
        let mut state = TransportState::at(
            &self.timeline,
            self.playing && self.count_in.is_none(),
            position,
            self.sample_rate,
        );
        state.count_in = self.count_in;
        // Past the end the loop no longer wraps (see `advance`), so plugins
        // must not expect it to.
        state.looping = self
            .loop_range
            .filter(|r| self.position < r.end.0)
            .map(|r| TransportLoop::at(&self.timeline, r.start, r.end, self.sample_rate));
        state
    }

    /// Whether the transport was told to play; also true while it counts
    /// in.
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Whether a count-in is under way.
    pub fn is_counting_in(&self) -> bool {
        self.count_in.is_some()
    }

    pub fn position(&self) -> SampleTime {
        SampleTime(self.position)
    }

    pub fn loop_range(&self) -> Option<LoopRange> {
        self.loop_range
    }

    /// How many of the next `frames` samples run before the position jumps.
    pub fn block_len(&self, frames: usize) -> usize {
        if let Some(c) = &self.count_in {
            // Ends on a block boundary, so playback starts on the right sample.
            return frames.min(usize::try_from(c.remaining()).unwrap_or(usize::MAX));
        }
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
        if let Some(c) = &mut self.count_in {
            c.elapsed += frames as i64;
            if c.is_empty() {
                self.count_in = None;
            }
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
    fn count_in_holds_the_position_then_plays() {
        // One bar of 3/4 at 120 bpm: three beats of 24 000 samples.
        let tl = Arc::new(Timeline::constant(120.0, 3, 4));
        let mut t = Transport::with_timeline(48_000.0, tl);
        t.apply(TransportCommand::Locate(SampleTime(72_000)));
        t.apply(TransportCommand::PlayWithCountIn(1));
        assert!(t.is_playing() && t.is_counting_in());
        let s = t.state();
        assert!(!s.playing);
        let c = s.count_in.unwrap();
        assert_eq!((c.beats, c.beats_per_bar, c.len()), (3, 3, 72_000));
        // Blocks end where the count-in does.
        let mut counted = 0;
        while t.is_counting_in() {
            let len = t.block_len(4096);
            counted += len;
            t.advance(len);
            assert_eq!(t.position(), SampleTime(72_000));
        }
        assert_eq!(counted, 72_000);
        assert!(t.state().playing && t.state().count_in.is_none());
        t.advance(64);
        assert_eq!(t.position(), SampleTime(72_064));
    }

    #[test]
    fn a_count_in_between_beats_clicks_on_the_song_beats() {
        // Half a beat into bar 1 of 4/4 at 120 bpm (beats are 24 000).
        let mut t = Transport::new(48_000.0);
        t.apply(TransportCommand::Locate(SampleTime(12_000)));
        t.apply(TransportCommand::PlayWithCountIn(1));
        let c = t.state().count_in.unwrap();
        // Clicks on the four beat lines before: the last is beat 1 of the
        // song, half a beat before playback starts.
        assert_eq!(c.len(), 3 * 24_000 + 12_000);
        assert_eq!(
            (0..4).map(|k| c.is_downbeat(k)).collect::<Vec<_>>(),
            [false, false, false, true]
        );
        // A locate drops the count-in.
        t.apply(TransportCommand::Locate(SampleTime(0)));
        assert!(!t.is_counting_in() && t.state().playing);
    }

    #[test]
    fn stop_cancels_a_count_in_and_zero_bars_plays_at_once() {
        let mut t = Transport::new(48_000.0);
        t.apply(TransportCommand::PlayWithCountIn(2));
        assert_eq!(t.state().count_in.unwrap().beats, 8);
        t.apply(TransportCommand::Stop);
        assert!(!t.is_counting_in() && !t.is_playing());
        t.apply(TransportCommand::PlayWithCountIn(0));
        assert!(t.state().playing && !t.is_counting_in());
        // Already playing: no count-in in the middle of the song.
        t.apply(TransportCommand::PlayWithCountIn(1));
        assert!(!t.is_counting_in());
    }

    #[test]
    fn invalid_loop_is_ignored() {
        let mut t = Transport::new(48_000.0);
        t.apply(TransportCommand::SetLoop(Some(LoopRange {
            start: SampleTime(10),
            end: SampleTime(10),
        })));
        assert_eq!(t.loop_range(), None);
    }

    #[test]
    fn state_reports_the_loop_in_beats() {
        let mut t = Transport::new(48_000.0);
        assert_eq!(t.state().looping, None);
        // Bars 2 and 3 at 120 bpm in 4/4.
        t.apply(TransportCommand::SetLoop(Some(LoopRange {
            start: SampleTime(96_000),
            end: SampleTime(192_000),
        })));
        let l = t.state().looping.unwrap();
        assert_eq!((l.start, l.end), (SampleTime(96_000), SampleTime(192_000)));
        assert_eq!((l.start_beats, l.end_beats), (4.0, 8.0));
        // Past the end playback runs on, so there is no loop to report.
        t.apply(TransportCommand::Locate(SampleTime(192_000)));
        assert_eq!(t.state().looping, None);
        t.apply(TransportCommand::Locate(SampleTime(191_999)));
        assert!(t.state().looping.is_some());
        t.apply(TransportCommand::SetLoop(None));
        assert_eq!(t.state().looping, None);
    }

    #[test]
    fn state_follows_the_timeline() {
        use daw_core::time::{Meter, MeterPoint, PPQ, SegmentTempoMap, TickTime};

        let mut t = Transport::new(48_000.0);
        assert_eq!(t.state().bpm, 120.0);
        assert_eq!(t.state().music.numerator, 4);

        // 120 bpm, then 160 from beat 4, in 3/4.
        let mut tempo = SegmentTempoMap::new(120.0);
        tempo.set_tempo(TickTime(4 * PPQ), 160.0, false);
        let meter = Meter::new([MeterPoint {
            tick: TickTime(0),
            numerator: 3,
            denominator: 4,
        }]);
        t.set_timeline(Arc::new(Timeline::new(tempo, meter)));
        // Beat 5 (quarter 5.0) is 2 s + 0.375 s in: bar 1, third beat.
        t.apply(TransportCommand::Locate(SampleTime(114_000)));
        let s = t.state();
        assert_eq!(s.bpm, 160.0);
        assert!((s.music.beats - 5.0).abs() < 1e-9);
        assert_eq!((s.music.bar, s.music.bar_start), (1, 3.0));
        assert_eq!((s.music.numerator, s.music.denominator), (3, 4));
    }
}
