//! The files built-in devices' saved state plays, for the model's lists of
//! missing and external files and its relink and collect edits.

use std::path::{Path, PathBuf};

use daw_devices::Sampler;
use daw_model::files::StateFiles;
use daw_model::{Device, DeviceSource, StateBlob};

/// Whether `device` is a built-in sampler, the device whose saved state
/// names sample files.
pub fn is_sampler(device: &Device) -> bool {
    matches!(&device.source, DeviceSource::Builtin { kind } if is_sampler_kind(kind))
}

pub(crate) fn is_sampler_kind(kind: &str) -> bool {
    kind == daw_devices::devices::sampler::INFO.id
}

/// Reads the built-in devices' saved state: a sampler's samples. A plugin's
/// state is its own, and names no file Kadence knows of.
pub struct BuiltinStateFiles;

impl StateFiles for BuiltinStateFiles {
    fn files(&self, device: &Device) -> Vec<PathBuf> {
        match &device.state {
            Some(state) if is_sampler(device) => {
                Sampler::saved_samples(&state.0).unwrap_or_default()
            }
            _ => Vec::new(),
        }
    }

    fn relinked(
        &self,
        device: &Device,
        to: &dyn Fn(&Path) -> Option<PathBuf>,
    ) -> Option<StateBlob> {
        let state = device.state.as_ref().filter(|_| is_sampler(device))?;
        Sampler::relinked_state(&state.0, to)
            .ok()
            .flatten()
            .map(StateBlob::from)
    }
}

#[cfg(test)]
mod tests {
    use daw_model::files::StateFiles;

    use super::*;

    fn sampler(state: Option<Vec<u8>>) -> Device {
        let mut d = Device::new(
            "Sampler",
            DeviceSource::Builtin {
                kind: daw_devices::devices::sampler::INFO.id.into(),
            },
        );
        d.state = state.map(StateBlob::from);
        d
    }

    #[test]
    fn a_sampler_names_its_sample_and_takes_a_new_one() {
        let d = sampler(Some(Sampler::state_with_sample("audio/hit.wav")));
        assert_eq!(
            BuiltinStateFiles.files(&d),
            [PathBuf::from("audio/hit.wav")]
        );
        let moved = BuiltinStateFiles
            .relinked(&d, &|_| Some(PathBuf::from("/samples/hit.wav")))
            .unwrap();
        assert_eq!(
            Sampler::saved_samples(&moved.0).unwrap(),
            [PathBuf::from("/samples/hit.wav")]
        );
        assert!(BuiltinStateFiles.relinked(&d, &|_| None).is_none());
    }

    #[test]
    fn other_devices_and_empty_samplers_name_nothing() {
        assert!(BuiltinStateFiles.files(&sampler(None)).is_empty());
        let mut eq = sampler(Some(Sampler::state_with_sample("a.wav")));
        eq.source = DeviceSource::Builtin {
            kind: "daw.eq".into(),
        };
        assert!(BuiltinStateFiles.files(&eq).is_empty());
        assert!(
            BuiltinStateFiles
                .relinked(&eq, &|_| Some(PathBuf::from("b.wav")))
                .is_none()
        );
    }
}
