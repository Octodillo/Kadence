//! Sample time, musical time, and the conversion between them.

use serde::{Deserialize, Serialize};

/// Ticks per quarter note used everywhere in the project model.
pub const PPQ: i64 = 960;

/// Samples from project start.
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct SampleTime(pub i64);

/// [`PPQ`] ticks from project start.
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct TickTime(pub i64);

/// Converts between musical and sample time.
///
/// Implementations must be allocation free: the engine calls these on the
/// audio thread. A map is immutable once built; edits produce a new map that
/// is swapped in. [`SegmentTempoMap`] is the implementation with tempo changes.
pub trait TempoMap: Send + Sync {
    fn ticks_to_samples(&self, t: TickTime, sample_rate: f64) -> SampleTime;
    fn samples_to_ticks(&self, s: SampleTime, sample_rate: f64) -> TickTime;
    fn bpm_at(&self, t: TickTime) -> f64;
}

/// One tempo change in a [`SegmentTempoMap`].
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct TempoSegment {
    pub start: TickTime,
    pub bpm: f64,
    /// When set, the tempo moves linearly (in ticks) to the next segment's bpm.
    /// Ignored on the last segment.
    pub ramp: bool,
    // Wall-clock time of `start`, cached so lookups never walk the whole map.
    start_seconds: f64,
}

impl TempoSegment {
    fn seconds_per_tick(bpm: f64) -> f64 {
        60.0 / (bpm * PPQ as f64)
    }
}

/// A tempo map made of tempo changes, each either constant or ramping to the next.
///
/// Edits (`set_tempo`, `remove`) allocate and belong off the real-time thread.
/// Conversions and `bpm_at` are a binary search plus a little arithmetic, so they
/// are RT-safe. Times before tick 0 use the first segment's tempo.
#[derive(Clone, Debug, PartialEq)]
pub struct SegmentTempoMap {
    // Sorted by start; the first segment always starts at tick 0.
    segments: Vec<TempoSegment>,
}

impl SegmentTempoMap {
    /// A map with a single constant tempo.
    pub fn new(bpm: f64) -> Self {
        assert_valid_bpm(bpm);
        Self {
            segments: vec![TempoSegment {
                start: TickTime(0),
                bpm,
                ramp: false,
                start_seconds: 0.0,
            }],
        }
    }

    pub fn segments(&self) -> &[TempoSegment] {
        &self.segments
    }

    /// Adds a tempo change at `at`, replacing any change already there.
    /// Changes before tick 0 are moved to tick 0.
    pub fn set_tempo(&mut self, at: TickTime, bpm: f64, ramp: bool) {
        assert_valid_bpm(bpm);
        let at = TickTime(at.0.max(0));
        let seg = TempoSegment {
            start: at,
            bpm,
            ramp,
            start_seconds: 0.0,
        };
        match self.segments.binary_search_by_key(&at, |s| s.start) {
            Ok(i) => self.segments[i] = seg,
            Err(i) => self.segments.insert(i, seg),
        }
        self.recompute();
    }

    /// Removes the tempo change at `at`. The change at tick 0 can't be removed.
    pub fn remove(&mut self, at: TickTime) -> bool {
        if at.0 == 0 {
            return false;
        }
        match self.segments.binary_search_by_key(&at, |s| s.start) {
            Ok(i) => {
                self.segments.remove(i);
                self.recompute();
                true
            }
            Err(_) => false,
        }
    }

    fn recompute(&mut self) {
        let mut seconds = 0.0;
        for i in 0..self.segments.len() {
            self.segments[i].start_seconds = seconds;
            if let Some(next) = self.segments.get(i + 1) {
                seconds += self.span_seconds(i, (next.start.0 - self.segments[i].start.0) as f64);
            }
        }
    }

    /// The bpm at the start and the change in bpm per tick for segment `i`.
    fn slope(&self, i: usize) -> (f64, f64) {
        let seg = &self.segments[i];
        match self.segments.get(i + 1) {
            Some(next) if seg.ramp && next.bpm != seg.bpm => (
                seg.bpm,
                (next.bpm - seg.bpm) / (next.start.0 - seg.start.0) as f64,
            ),
            _ => (seg.bpm, 0.0),
        }
    }

    /// Seconds covered by the first `ticks` ticks of segment `i`.
    fn span_seconds(&self, i: usize, ticks: f64) -> f64 {
        let (b0, k) = self.slope(i);
        if k == 0.0 || ticks < 0.0 {
            ticks * TempoSegment::seconds_per_tick(b0)
        } else {
            // Integral of 60 / (PPQ * (b0 + k t)) dt. `ln_1p` keeps its
            // precision when the ramp is nearly flat and `k` tiny.
            60.0 / (PPQ as f64 * k) * (k * ticks / b0).ln_1p()
        }
    }

    /// Ticks covered by the first `seconds` seconds of segment `i`.
    fn span_ticks(&self, i: usize, seconds: f64) -> f64 {
        let (b0, k) = self.slope(i);
        if k == 0.0 || seconds < 0.0 {
            seconds / TempoSegment::seconds_per_tick(b0)
        } else {
            b0 / k * (seconds * PPQ as f64 * k / 60.0).exp_m1()
        }
    }

    fn segment_at_tick(&self, t: TickTime) -> usize {
        self.segments
            .partition_point(|s| s.start <= t)
            .saturating_sub(1)
    }

    fn segment_at_seconds(&self, secs: f64) -> usize {
        self.segments
            .partition_point(|s| s.start_seconds <= secs)
            .saturating_sub(1)
    }

    /// Wall-clock seconds at tick `t`.
    pub fn ticks_to_seconds(&self, t: TickTime) -> f64 {
        let i = self.segment_at_tick(t);
        let seg = &self.segments[i];
        seg.start_seconds + self.span_seconds(i, (t.0 - seg.start.0) as f64)
    }

    /// Fractional ticks at wall-clock `seconds`.
    pub fn seconds_to_ticks(&self, seconds: f64) -> f64 {
        let i = self.segment_at_seconds(seconds);
        let seg = &self.segments[i];
        seg.start.0 as f64 + self.span_ticks(i, seconds - seg.start_seconds)
    }
}

impl Default for SegmentTempoMap {
    fn default() -> Self {
        Self::new(120.0)
    }
}

impl TempoMap for SegmentTempoMap {
    fn ticks_to_samples(&self, t: TickTime, sample_rate: f64) -> SampleTime {
        SampleTime((self.ticks_to_seconds(t) * sample_rate).round() as i64)
    }

    fn samples_to_ticks(&self, s: SampleTime, sample_rate: f64) -> TickTime {
        TickTime(self.seconds_to_ticks(s.0 as f64 / sample_rate).round() as i64)
    }

    fn bpm_at(&self, t: TickTime) -> f64 {
        let i = self.segment_at_tick(t);
        let (b0, k) = self.slope(i);
        b0 + k * (t.0 - self.segments[i].start.0).max(0) as f64
    }
}

fn assert_valid_bpm(bpm: f64) {
    assert!(bpm.is_finite() && bpm > 0.0, "invalid bpm {bpm}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const SR: f64 = 48_000.0;

    #[test]
    fn constant_tempo() {
        let map = SegmentTempoMap::new(120.0);
        // One beat at 120 bpm is half a second.
        assert_eq!(map.ticks_to_samples(TickTime(PPQ), SR), SampleTime(24_000));
        assert_eq!(map.samples_to_ticks(SampleTime(24_000), SR), TickTime(PPQ));
        assert_eq!(
            map.ticks_to_samples(TickTime(-PPQ), SR),
            SampleTime(-24_000)
        );
        assert_eq!(map.bpm_at(TickTime(12345)), 120.0);
    }

    #[test]
    fn tempo_change() {
        let mut map = SegmentTempoMap::new(120.0);
        map.set_tempo(TickTime(4 * PPQ), 60.0, false);
        // Four beats at 120 (2 s), then each beat takes 1 s.
        assert_eq!(
            map.ticks_to_samples(TickTime(4 * PPQ), SR),
            SampleTime(96_000)
        );
        assert_eq!(
            map.ticks_to_samples(TickTime(5 * PPQ), SR),
            SampleTime(144_000)
        );
        assert_eq!(
            map.samples_to_ticks(SampleTime(144_000), SR),
            TickTime(5 * PPQ)
        );
        assert_eq!(map.bpm_at(TickTime(4 * PPQ - 1)), 120.0);
        assert_eq!(map.bpm_at(TickTime(4 * PPQ)), 60.0);

        assert!(map.remove(TickTime(4 * PPQ)));
        assert!(!map.remove(TickTime(0)));
        assert_eq!(map, SegmentTempoMap::new(120.0));
    }

    #[test]
    fn set_tempo_replaces_and_clamps() {
        let mut map = SegmentTempoMap::new(120.0);
        map.set_tempo(TickTime(-5), 90.0, false);
        assert_eq!(map.segments().len(), 1);
        assert_eq!(map.bpm_at(TickTime(0)), 90.0);
    }

    #[test]
    fn ramp() {
        let mut map = SegmentTempoMap::new(60.0);
        map.set_tempo(TickTime(0), 60.0, true);
        map.set_tempo(TickTime(4 * PPQ), 120.0, false);
        assert_eq!(map.bpm_at(TickTime(2 * PPQ)), 90.0);
        // 4 beats ramping 60→120: 4 * ln(2) seconds.
        let secs = map.ticks_to_seconds(TickTime(4 * PPQ));
        assert!((secs - 4.0 * 2f64.ln()).abs() < 1e-9, "{secs}");
        assert!((map.seconds_to_ticks(secs) - (4 * PPQ) as f64).abs() < 1e-6);
        // After the ramp the tempo is constant at 120.
        let after = map.ticks_to_seconds(TickTime(5 * PPQ));
        assert!((after - secs - 0.5).abs() < 1e-9);
    }

    #[test]
    fn ramp_between_nearly_equal_tempos() {
        for eps in [1e-9, 1e-11, 1e-13] {
            let mut map = SegmentTempoMap::new(120.0);
            map.set_tempo(TickTime(0), 120.0, true);
            map.set_tempo(TickTime(64 * PPQ), 120.0 + eps, false);
            // 64 beats at (almost exactly) 120 bpm take 32 s.
            assert_eq!(
                map.ticks_to_samples(TickTime(64 * PPQ), SR),
                SampleTime(1_536_000),
                "{eps}"
            );
            assert_eq!(
                map.samples_to_ticks(SampleTime(768_000), SR),
                TickTime(32 * PPQ),
                "{eps}"
            );
        }
    }

    #[test]
    #[should_panic]
    fn rejects_zero_bpm() {
        SegmentTempoMap::new(0.0);
    }

    fn tempo_map() -> impl Strategy<Value = SegmentTempoMap> {
        (
            20.0..400.0f64,
            any::<bool>(),
            proptest::collection::vec((1..100_000i64, 20.0..400.0f64, any::<bool>()), 0..16),
        )
            .prop_map(|(bpm, ramp, changes)| {
                let mut map = SegmentTempoMap::new(bpm);
                map.set_tempo(TickTime(0), bpm, ramp);
                for (tick, bpm, ramp) in changes {
                    map.set_tempo(TickTime(tick), bpm, ramp);
                }
                map
            })
    }

    proptest! {
        #[test]
        fn ticks_round_trip_within_one_tick(
            map in tempo_map(),
            tick in -10_000..1_000_000i64,
            sr in prop::sample::select(vec![22_050.0, 44_100.0, 48_000.0, 96_000.0, 192_000.0]),
        ) {
            let s = map.ticks_to_samples(TickTime(tick), sr);
            let back = map.samples_to_ticks(s, sr);
            prop_assert!((back.0 - tick).abs() <= 1, "{tick} -> {s:?} -> {back:?}");
        }

        #[test]
        fn conversion_is_monotonic(map in tempo_map(), a in 0..1_000_000i64, b in 0..1_000_000i64) {
            let (lo, hi) = (a.min(b), a.max(b));
            prop_assert!(map.ticks_to_samples(TickTime(lo), SR) <= map.ticks_to_samples(TickTime(hi), SR));
        }
    }
}
