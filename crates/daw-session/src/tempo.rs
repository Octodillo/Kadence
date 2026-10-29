//! The model's tempo data as a `daw_core` tempo map.

use daw_core::time::SegmentTempoMap;
use daw_model::TempoData;

/// Builds the engine's tempo map from the model's tempo points.
pub fn tempo_map(data: &TempoData) -> SegmentTempoMap {
    let first = data.tempos.first().map_or(120.0, |p| p.bpm);
    let mut map = SegmentTempoMap::new(first);
    for p in &data.tempos {
        map.set_tempo(p.tick, p.bpm, p.ramp);
    }
    map
}

#[cfg(test)]
mod tests {
    use daw_core::time::{PPQ, SampleTime, TempoMap, TickTime};
    use daw_model::TempoPoint;

    use super::*;

    #[test]
    fn tempo_change_moves_later_beats() {
        let mut data = TempoData::default();
        data.tempos.push(TempoPoint {
            tick: TickTime(4 * PPQ),
            bpm: 60.0,
            ramp: false,
        });
        let map = tempo_map(&data);
        // Four beats at 120 bpm, then one at 60 bpm: 2 s + 1 s.
        assert_eq!(
            map.ticks_to_samples(TickTime(5 * PPQ), 48_000.0),
            SampleTime(3 * 48_000)
        );
    }
}
