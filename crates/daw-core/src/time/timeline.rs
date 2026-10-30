//! The tempo map and time signatures together, and musical positions on them.

use super::{MAX_TICK, Meter, PPQ, SampleTime, SegmentTempoMap, TempoMap, TickTime};

/// Everything needed to turn a sample position into bars and beats: the
/// project's tempo map and its time signatures.
///
/// Like the maps it holds, a timeline is immutable once built; edits build a
/// new one. [`Timeline::position`] is RT-safe.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Timeline {
    pub tempo: SegmentTempoMap,
    pub meter: Meter,
}

/// Where a point in time falls in bars and beats.
///
/// Beat values here count quarter notes from tick 0, whatever the time
/// signature, because that is what the CLAP, VST3 and LV2 transports expect.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct MusicalPosition {
    /// Quarter notes from tick 0, with the fraction.
    pub beats: f64,
    /// 0-based bar number; bars before tick 0 are negative.
    pub bar: i64,
    /// Where that bar starts, in quarter notes from tick 0.
    pub bar_start: f64,
    /// Length of that bar in quarter notes. Shorter than a full bar when the
    /// next time-signature change cuts it off.
    pub bar_len: f64,
    /// Time signature of that bar, like 6 and 8 for 6/8.
    pub numerator: u8,
    pub denominator: u8,
}

impl Default for MusicalPosition {
    /// The start of a 4/4 timeline.
    fn default() -> Self {
        Self {
            beats: 0.0,
            bar: 0,
            bar_start: 0.0,
            bar_len: 4.0,
            numerator: 4,
            denominator: 4,
        }
    }
}

impl MusicalPosition {
    /// Length of one beat of the time signature (a `1/denominator` note) in
    /// quarter notes: 1 in 4/4, 0.5 in 6/8.
    pub fn beat_len(&self) -> f64 {
        4.0 / f64::from(self.denominator.max(1))
    }

    /// Beats of the time signature since the bar started, with the fraction:
    /// 2.5 is halfway through the third beat.
    pub fn bar_beat(&self) -> f64 {
        (self.beats - self.bar_start) / self.beat_len()
    }
}

impl Timeline {
    pub fn new(tempo: SegmentTempoMap, meter: Meter) -> Self {
        Self { tempo, meter }
    }

    /// One tempo and one time signature for the whole timeline.
    pub fn constant(bpm: f64, numerator: u8, denominator: u8) -> Self {
        Self {
            tempo: SegmentTempoMap::new(bpm),
            meter: Meter::constant(numerator, denominator),
        }
    }

    /// Fractional ticks at sample `s`, clamped to `±MAX_TICK`. A sample rate
    /// that is not positive and finite counts as tick 0.
    fn ticks_at(&self, s: SampleTime, sample_rate: f64) -> f64 {
        if !(sample_rate.is_finite() && sample_rate > 0.0) {
            return 0.0;
        }
        #[allow(clippy::cast_precision_loss)]
        let ticks = self.tempo.seconds_to_ticks(s.0 as f64 / sample_rate);
        #[allow(clippy::cast_precision_loss)]
        let max = MAX_TICK as f64;
        if ticks.is_nan() {
            0.0
        } else {
            ticks.clamp(-max, max)
        }
    }

    /// The tempo at sample `s`.
    pub fn bpm_at(&self, s: SampleTime, sample_rate: f64) -> f64 {
        #[allow(clippy::cast_possible_truncation)]
        let tick = TickTime(self.ticks_at(s, sample_rate).floor() as i64);
        self.tempo.bpm_at(tick)
    }

    /// Where sample `s` falls in bars and beats.
    pub fn position(&self, s: SampleTime, sample_rate: f64) -> MusicalPosition {
        let ticks = self.ticks_at(s, sample_rate);
        #[allow(clippy::cast_possible_truncation)]
        let bar = self.meter.bar_at(TickTime(ticks.floor() as i64));
        #[allow(clippy::cast_precision_loss)]
        let q = |t: i64| t as f64 / PPQ as f64;
        MusicalPosition {
            beats: ticks / PPQ as f64,
            bar: bar.index,
            bar_start: q(bar.start.0),
            bar_len: q(bar.len),
            numerator: bar.numerator,
            denominator: bar.denominator,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::MeterPoint;
    use super::*;

    const SR: f64 = 48_000.0;

    #[test]
    fn four_four_at_120() {
        let tl = Timeline::default();
        // Bar 2 (0-based 1), beat 3 and a half: 4 + 2.5 quarters = 3.25 s.
        let p = tl.position(SampleTime(156_000), SR);
        assert_eq!(p.beats, 6.5);
        assert_eq!((p.bar, p.bar_start, p.bar_len), (1, 4.0, 4.0));
        assert_eq!((p.numerator, p.denominator), (4, 4));
        assert_eq!(p.bar_beat(), 2.5);
        assert_eq!(tl.bpm_at(SampleTime(156_000), SR), 120.0);
    }

    #[test]
    fn beats_integrate_tempo_changes() {
        // The failure from issue #45: 120 bpm, then 160 from beat 4.
        let mut tempo = SegmentTempoMap::new(120.0);
        tempo.set_tempo(TickTime(4 * PPQ), 160.0, false);
        let tl = Timeline::new(tempo, Meter::default());
        // Four beats at 120 take 2 s; one more at 160 takes 0.375 s.
        let p = tl.position(SampleTime(114_000), SR);
        assert!((p.beats - 5.0).abs() < 1e-9, "{}", p.beats);
        assert_eq!(p.bar, 1);
        assert_eq!(tl.bpm_at(SampleTime(114_000), SR), 160.0);
        // Before the change nothing moved.
        let p = tl.position(SampleTime(72_000), SR);
        assert!((p.beats - 3.0).abs() < 1e-9);
    }

    #[test]
    fn time_signatures_set_bars_and_beat_length() {
        // Two bars of 4/4, then 6/8.
        let meter = Meter::new([
            MeterPoint {
                tick: TickTime(0),
                numerator: 4,
                denominator: 4,
            },
            MeterPoint {
                tick: TickTime(8 * PPQ),
                numerator: 6,
                denominator: 8,
            },
        ]);
        let tl = Timeline::new(SegmentTempoMap::new(120.0), meter);
        // Quarter 12.5 is a quarter and a half into the second 6/8 bar.
        let p = tl.position(SampleTime(300_000), SR);
        assert_eq!(p.beats, 12.5);
        assert_eq!((p.bar, p.bar_start, p.bar_len), (3, 11.0, 3.0));
        assert_eq!((p.numerator, p.denominator), (6, 8));
        assert_eq!(p.beat_len(), 0.5);
        assert_eq!(p.bar_beat(), 3.0);
    }

    #[test]
    fn before_the_start_and_bad_rates() {
        let tl = Timeline::constant(120.0, 3, 4);
        let p = tl.position(SampleTime(-24_000), SR);
        assert_eq!((p.beats, p.bar, p.bar_start), (-1.0, -1, -3.0));
        for sr in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let p = tl.position(SampleTime(1000), sr);
            assert_eq!((p.beats, p.bar), (0.0, 0));
        }
        let far = tl.position(SampleTime(i64::MAX), 1.0);
        assert!(far.beats.is_finite() && far.bar > 0);
    }
}
