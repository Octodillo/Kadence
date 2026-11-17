//! Reports what the audio thread had to drop.
//!
//! The engine cannot log from the audio thread; it counts. [`DropLog`] reads
//! the counts on the control side and writes a warning when they move.

use std::time::{Duration, Instant};

use daw_engine::EngineController;

/// The shortest time between two warnings, so a block that overflows every
/// period does not flood the log.
const INTERVAL: Duration = Duration::from_secs(5);

/// The engine's drop counters as of the last warning.
#[derive(Default)]
pub(crate) struct DropLog {
    events: u64,
    params: u64,
    warned_at: Option<Instant>,
}

impl DropLog {
    /// Logs a warning if the engine has dropped events or parameter changes
    /// since the last one, at most once per [`INTERVAL`]. Call on the
    /// session's timer.
    pub(crate) fn report(&mut self, engine: &EngineController) {
        let now = Instant::now();
        if let Some(message) = self.message(engine.dropped_events(), engine.dropped_params(), now) {
            log::warn!("{message}");
        }
    }

    /// The warning for the counters' current values, if one is due.
    fn message(&mut self, events: u64, params: u64, now: Instant) -> Option<String> {
        let (new_events, new_params) = (events - self.events, params - self.params);
        let waited = self
            .warned_at
            .is_none_or(|at| now.duration_since(at) >= INTERVAL);
        if (new_events == 0 && new_params == 0) || !waited {
            return None;
        }
        (self.events, self.params, self.warned_at) = (events, params, Some(now));
        Some(format!(
            "the engine dropped {new_events} events (more reached one device in a block than \
             fit; a lost note-off leaves a note sounding) and {new_params} parameter changes"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warns_when_a_count_moves_and_not_more_often_than_the_interval() {
        let mut log = DropLog::default();
        let start = Instant::now();
        assert_eq!(log.message(0, 0, start), None);
        let first = log.message(3, 1, start).expect("counts moved");
        assert!(first.contains("dropped 3 events"), "{first}");
        assert!(first.contains("1 parameter changes"), "{first}");
        // More drops straight away wait for the interval, then are reported
        // together, counted from the last warning.
        assert_eq!(log.message(5, 1, start + INTERVAL / 2), None);
        let second = log
            .message(9, 1, start + INTERVAL)
            .expect("interval passed");
        assert!(second.contains("dropped 6 events"), "{second}");
        assert!(second.contains("0 parameter changes"), "{second}");
        assert_eq!(log.message(9, 1, start + 3 * INTERVAL), None);
    }
}
