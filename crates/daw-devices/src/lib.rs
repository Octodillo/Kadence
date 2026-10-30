//! Built-in instruments and effects.
//!
//! Every device is a [`Node`] with parameters described by [`ParamSpec`]
//! (and the contract's [`ParamInfo`]). Parameters can be changed off the
//! real-time thread with [`Device::set_param`], or sample-accurately on it by
//! sending [`EventKind::ParamValue`](daw_core::event::EventKind::ParamValue)
//! events through `ProcessContext::events_in`.
//!
//! [`devices()`] lists every device with its constructor.

use std::any::Any;

use daw_core::event::ParamId;
use daw_core::node::Node;
use daw_core::plugin::ParamInfo;

pub mod devices;
pub mod dsp;
mod events;
pub mod param;
mod state;

pub use devices::compressor::Compressor;
pub use devices::delay::Delay;
pub use devices::eq::ParametricEq;
pub use devices::metronome::Metronome;
pub use devices::reverb::Reverb;
pub use devices::sampler::{SampleBuffer, Sampler, Zone};
pub use devices::synth::Synth;
pub use devices::utility::Utility;
pub use param::{ParamKind, ParamSpec, ParamUnit, Taper};
pub use state::StateError;

/// What a device is for, so the UI can group the catalog.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum DeviceCategory {
    Instrument,
    AudioEffect,
    Utility,
}

/// Static description of a built-in device.
#[derive(Debug)]
pub struct DeviceInfo {
    /// Stable identifier stored in project files, e.g. `daw.synth`.
    pub id: &'static str,
    /// Display name (English; the UI translates it).
    pub name: &'static str,
    pub category: DeviceCategory,
    pub description: &'static str,
}

/// A built-in instrument or effect.
pub trait Device: Node {
    fn info(&self) -> &'static DeviceInfo;

    /// Parameter metadata. A parameter's id equals its index in this slice.
    fn param_specs(&self) -> &'static [ParamSpec];

    /// Current plain value of a parameter. Unknown ids return 0.
    fn param_value(&self, id: ParamId) -> f64;

    /// Set a parameter's plain value (clamped to its range). Real-time safe,
    /// so it is also what `process` calls for `ParamValue` events.
    fn set_param(&mut self, id: ParamId, value: f64);

    /// Parameter metadata in the contract's form.
    fn params(&self) -> Vec<ParamInfo> {
        self.param_specs().iter().map(ParamSpec::info).collect()
    }

    /// Serialize all parameter values, plus any device-specific state such as
    /// the sampler's zone map. Audio data itself is not included: the sampler
    /// stores each sample's file path, and the project keeps the files.
    fn save_state(&self) -> Vec<u8> {
        state::save(self.info(), self.param_specs(), |id| self.param_value(id))
    }

    /// Restore parameter values saved by [`Device::save_state`]. Parameters
    /// missing from `data` go back to their defaults; unknown ones are ignored.
    fn load_state(&mut self, data: &[u8]) -> Result<(), StateError> {
        let values = state::load(self.info(), self.param_specs(), data)?;
        for (id, value) in values.into_iter().enumerate() {
            self.set_param(id as ParamId, value);
        }
        self.snap_smoothing();
        Ok(())
    }

    /// Jump smoothed parameters straight to their current values, so the next
    /// block starts there instead of gliding from the old ones. Loading state
    /// does this; a preset restored after `prepare` would otherwise fade in
    /// from the previous settings. Call off the real-time thread.
    fn snap_smoothing(&mut self) {}

    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;

    /// A gain reduction meter (dB, positive) for dynamics devices, readable
    /// from another thread once the device runs in the engine.
    fn gain_reduction_meter(&self) -> Option<std::sync::Arc<daw_core::meter::PeakMeter>> {
        None
    }
}

/// A catalog entry: what the device is and how to make one.
pub struct DeviceEntry {
    pub info: &'static DeviceInfo,
    pub create: fn() -> Box<dyn Device>,
}

impl std::fmt::Debug for DeviceEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceEntry")
            .field("info", self.info)
            .finish()
    }
}

static CATALOG: &[DeviceEntry] = &[
    DeviceEntry {
        info: &devices::utility::INFO,
        create: || Box::new(Utility::new()),
    },
    DeviceEntry {
        info: &devices::metronome::INFO,
        create: || Box::new(Metronome::new()),
    },
    DeviceEntry {
        info: &devices::sampler::INFO,
        create: || Box::new(Sampler::new()),
    },
    DeviceEntry {
        info: &devices::synth::INFO,
        create: || Box::new(Synth::new()),
    },
    DeviceEntry {
        info: &devices::eq::INFO,
        create: || Box::new(ParametricEq::new()),
    },
    DeviceEntry {
        info: &devices::compressor::INFO,
        create: || Box::new(Compressor::new()),
    },
    DeviceEntry {
        info: &devices::delay::INFO,
        create: || Box::new(Delay::new()),
    },
    DeviceEntry {
        info: &devices::reverb::INFO,
        create: || Box::new(Reverb::new()),
    },
];

/// Every built-in device, in catalog order.
pub fn devices() -> &'static [DeviceEntry] {
    CATALOG
}

/// Create a device by its stable id.
pub fn create(id: &str) -> Option<Box<dyn Device>> {
    CATALOG
        .iter()
        .find(|e| e.info.id == id)
        .map(|e| (e.create)())
}

/// Implements the boilerplate parts of [`Device`] for a struct with
/// `params: ParamStore` and an `apply_param(&mut self, id)` method.
macro_rules! impl_device {
    ($ty:ty, $info:expr, $specs:expr $(, { $($extra:tt)* })?) => {
        impl $crate::Device for $ty {
            $($($extra)*)?
            fn info(&self) -> &'static $crate::DeviceInfo {
                &$info
            }
            fn param_specs(&self) -> &'static [$crate::ParamSpec] {
                $specs
            }
            fn param_value(&self, id: daw_core::event::ParamId) -> f64 {
                self.params.get(id)
            }
            fn set_param(&mut self, id: daw_core::event::ParamId, value: f64) {
                if self.params.set(id, value) {
                    self.apply_param(id);
                }
            }
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
            fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
                self
            }
        }
    };
}
pub(crate) use impl_device;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_ids_are_unique_and_creatable() {
        let mut ids: Vec<_> = devices().iter().map(|e| e.info.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), devices().len());
        for e in devices() {
            let d = create(e.info.id).expect("creatable");
            assert_eq!(d.info().id, e.info.id);
        }
        assert!(create("nope").is_none());
    }

    #[test]
    fn param_ids_match_indices_and_defaults_are_in_range() {
        for e in devices() {
            let d = (e.create)();
            for (i, s) in d.param_specs().iter().enumerate() {
                assert_eq!(s.id as usize, i, "{} {}", e.info.id, s.key);
                assert!(
                    s.min <= s.default && s.default <= s.max,
                    "{} {}",
                    e.info.id,
                    s.key
                );
                assert_eq!(d.param_value(s.id), s.default, "{} {}", e.info.id, s.key);
            }
            let mut keys: Vec<_> = d.param_specs().iter().map(|s| s.key).collect();
            keys.sort();
            keys.dedup();
            assert_eq!(keys.len(), d.param_specs().len(), "{}", e.info.id);
        }
    }

    #[test]
    fn state_round_trips() {
        for e in devices() {
            let mut a = (e.create)();
            for s in a.param_specs() {
                let v = s.min + (s.max - s.min) * 0.3;
                a.set_param(s.id, v);
            }
            let blob = a.save_state();
            let mut b = (e.create)();
            b.load_state(&blob).unwrap();
            for s in a.param_specs() {
                assert_eq!(
                    a.param_value(s.id),
                    b.param_value(s.id),
                    "{} {}",
                    e.info.id,
                    s.key
                );
            }
        }
    }

    #[test]
    fn state_rejects_other_devices() {
        let synth = create("daw.synth").unwrap();
        let mut eq = create("daw.eq").unwrap();
        assert!(eq.load_state(&synth.save_state()).is_err());
        assert!(eq.load_state(b"not json").is_err());
    }
}
