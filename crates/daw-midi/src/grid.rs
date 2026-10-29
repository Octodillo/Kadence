//! The snap grid, in PPQ ticks.
//!
//! Grid lines restart at every bar, so a grid that does not divide the bar
//! evenly (dotted notes in 4/4, eighths in 7/8 and so on) still puts a line
//! on each downbeat. Bars come from a [`Meter`]: a list of time-signature
//! changes, each of which starts a new bar.

use std::ops::Range;

use daw_core::time::{PPQ, TickTime};

/// The latest tick the grid works with. Times beyond `±MAX_TICK` are
/// clamped, which keeps all the bar arithmetic far from overflow. At 960 PPQ
/// and 300 bpm this is still over 29,000 years.
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

/// The step inside one bar, as the exact fraction `num / den` ticks so that
/// steps that are not whole ticks (sevenths, twelfth-note triplets) do not
/// drift across the bar. Line `k` sits at `floor(k * num / den)`.
#[derive(Copy, Clone, Debug)]
struct Steps {
    bar: Bar,
    num: i64,
    den: i64,
    swing: i64,
}

impl Steps {
    /// Line `k`, or `None` outside the bar.
    fn line(&self, k: i64) -> Option<i64> {
        let off = if k % 2 == 1 { self.swing } else { 0 };
        let at = k * self.num / self.den + off;
        (k >= 0 && at < self.bar.len).then_some(self.bar.start.0 + at)
    }

    /// The index of the step that `t` falls in, ignoring swing.
    fn index(&self, t: i64) -> i64 {
        (t - self.bar.start.0) * self.den / self.num
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

    fn steps(&self, bar: Bar) -> Steps {
        let (mut num, mut den) = match self.division {
            Division::Bar => (bar.len, 1),
            Division::Beat => (bar.beat_ticks(), 1),
            Division::Note(n) => (4 * PPQ, n.max(1) as i64),
        };
        if !matches!(self.division, Division::Bar) {
            match self.feel {
                Feel::Straight => {}
                Feel::Triplet => (num, den) = (num * 2, den * 3),
                Feel::Dotted => (num, den) = (num * 3, den * 2),
            }
        }
        // Never finer than one tick.
        if num < den {
            (num, den) = (1, 1);
        }
        let swing = if self.swing.is_finite() && !matches!(self.division, Division::Bar) {
            (self.swing.clamp(0.0, 1.0) * num as f64 / den as f64 / 3.0).round() as i64
        } else {
            0
        };
        Steps {
            bar,
            num,
            den,
            swing,
        }
    }

    /// The step length in ticks inside `bar` (rounded), at least 1.
    pub fn step_in(&self, bar: &Bar) -> i64 {
        let s = self.steps(*bar);
        ((s.num + s.den / 2) / s.den).max(1)
    }

    /// The step length in ticks at `t`.
    pub fn step_at(&self, t: TickTime, meter: &Meter) -> i64 {
        self.step_in(&meter.bar_at(t))
    }

    /// The last line at or before `t` (clamped to `±MAX_TICK`).
    pub fn floor(&self, t: TickTime, meter: &Meter) -> TickTime {
        let t = clamp_tick(t);
        let s = self.steps(meter.bar_at(TickTime(t)));
        let k = s.index(t);
        // Line k + 1 can land on t through rounding; swing can push line k
        // past t, but then line k - 1 is unswung and before t. Line 0 is
        // the bar start, so one of these always exists.
        let line = [k + 1, k, k - 1]
            .into_iter()
            .filter_map(|k| s.line(k))
            .find(|&l| l <= t)
            .unwrap_or(s.bar.start.0);
        TickTime(line)
    }

    /// The first line strictly after `t` (clamped to `±MAX_TICK`).
    pub fn next(&self, t: TickTime, meter: &Meter) -> TickTime {
        let t = clamp_tick(t);
        let s = self.steps(meter.bar_at(TickTime(t)));
        let k = s.index(t);
        [k, k + 1, k + 2]
            .into_iter()
            .filter_map(|k| s.line(k))
            .find(|&l| l > t)
            .map_or(s.bar.end(), TickTime)
    }

    /// The last line strictly before `t`.
    pub fn prev(&self, t: TickTime, meter: &Meter) -> TickTime {
        self.floor(TickTime(clamp_tick(t) - 1), meter)
    }

    /// The first line at or after `t`.
    pub fn ceil(&self, t: TickTime, meter: &Meter) -> TickTime {
        let f = self.floor(t, meter);
        if f.0 == clamp_tick(t) {
            f
        } else {
            self.next(t, meter)
        }
    }

    /// The nearest line to `t`; halfway goes to the later line.
    pub fn snap(&self, t: TickTime, meter: &Meter) -> TickTime {
        let f = self.floor(t, meter);
        let c = clamp_tick(t);
        if f.0 == c {
            return f;
        }
        let n = self.next(t, meter);
        if c - f.0 < n.0 - c { f } else { n }
    }

    /// Every grid line in `range` (start inclusive, end exclusive), in order,
    /// marked as bar, beat or plain step for drawing.
    pub fn lines<'a>(
        &'a self,
        range: Range<TickTime>,
        meter: &'a Meter,
    ) -> impl Iterator<Item = GridLine> + 'a {
        let end = clamp_tick(range.end);
        let first = self.ceil(range.start, meter).0;
        let mut s = self.steps(meter.bar_at(TickTime(first)));
        let mut k = s.index(first);
        // Step back to the first line at or after `first`.
        while k > 0 && s.line(k - 1).is_some_and(|l| l >= first) {
            k -= 1;
        }
        while s.line(k).is_some_and(|l| l < first) {
            k += 1;
        }
        std::iter::from_fn(move || {
            let tick = match s.line(k) {
                Some(l) => l,
                None => {
                    if s.bar.end().0 > MAX_TICK {
                        return None;
                    }
                    s = self.steps(meter.bar_at(s.bar.end()));
                    k = 0;
                    s.bar.start.0
                }
            };
            if tick >= end {
                return None;
            }
            k += 1;
            let into = tick - s.bar.start.0;
            let kind = if into == 0 {
                LineKind::Bar
            } else if into % s.bar.beat_ticks() == 0 {
                LineKind::Beat
            } else {
                LineKind::Step
            };
            Some(GridLine {
                tick: TickTime(tick),
                kind,
            })
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

    #[test]
    fn uneven_steps_do_not_drift() {
        let m = Meter::default();
        // Sevenths of a whole note: 548.57 ticks each, seven per bar.
        let g = Grid::note(7);
        let lines: Vec<i64> = g.lines(t(0)..t(8 * Q), &m).map(|l| l.tick.0).collect();
        assert_eq!(lines.len(), 14);
        assert_eq!(lines[6], 3291);
        assert_eq!(lines[7], 4 * Q);
        assert_eq!(g.floor(t(4 * Q - 1), &m), t(3291));
        // Twelfth-note triplets: 213.33 ticks, eighteen per bar.
        let g = Grid::note(12).triplet();
        let n = g.lines(t(0)..t(4 * Q), &m).count();
        assert_eq!(n, 18);
        assert_eq!(g.prev(t(4 * Q), &m), t(3626));
    }

    #[test]
    fn extreme_values_do_not_panic() {
        let m = Meter::new([
            MeterPoint {
                tick: t(0),
                numerator: 4,
                denominator: 4,
            },
            MeterPoint {
                tick: t(i64::MAX),
                numerator: 3,
                denominator: 4,
            },
            MeterPoint {
                tick: t(MAX_TICK),
                numerator: 3,
                denominator: 4,
            },
        ]);
        assert_eq!(m.points().len(), 2);
        let g = Grid::note(16);
        for x in [i64::MIN, -MAX_TICK, MAX_TICK, i64::MAX] {
            g.floor(t(x), &m);
            g.next(t(x), &m);
            g.prev(t(x), &m);
            g.snap(t(x), &m);
            m.bar_beat(t(x));
            assert!(g.lines(t(x)..t(i64::MAX), &m).take(3).count() <= 3);
        }
        m.bar_start(i64::MIN);
        m.bar_start(i64::MAX);
        let bad = MeterPoint {
            tick: t(0),
            numerator: 0,
            denominator: 0,
        };
        assert_eq!(bad.beat_ticks(), 4 * Q);
        assert_eq!(bad.bar_ticks(), 4 * Q);
        assert_eq!(Grid::note(0).step_at(t(0), &m), 4 * Q);
        assert_eq!(Grid::note(u16::MAX).step_at(t(0), &m), 1);
    }
}
