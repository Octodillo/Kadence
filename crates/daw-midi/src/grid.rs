//! The snap grid, in PPQ ticks.
//!
//! Grid lines restart at every bar, so a grid that does not divide the bar
//! evenly (dotted notes in 4/4, eighths in 7/8 and so on) still puts a line
//! on each downbeat. Bars come from a [`Meter`]: a list of time-signature
//! changes, each of which starts a new bar.

use std::ops::Range;

use daw_core::time::{PPQ, TickTime};

/// A time-signature change. Each one starts a new bar at `tick`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct MeterPoint {
    pub tick: TickTime,
    pub numerator: u8,
    pub denominator: u8,
}

impl MeterPoint {
    /// Length of one bar in ticks.
    pub fn bar_ticks(&self) -> i64 {
        self.numerator as i64 * self.beat_ticks()
    }

    /// Length of one beat (one `1/denominator` note) in ticks.
    pub fn beat_ticks(&self) -> i64 {
        (4 * PPQ / self.denominator as i64).max(1)
    }
}

/// One bar of a [`Meter`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Bar {
    /// 0-based bar number; bars before tick 0 are negative.
    pub index: i64,
    pub start: TickTime,
    /// In ticks. Shorter than a full bar when the next meter change cuts it off.
    pub len: i64,
    pub numerator: u8,
    pub denominator: u8,
}

impl Bar {
    pub fn end(&self) -> TickTime {
        TickTime(self.start.0 + self.len)
    }

    pub fn beat_ticks(&self) -> i64 {
        (4 * PPQ / self.denominator as i64).max(1)
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
    /// 64, a negative tick) are skipped. The first valid point is moved to
    /// tick 0; with none, the meter is 4/4. A later point at the same tick
    /// replaces an earlier one.
    pub fn new(points: impl IntoIterator<Item = MeterPoint>) -> Self {
        let mut points: Vec<MeterPoint> = points
            .into_iter()
            .filter(|p| {
                p.numerator > 0
                    && p.denominator.is_power_of_two()
                    && p.denominator <= 64
                    && p.tick.0 >= 0
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
                let span = next.tick.0 - p.tick.0;
                bar += (span + p.bar_ticks() - 1) / p.bar_ticks();
            }
        }
        Self { points, first_bar }
    }

    pub fn points(&self) -> &[MeterPoint] {
        &self.points
    }

    fn segment(&self, t: TickTime) -> usize {
        self.points
            .partition_point(|p| p.tick <= t)
            .saturating_sub(1)
    }

    /// The bar that contains `t`.
    pub fn bar_at(&self, t: TickTime) -> Bar {
        let i = self.segment(t);
        let p = self.points[i];
        let full = p.bar_ticks();
        let n = (t.0 - p.tick.0).div_euclid(full);
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

    /// Where bar `index` starts. Negative bars extend the first meter backwards.
    pub fn bar_start(&self, index: i64) -> TickTime {
        let i = self
            .first_bar
            .partition_point(|&b| b <= index)
            .saturating_sub(1);
        let p = self.points[i];
        TickTime(p.tick.0 + (index - self.first_bar[i]) * p.bar_ticks())
    }

    /// `t` as bar, beat and tick.
    pub fn bar_beat(&self, t: TickTime) -> BarBeat {
        let bar = self.bar_at(t);
        let into = t.0 - bar.start.0;
        let beat = bar.beat_ticks();
        BarBeat {
            bar: bar.index,
            beat: into / beat,
            tick: into % beat,
        }
    }
}

/// How long one grid step is.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Division {
    /// One line per bar.
    Bar,
    /// One line per beat of the current meter (a quarter in 4/4, an eighth in 6/8).
    Beat,
    /// One line per `1/n` note: `Note(16)` is a sixteenth grid.
    Note(u16),
}

/// Straight, triplet or dotted steps.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Feel {
    #[default]
    Straight,
    /// Three steps in the time of two.
    Triplet,
    /// One and a half times as long.
    Dotted,
}

/// What a grid line is, for drawing.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LineKind {
    Bar,
    Beat,
    Step,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct GridLine {
    pub tick: TickTime,
    pub kind: LineKind,
}

/// A snap grid: a step length, a feel and an optional swing.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Grid {
    pub division: Division,
    pub feel: Feel,
    /// 0 to 1. Moves every second line of a bar later by up to a third of a
    /// step, so 1 turns straight eighths into a triplet shuffle.
    pub swing: f64,
}

impl Default for Grid {
    fn default() -> Self {
        Self::note(16)
    }
}

impl Grid {
    pub fn new(division: Division) -> Self {
        Self {
            division,
            feel: Feel::Straight,
            swing: 0.0,
        }
    }

    /// A straight `1/n` note grid.
    pub fn note(n: u16) -> Self {
        Self::new(Division::Note(n))
    }

    pub fn triplet(mut self) -> Self {
        self.feel = Feel::Triplet;
        self
    }

    pub fn dotted(mut self) -> Self {
        self.feel = Feel::Dotted;
        self
    }

    pub fn with_swing(mut self, swing: f64) -> Self {
        self.swing = swing;
        self
    }

    /// The step length in ticks inside `bar`, at least 1.
    pub fn step_in(&self, bar: &Bar) -> i64 {
        let base = match self.division {
            Division::Bar => return bar.len.max(1),
            Division::Beat => bar.beat_ticks(),
            Division::Note(n) => 4 * PPQ / n.max(1) as i64,
        };
        let step = match self.feel {
            Feel::Straight => base,
            Feel::Triplet => base * 2 / 3,
            Feel::Dotted => base * 3 / 2,
        };
        step.max(1)
    }

    /// The step length in ticks at `t`.
    pub fn step_at(&self, t: TickTime, meter: &Meter) -> i64 {
        self.step_in(&meter.bar_at(t))
    }

    fn swing_offset(&self, step: i64) -> i64 {
        if self.swing.is_finite() && !matches!(self.division, Division::Bar) {
            (self.swing.clamp(0.0, 1.0) * step as f64 / 3.0).round() as i64
        } else {
            0
        }
    }

    /// Line `k` of `bar`, or `None` past the bar's end.
    fn line(&self, bar: &Bar, step: i64, k: i64) -> Option<i64> {
        let off = if k % 2 == 1 {
            self.swing_offset(step)
        } else {
            0
        };
        let at = k * step + off;
        (k >= 0 && at < bar.len).then_some(bar.start.0 + at)
    }

    /// The last line at or before `t`.
    pub fn floor(&self, t: TickTime, meter: &Meter) -> TickTime {
        let bar = meter.bar_at(t);
        let step = self.step_in(&bar);
        let k = (t.0 - bar.start.0) / step;
        // Swing can push line k past t, but never line k - 1.
        let line = [k, k - 1]
            .into_iter()
            .filter_map(|k| self.line(&bar, step, k))
            .find(|&l| l <= t.0)
            // Line 0 is the bar start; only a tail line past the end is missing.
            .unwrap_or_else(|| self.last_line(&bar, step));
        TickTime(line)
    }

    fn last_line(&self, bar: &Bar, step: i64) -> i64 {
        let mut k = (bar.len - 1) / step;
        loop {
            if let Some(l) = self.line(bar, step, k) {
                return l;
            }
            k -= 1;
        }
    }

    /// The first line strictly after `t`.
    pub fn next(&self, t: TickTime, meter: &Meter) -> TickTime {
        let bar = meter.bar_at(t);
        let step = self.step_in(&bar);
        let k = (t.0 - bar.start.0) / step;
        [k, k + 1, k + 2]
            .into_iter()
            .filter_map(|k| self.line(&bar, step, k))
            .find(|&l| l > t.0)
            .map_or(bar.end(), TickTime)
    }

    /// The last line strictly before `t`.
    pub fn prev(&self, t: TickTime, meter: &Meter) -> TickTime {
        self.floor(TickTime(t.0 - 1), meter)
    }

    /// The first line at or after `t`.
    pub fn ceil(&self, t: TickTime, meter: &Meter) -> TickTime {
        let f = self.floor(t, meter);
        if f == t { f } else { self.next(t, meter) }
    }

    /// The nearest line to `t`; halfway goes to the later line.
    pub fn snap(&self, t: TickTime, meter: &Meter) -> TickTime {
        let f = self.floor(t, meter);
        if f == t {
            return f;
        }
        let n = self.next(t, meter);
        if t.0 - f.0 < n.0 - t.0 { f } else { n }
    }

    /// Every grid line in `range` (start inclusive, end exclusive), in order,
    /// marked as bar, beat or plain step for drawing.
    pub fn lines<'a>(
        &'a self,
        range: Range<TickTime>,
        meter: &'a Meter,
    ) -> impl Iterator<Item = GridLine> + 'a {
        let mut t = self.ceil(range.start, meter);
        std::iter::from_fn(move || {
            if t >= range.end {
                return None;
            }
            let here = t;
            t = self.next(t, meter);
            let bar = meter.bar_at(here);
            let into = here.0 - bar.start.0;
            let kind = if into == 0 {
                LineKind::Bar
            } else if into % bar.beat_ticks() == 0 {
                LineKind::Beat
            } else {
                LineKind::Step
            };
            Some(GridLine { tick: here, kind })
        })
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

    #[test]
    fn sixteenth_snap() {
        let m = Meter::default();
        let g = Grid::note(16);
        assert_eq!(g.step_at(t(0), &m), Q / 4);
        assert_eq!(g.snap(t(100), &m), t(0));
        assert_eq!(g.snap(t(120), &m), t(240));
        assert_eq!(g.snap(t(130), &m), t(240));
        assert_eq!(g.floor(t(479), &m), t(240));
        assert_eq!(g.ceil(t(241), &m), t(480));
        assert_eq!(g.ceil(t(240), &m), t(240));
        assert_eq!(g.next(t(240), &m), t(480));
        assert_eq!(g.prev(t(240), &m), t(0));
        assert_eq!(g.floor(t(-1), &m), t(-240));
    }

    #[test]
    fn triplets_and_dotted() {
        let m = Meter::default();
        assert_eq!(Grid::note(8).triplet().step_at(t(0), &m), Q / 3);
        assert_eq!(Grid::note(8).dotted().step_at(t(0), &m), Q * 3 / 4);
        // Dotted quarters in 4/4: lines at 0, 1.5 and 3 beats, then the downbeat.
        let g = Grid::note(4).dotted();
        let lines: Vec<i64> = g
            .lines(t(0)..t(8 * Q), &m)
            .map(|l| l.tick.0 / (Q / 2))
            .collect();
        assert_eq!(lines, [0, 3, 6, 8, 11, 14]);
        assert_eq!(g.next(t(3 * Q), &m), t(4 * Q));
        assert_eq!(g.floor(t(4 * Q - 1), &m), t(3 * Q));
    }

    #[test]
    fn beat_division_follows_meter() {
        let m = Meter::constant(6, 8);
        let g = Grid::new(Division::Beat);
        assert_eq!(g.step_at(t(0), &m), Q / 2);
        assert_eq!(Grid::new(Division::Bar).step_at(t(0), &m), 3 * Q);
        assert_eq!(Grid::new(Division::Bar).snap(t(1600), &m), t(3 * Q));
    }

    #[test]
    fn swing_moves_odd_lines() {
        let m = Meter::default();
        let g = Grid::note(8).with_swing(1.0);
        let lines: Vec<i64> = g.lines(t(0)..t(2 * Q), &m).map(|l| l.tick.0).collect();
        assert_eq!(lines, [0, Q / 2 + Q / 6, Q, Q + Q / 2 + Q / 6]);
        assert_eq!(g.floor(t(Q / 2 + 10), &m), t(0));
        assert_eq!(g.snap(t(Q / 2 + 10), &m), t(Q / 2 + Q / 6));
    }

    #[test]
    fn line_kinds() {
        let m = Meter::default();
        let kinds: Vec<LineKind> = Grid::note(8)
            .lines(t(3 * Q)..t(5 * Q), &m)
            .map(|l| l.kind)
            .collect();
        assert_eq!(
            kinds,
            [
                LineKind::Beat,
                LineKind::Step,
                LineKind::Bar,
                LineKind::Step
            ]
        );
    }
}
