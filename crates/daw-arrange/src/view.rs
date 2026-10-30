//! The horizontal mapping between musical time and pixels, shared by every
//! timeline-style editor (arrange view, piano roll, automation lanes).

use daw_core::time::{PPQ, TickTime};

/// Smallest and largest horizontal zoom, in pixels per beat.
pub const MIN_PX_PER_BEAT: f64 = 0.5;
pub const MAX_PX_PER_BEAT: f64 = 2000.0;

/// Horizontal zoom and scroll. `x` is in logical pixels from the left edge
/// of the timeline area (not the window); beats are quarter notes (`PPQ`
/// ticks) whatever the meter.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct TimelineView {
    /// Pixels per quarter note.
    pub px_per_beat: f64,
    /// Pixels between tick 0 and the left edge of the view.
    pub scroll_x: f64,
}

impl Default for TimelineView {
    fn default() -> Self {
        Self {
            px_per_beat: 32.0,
            scroll_x: 0.0,
        }
    }
}

impl TimelineView {
    pub fn px_per_tick(&self) -> f64 {
        self.px_per_beat / PPQ as f64
    }

    pub fn tick_to_x(&self, t: TickTime) -> f64 {
        t.0 as f64 * self.px_per_tick() - self.scroll_x
    }

    /// The tick under `x`, rounded down.
    pub fn x_to_tick(&self, x: f64) -> TickTime {
        TickTime(((x + self.scroll_x) / self.px_per_tick()).floor() as i64)
    }

    /// A horizontal distance in pixels as ticks.
    pub fn dx_to_ticks(&self, dx: f64) -> i64 {
        (dx / self.px_per_tick()).round() as i64
    }

    /// The ticks visible in a view `width` pixels wide.
    pub fn visible(&self, width: f64) -> std::ops::Range<TickTime> {
        self.x_to_tick(0.0)..TickTime(self.x_to_tick(width).0 + 1)
    }

    /// Zooms by `factor` keeping the tick under `anchor_x` where it is.
    pub fn zoom_at(&mut self, factor: f64, anchor_x: f64) {
        let ticks = (anchor_x + self.scroll_x) / self.px_per_tick();
        self.px_per_beat = (self.px_per_beat * factor).clamp(MIN_PX_PER_BEAT, MAX_PX_PER_BEAT);
        self.scroll_x = (ticks * self.px_per_tick() - anchor_x).max(0.0);
    }

    pub fn scroll_by(&mut self, dx: f64) {
        self.scroll_x = (self.scroll_x + dx).max(0.0);
    }

    /// Scrolls just enough to bring `t` into a view `width` wide, keeping a
    /// `margin` of pixels on either side (for following the playhead).
    pub fn reveal(&mut self, t: TickTime, width: f64, margin: f64) {
        let x = self.tick_to_x(t);
        if x < margin {
            self.scroll_by(x - margin);
        } else if x > width - margin {
            self.scroll_by(x - (width - margin));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_keeps_anchor_fixed() {
        let mut v = TimelineView {
            px_per_beat: 20.0,
            scroll_x: 100.0,
        };
        let before = v.x_to_tick(300.0);
        v.zoom_at(2.0, 300.0);
        assert_eq!(v.px_per_beat, 40.0);
        assert!((v.tick_to_x(before) - 300.0).abs() < 1.0);
    }

    #[test]
    fn zoom_is_clamped_and_scroll_never_negative() {
        let mut v = TimelineView::default();
        v.zoom_at(1e9, 0.0);
        assert_eq!(v.px_per_beat, MAX_PX_PER_BEAT);
        v.zoom_at(1e-9, 500.0);
        assert_eq!(v.px_per_beat, MIN_PX_PER_BEAT);
        assert!(v.scroll_x >= 0.0);
    }

    #[test]
    fn round_trip() {
        let v = TimelineView {
            px_per_beat: 48.0,
            scroll_x: 10.0,
        };
        for t in [0, 1, PPQ, 7 * PPQ + 3] {
            let x = v.tick_to_x(TickTime(t));
            let back = v.x_to_tick(x + 1e-6);
            assert!((back.0 - t).abs() <= 1, "{t} -> {x} -> {back:?}");
        }
    }
}
