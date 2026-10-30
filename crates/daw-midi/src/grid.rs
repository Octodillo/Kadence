//! The snap grid, in PPQ ticks.
//!
//! Grid lines restart at every bar, so a grid that does not divide the bar
//! evenly (dotted notes in 4/4, eighths in 7/8 and so on) still puts a line
//! on each downbeat. Bars come from a [`Meter`] (from `daw_core::time`): a
//! list of time-signature changes, each of which starts a new bar.

use std::ops::Range;

use daw_core::time::{PPQ, TickTime};

pub use daw_core::time::{Bar, BarBeat, MAX_TICK, Meter, MeterPoint, beat_ticks};

fn clamp_tick(t: TickTime) -> i64 {
    t.0.clamp(-MAX_TICK, MAX_TICK)
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
    /// step, so 1 turns straight eighths into a triplet shuffle. A swung line
    /// sits on the tick at or before its exact position, like every other
    /// line, and always before the next line.
    pub swing: f64,
}

impl Default for Grid {
    fn default() -> Self {
        Self::note(16)
    }
}

/// Swing is kept as a whole number of `1 / SWING_DEN` steps, so that swung
/// lines are exact fractions too. A full swing is a third of a step.
const SWING_DEN: i64 = 3 * 1024;

/// The step inside one bar, as the exact fraction `num / den` ticks so that
/// steps that are not whole ticks (sevenths, twelfth-note triplets) do not
/// drift across the bar. Line `k` sits at `floor(k * num / den)`; odd lines
/// are moved later by `swing / SWING_DEN` steps.
///
/// This is the only definition of where lines are: `floor`, `next` and
/// `lines` all go through [`Steps::line`].
#[derive(Copy, Clone, Debug)]
struct Steps {
    bar: Bar,
    num: i64,
    den: i64,
    swing: i64,
}

impl Steps {
    /// Line `k`, or `None` outside the bar. Lines are strictly increasing
    /// in `k`: a step is at least one tick, and a swung line is kept before
    /// the next unswung line and inside the bar.
    fn line(&self, k: i64) -> Option<i64> {
        if k < 0 {
            return None;
        }
        // At most bar length times `den`, so far from overflow even times
        // SWING_DEN.
        let base = k * self.num / self.den;
        if base >= self.bar.len {
            return None;
        }
        let at = if k % 2 == 1 && self.swing > 0 {
            let swung = (k * self.num * SWING_DEN + self.swing * self.num) / (self.den * SWING_DEN);
            let next = (k + 1) * self.num / self.den;
            swung.min(next - 1).min(self.bar.len - 1)
        } else {
            base
        };
        Some(self.bar.start.0 + at)
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
            (self.swing.clamp(0.0, 1.0) * (SWING_DEN / 3) as f64).round() as i64
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
        if t == MAX_TICK {
            return TickTime(MAX_TICK);
        }
        TickTime(self.grid_floor(t, meter).max(-MAX_TICK))
    }

    /// The first line strictly after `t` (clamped to `±MAX_TICK`). At
    /// `MAX_TICK` there is none, so this returns `MAX_TICK`.
    pub fn next(&self, t: TickTime, meter: &Meter) -> TickTime {
        let t = clamp_tick(t);
        if t == MAX_TICK {
            return TickTime(MAX_TICK);
        }
        TickTime(self.grid_next(t, meter).min(MAX_TICK))
    }

    /// The last grid line at or before `t`, ignoring the `±MAX_TICK` edges.
    /// `t` must already be clamped.
    fn grid_floor(&self, t: i64, meter: &Meter) -> i64 {
        let s = self.steps(meter.bar_at(TickTime(t)));
        let k = s.index(t);
        // Line k + 1 can land on t through rounding; swing can push line k
        // past t, but then line k - 1 is unswung and before t. Line 0 is
        // the bar start, so one of these always exists.
        [k + 1, k, k - 1]
            .into_iter()
            .filter_map(|k| s.line(k))
            .find(|&l| l <= t)
            .unwrap_or(s.bar.start.0)
    }

    /// The first grid line strictly after `t`, ignoring the `±MAX_TICK`
    /// edges; it can lie up to one bar past `MAX_TICK`. `t` must already be
    /// clamped.
    fn grid_next(&self, t: i64, meter: &Meter) -> i64 {
        let s = self.steps(meter.bar_at(TickTime(t)));
        let k = s.index(t);
        [k, k + 1, k + 2]
            .into_iter()
            .filter_map(|k| s.line(k))
            .find(|&l| l > t)
            .unwrap_or(s.bar.end().0)
    }

    /// The last line strictly before `t`. At `-MAX_TICK` there is none, so
    /// this returns `-MAX_TICK`.
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
        // The `±MAX_TICK` edges are lines even when the grid has none there.
        let edge = |tick: i64| GridLine {
            tick: TickTime(tick),
            kind: line_kind(tick, &meter.bar_at(TickTime(tick))),
        };
        let low = (first == -MAX_TICK && first < end && self.grid_floor(first, meter) != first)
            .then(|| edge(-MAX_TICK));
        // Grid lines stop before `end`, so a range past `MAX_TICK` still
        // needs the edge itself.
        let high = (range.end.0 > MAX_TICK).then(|| edge(MAX_TICK));
        let mut s = self.steps(meter.bar_at(TickTime(first)));
        let mut k = s.index(first);
        // Step back to the first line at or after `first`.
        while k > 0 && s.line(k - 1).is_some_and(|l| l >= first) {
            k -= 1;
        }
        while s.line(k).is_some_and(|l| l < first) {
            k += 1;
        }
        let grid = std::iter::from_fn(move || {
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
            Some(GridLine {
                tick: TickTime(tick),
                kind: line_kind(tick, &s.bar),
            })
        });
        low.into_iter().chain(grid).chain(high)
    }
}

/// Whether a line at `tick` inside `bar` is a bar, beat or plain step line.
fn line_kind(tick: i64, bar: &Bar) -> LineKind {
    let into = tick - bar.start.0;
    if into == 0 {
        LineKind::Bar
    } else if into % bar.beat_ticks() == 0 {
        LineKind::Beat
    } else {
        LineKind::Step
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
    fn heavy_swing_on_fine_grids_keeps_lines_distinct() {
        let m = Meter::default();
        // Steps of 1.998 ticks: a rounded swing offset used to push odd lines
        // onto the following even line.
        let g = Grid::note(1922).with_swing(1.0);
        let lines: Vec<i64> = g.lines(t(-970)..t(-950), &m).map(|l| l.tick.0).collect();
        assert!(lines.windows(2).all(|w| w[0] < w[1]), "{lines:?}");
        for w in lines.windows(2) {
            assert_eq!(g.next(t(w[0]), &m), t(w[1]));
        }
        // Two-tick steps: the exact swung line is 2 2/3 ticks in, so it
        // lands on tick 2, like every other line that falls between ticks.
        let g = Grid::note(1920).with_swing(1.0);
        let lines: Vec<i64> = g.lines(t(0)..t(8), &m).map(|l| l.tick.0).collect();
        assert_eq!(lines, [0, 2, 4, 6]);
        // Swing that does not fit in a tick still moves coarse grids.
        let g = Grid::note(8).with_swing(0.5);
        assert_eq!(g.next(t(0), &m), t(Q / 2 + Q / 12));
    }

    #[test]
    fn results_stay_within_max_tick() {
        let m = Meter::default();
        let g = Grid::note(16);
        for x in [MAX_TICK - 1, MAX_TICK, i64::MAX] {
            let n = g.next(t(x), &m);
            assert!(n.0 <= MAX_TICK, "next({x}) = {n:?}");
            assert_eq!(g.floor(n, &m), n);
            assert!(g.ceil(t(x), &m).0 <= MAX_TICK);
            assert!(g.snap(t(x), &m).0 <= MAX_TICK);
        }
        for x in [-MAX_TICK + 1, -MAX_TICK, i64::MIN] {
            let p = g.prev(t(x), &m);
            assert!(p.0 >= -MAX_TICK, "prev({x}) = {p:?}");
            assert_eq!(g.floor(p, &m), p);
            assert!(g.floor(t(x), &m).0 >= -MAX_TICK);
            assert!(g.snap(t(x), &m).0 >= -MAX_TICK);
        }
        // The edges are lines, so lines() and next() still agree there.
        let lines: Vec<i64> = g
            .lines(t(-MAX_TICK)..t(-MAX_TICK + 500), &m)
            .map(|l| l.tick.0)
            .collect();
        assert_eq!(lines[0], -MAX_TICK);
        for w in lines.windows(2) {
            assert_eq!(g.next(t(w[0]), &m), t(w[1]));
        }
        let lines: Vec<i64> = g
            .lines(t(MAX_TICK - 300)..t(i64::MAX), &m)
            .map(|l| l.tick.0)
            .collect();
        assert_eq!(lines.last(), Some(&MAX_TICK));
        for w in lines.windows(2) {
            assert_eq!(g.next(t(w[0]), &m), t(w[1]));
        }
        assert_eq!(g.lines(t(MAX_TICK)..t(i64::MAX), &m).count(), 1);
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
