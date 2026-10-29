//! Snapping and grid lines for the arrange view, on top of `daw_midi::grid`.

use daw_core::time::{PPQ, TickTime};
use daw_midi::grid::{Division, Grid, Meter, MeterPoint};
use daw_model::TempoData;

/// Grid lines closer than this are not drawn (and not snapped to when the
/// grid is adaptive).
pub const MIN_LINE_SPACING: f64 = 12.0;

/// How edits snap.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub enum Snap {
    /// To the finest musical grid that is still comfortably far apart at the
    /// current zoom.
    #[default]
    Adaptive,
    Fixed(Grid),
    Off,
}

/// The project's meter map in the form the grid wants.
pub fn meter(tempo: &TempoData) -> Meter {
    Meter::new(tempo.meters.iter().map(|m| MeterPoint {
        tick: m.tick,
        numerator: m.numerator,
        denominator: m.denominator,
    }))
}

/// Candidate grids from fine to coarse.
const STEPS: [Division; 7] = [
    Division::Note(64),
    Division::Note(32),
    Division::Note(16),
    Division::Note(8),
    Division::Beat,
    Division::Note(2),
    Division::Bar,
];

/// Rough length of one step in ticks, assuming 4/4, for choosing a grid.
fn approx_ticks(d: Division) -> f64 {
    match d {
        Division::Note(n) => 4.0 * PPQ as f64 / f64::from(n),
        Division::Beat => PPQ as f64,
        Division::Bar => 4.0 * PPQ as f64,
    }
}

/// The finest grid whose steps are at least `spacing` pixels apart, or
/// `None` when even bars are closer than that.
pub fn adaptive_grid(px_per_tick: f64, spacing: f64) -> Option<Grid> {
    STEPS
        .iter()
        .find(|&&d| approx_ticks(d) * px_per_tick >= spacing)
        .map(|&d| Grid::new(d))
}

/// How many bars to skip between drawn bar lines so they are at least
/// `spacing` apart.
pub fn bar_stride(px_per_tick: f64, spacing: f64, meter: &Meter, at: TickTime) -> i64 {
    let bar = meter.bar_at(at).len as f64 * px_per_tick;
    let mut stride = 1;
    while bar * (stride as f64) < spacing && stride < 1 << 20 {
        stride *= 2;
    }
    stride
}

impl Snap {
    /// The grid edits snap to at this zoom, if any.
    pub fn grid(&self, px_per_tick: f64) -> Option<Grid> {
        match self {
            Snap::Adaptive => adaptive_grid(px_per_tick, MIN_LINE_SPACING)
                .or(Some(Grid::new(Division::Bar))),
            Snap::Fixed(g) => Some(*g),
            Snap::Off => None,
        }
    }

    /// `t` moved to the nearest grid line, or unchanged with snapping off.
    pub fn apply(&self, t: TickTime, px_per_tick: f64, meter: &Meter) -> TickTime {
        match self.grid(px_per_tick) {
            Some(g) => g.snap(t, meter),
            None => t,
        }
    }

    /// The grid line at or before `t`.
    pub fn floor(&self, t: TickTime, px_per_tick: f64, meter: &Meter) -> TickTime {
        match self.grid(px_per_tick) {
            Some(g) => g.floor(t, meter),
            None => t,
        }
    }

    /// One grid step after `t` (for nudging); a beat with snapping off.
    pub fn next(&self, t: TickTime, px_per_tick: f64, meter: &Meter) -> TickTime {
        match self.grid(px_per_tick) {
            Some(g) => g.next(t, meter),
            None => TickTime(t.0 + PPQ),
        }
    }

    pub fn prev(&self, t: TickTime, px_per_tick: f64, meter: &Meter) -> TickTime {
        match self.grid(px_per_tick) {
            Some(g) => g.prev(t, meter),
            None => TickTime(t.0 - PPQ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adaptive_grid_coarsens_when_zooming_out() {
        // 32 px per beat: sixteenths are 8 px, eighths 16 px.
        let g = adaptive_grid(32.0 / PPQ as f64, 12.0).unwrap();
        assert_eq!(g.division, Division::Note(8));
        let g = adaptive_grid(4.0 / PPQ as f64, 12.0).unwrap();
        assert_eq!(g.division, Division::Bar);
        assert!(adaptive_grid(1.0 / PPQ as f64, 12.0).is_none());
    }

    #[test]
    fn snapping_uses_the_meter() {
        let m = meter(&TempoData::default());
        let snap = Snap::Fixed(Grid::new(Division::Bar));
        let ppt = 1.0;
        assert_eq!(snap.apply(TickTime(PPQ), ppt, &m), TickTime(0));
        assert_eq!(snap.apply(TickTime(3 * PPQ), ppt, &m), TickTime(4 * PPQ));
        assert_eq!(Snap::Off.apply(TickTime(123), ppt, &m), TickTime(123));
    }
}
