//! Sample time, musical time, and the conversion between them.

use serde::{Deserialize, Serialize};

/// Ticks per quarter note used everywhere in the project model.
pub const PPQ: i64 = 960;

/// Samples from project start.
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct SampleTime(pub i64);

/// [`PPQ`] ticks from project start.
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct TickTime(pub i64);

/// Converts between musical and sample time.
///
/// Implementations must be allocation free: the engine calls these on the
/// audio thread. A map is immutable once built; edits produce a new map that
/// is swapped in. The implementation with tempo changes lives in WU1.
pub trait TempoMap: Send + Sync {
    fn ticks_to_samples(&self, t: TickTime, sample_rate: f64) -> SampleTime;
    fn samples_to_ticks(&self, s: SampleTime, sample_rate: f64) -> TickTime;
    fn bpm_at(&self, t: TickTime) -> f64;
}
