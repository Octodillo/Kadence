//! Time signatures: bars and beats over the tick timeline.
//!
//! A [`Meter`] is a list of time-signature changes, each of which starts a
//! new bar. The snap grid, the arrange ruler and the transport all use it,
//! so they agree on where bars and beats are.

use super::{PPQ, TickTime};

/// The latest tick bar arithmetic works with. Times beyond `±MAX_TICK` are
/// clamped, which keeps all the bar arithmetic far from overflow. At 960 PPQ
/// and 300 bpm this is still over 29,000 years. The snap grid
/// treats `±MAX_TICK` as lines of their own.
pub const MAX_TICK: i64 = 1 << 52;

fn clamp_tick(t: TickTime) -> i64 {
    t.0.clamp(-MAX_TICK, MAX_TICK)
}

/// Length of one `1/denominator` note in ticks, at least 1.
pub fn beat_ticks(denominator: u8) -> i64 {
    (4 * PPQ / denominator.max(1) as i64).max(1)
}

/// A time-signature change. Each one starts a new bar at `tick`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct MeterPoint {
    pub tick: TickTime,
    pub numerator: u8,
    pub denominator: u8,
}

impl MeterPoint {
    /// Length of one bar in ticks, at least 1.
    pub fn bar_ticks(&self) -> i64 {
        self.numerator.max(1) as i64 * self.beat_ticks()
    }

    /// Length of one beat (one `1/denominator` note) in ticks.
    pub fn beat_ticks(&self) -> i64 {
        beat_ticks(self.denominator)
    }
}

/// One bar of a [`Meter`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Bar {
    /// 0-based bar number; bars before tick 0 are negative.
    pub index: i64,
    pub start: TickTime,
    /// In ticks, at least 1. Shorter than a full bar when the next meter
    /// change cuts it off.
    pub len: i64,
    pub numerator: u8,
    pub denominator: u8,
}

impl Bar {
    pub fn end(&self) -> TickTime {
        TickTime(self.start.0 + self.len)
    }

    pub fn beat_ticks(&self) -> i64 {
        beat_ticks(self.denominator)
    }
}

/// A position as bar, beat and tick, all 0-based (the UI adds one).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BarBeat {
    pub bar: i64,
    pub beat: i64,
    pub tick: i64,
}

/// Time-signature changes over the timeline.
///
/// The model stores the meter points; this is the lookup structure built
/// from them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Meter {
    // Sorted by tick, unique ticks; the first point is at tick 0.
    points: Vec<MeterPoint>,
    // Bar number at each point.
    first_bar: Vec<i64>,
}

impl Default for Meter {
    fn default() -> Self {
        Self::constant(4, 4)
    }
}

impl Meter {
    /// One time signature for the whole timeline.
    pub fn constant(numerator: u8, denominator: u8) -> Self {
        Self::new([MeterPoint {
            tick: TickTime(0),
            numerator,
            denominator,
        }])
    }

    /// Builds a meter from time-signature changes in any order. Invalid
    /// points (zero numerator, a denominator that is not a power of two up to
    /// 64, a tick outside `0..=MAX_TICK`) are skipped. The first valid point
    /// is moved to tick 0; with none, the meter is 4/4. A later point at the
    /// same tick replaces an earlier one.
    pub fn new(points: impl IntoIterator<Item = MeterPoint>) -> Self {
        let mut points: Vec<MeterPoint> = points
            .into_iter()
            .filter(|p| {
                p.numerator > 0
                    && p.denominator.is_power_of_two()
                    && p.denominator <= 64
                    && (0..=MAX_TICK).contains(&p.tick.0)
            })
            .collect();
        points.sort_by_key(|p| p.tick);
        // Keep the last of equal ticks.
        points.reverse();
        points.dedup_by_key(|p| p.tick);
        points.reverse();
        match points.first_mut() {
            Some(p) => p.tick = TickTime(0),
            None => points.push(MeterPoint {
                tick: TickTime(0),
                numerator: 4,
                denominator: 4,
            }),
        }
        let mut first_bar = Vec::with_capacity(points.len());
        let mut bar = 0;
        for (i, p) in points.iter().enumerate() {
            first_bar.push(bar);
            if let Some(next) = points.get(i + 1) {
                // Both ticks are within 0..=MAX_TICK, so this cannot overflow.
                let span = next.tick.0 - p.tick.0;
                bar += (span + p.bar_ticks() - 1) / p.bar_ticks();
            }
        }
        Self { points, first_bar }
    }

    pub fn points(&self) -> &[MeterPoint] {
        &self.points
    }

    /// The bar that contains `t` (clamped to `±MAX_TICK`).
    pub fn bar_at(&self, t: TickTime) -> Bar {
        let t = clamp_tick(t);
        let i = self
            .points
            .partition_point(|p| p.tick.0 <= t)
            .saturating_sub(1);
        let p = self.points[i];
        let full = p.bar_ticks();
        let n = (t - p.tick.0).div_euclid(full);
        let start = p.tick.0 + n * full;
        let len = match self.points.get(i + 1) {
            Some(next) => full.min(next.tick.0 - start),
            None => full,
        };
        Bar {
            index: self.first_bar[i] + n,
            start: TickTime(start),
            len,
            numerator: p.numerator,
            denominator: p.denominator,
        }
    }

    /// Where bar `index` starts. Negative bars extend the first meter
    /// backwards. Clamped to `±MAX_TICK`.
    pub fn bar_start(&self, index: i64) -> TickTime {
        let i = self
            .first_bar
            .partition_point(|&b| b <= index)
            .saturating_sub(1);
        let p = self.points[i];
        let bars = (index - self.first_bar[i]).clamp(-MAX_TICK, MAX_TICK);
        let t = p.tick.0.saturating_add(bars.saturating_mul(p.bar_ticks()));
        TickTime(t.clamp(-MAX_TICK, MAX_TICK))
    }

    /// `t` as bar, beat and tick.
    pub fn bar_beat(&self, t: TickTime) -> BarBeat {
        let bar = self.bar_at(t);
        let into = clamp_tick(t) - bar.start.0;
        let beat = bar.beat_ticks();
        BarBeat {
            bar: bar.index,
            beat: into / beat,
            tick: into % beat,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const Q: i64 = PPQ;

    fn t(x: i64) -> TickTime {
        TickTime(x)
    }

    #[test]
    fn four_four_bars_and_beats() {
        let m = Meter::default();
        let b = m.bar_at(t(4 * Q * 3 + 10));
        assert_eq!(b.index, 3);
        assert_eq!(b.start, t(12 * Q));
        assert_eq!(b.len, 4 * Q);
        assert_eq!(
            m.bar_beat(t(4 * Q + 2 * Q + 5)),
            BarBeat {
                bar: 1,
                beat: 2,
                tick: 5
            }
        );
        assert_eq!(m.bar_at(t(-1)).index, -1);
        assert_eq!(m.bar_start(-1), t(-4 * Q));
    }

    #[test]
    fn meter_changes_start_new_bars() {
        // Two bars of 4/4, then 7/8 from a point that cuts the third bar short.
        let m = Meter::new([
            MeterPoint {
                tick: t(9 * Q),
                numerator: 7,
                denominator: 8,
            },
            MeterPoint {
                tick: t(0),
                numerator: 4,
                denominator: 4,
            },
        ]);
        let short = m.bar_at(t(8 * Q));
        assert_eq!((short.index, short.len), (2, Q));
        let seven = m.bar_at(t(9 * Q));
        assert_eq!((seven.index, seven.len), (3, 7 * Q / 2));
        assert_eq!(m.bar_start(3), t(9 * Q));
        assert_eq!(m.bar_start(4), t(9 * Q + 7 * Q / 2));
        assert_eq!(m.bar_start(2), t(8 * Q));
    }

    #[test]
    fn invalid_points_are_dropped() {
        let m = Meter::new([
            MeterPoint {
                tick: t(100),
                numerator: 3,
                denominator: 4,
            },
            MeterPoint {
                tick: t(0),
                numerator: 0,
                denominator: 4,
            },
            MeterPoint {
                tick: t(50),
                numerator: 3,
                denominator: 5,
            },
        ]);
        assert_eq!(m.points().len(), 1);
        assert_eq!(m.points()[0].tick, t(0));
        assert_eq!(m.points()[0].numerator, 3);
        assert_eq!(Meter::new([]), Meter::default());
    }
}
