//! The model's tempo data as a `daw_core` tempo map.

/// Builds the engine's tempo map from the model's tempo points. The same
/// map daw-midi uses, so MIDI and audio agree on where a beat falls.
pub use daw_midi::model::tempo_map;

#[cfg(test)]
mod tests {
    use daw_core::time::{PPQ, SampleTime, TempoMap, TickTime};
    use daw_model::{TempoData, TempoPoint};

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
