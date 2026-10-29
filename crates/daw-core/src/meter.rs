//! Peak meters the RT thread writes and the UI reads, with no queue in between.

use std::sync::atomic::Ordering::Relaxed;

use crate::sync::AtomicU32;

/// Holds the highest absolute sample value seen since the reader last took it.
///
/// Non-negative `f32`s order the same as their bit patterns, so `fetch_max` on the
/// bits keeps the peak without a compare-exchange loop. Share it through an `Arc`.
#[derive(Debug, Default)]
pub struct PeakMeter(AtomicU32);

impl PeakMeter {
    pub fn new() -> Self {
        Self(AtomicU32::new(0))
    }

    /// RT side: raises the stored peak to the peak of `samples`.
    pub fn update(&self, samples: &[f32]) {
        let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        self.update_peak(peak);
    }

    /// RT side: raises the stored peak to `peak`. NaN and negative values are ignored.
    pub fn update_peak(&self, peak: f32) {
        if peak > 0.0 {
            self.0.fetch_max(peak.to_bits(), Relaxed);
        }
    }

    /// Reader side: returns the peak and resets it to zero.
    pub fn take(&self) -> f32 {
        f32::from_bits(self.0.swap(0, Relaxed))
    }

    /// Reader side: returns the peak without resetting it.
    pub fn peek(&self) -> f32 {
        f32::from_bits(self.0.load(Relaxed))
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    #[test]
    fn keeps_highest_peak_until_taken() {
        let m = PeakMeter::new();
        m.update(&[0.1, -0.7, 0.3]);
        m.update(&[0.5]);
        m.update_peak(f32::NAN);
        assert_eq!(m.peek(), 0.7);
        assert_eq!(m.take(), 0.7);
        assert_eq!(m.take(), 0.0);
        m.update(&[f32::INFINITY]);
        assert_eq!(m.take(), f32::INFINITY);
    }
}
