//! Saved device state: a small JSON object of parameter values by key.

use std::collections::BTreeMap;

use daw_core::event::ParamId;
use serde::{Deserialize, Serialize};

use crate::{DeviceInfo, ParamSpec};

const VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("state is not valid: {0}")]
    Invalid(#[from] serde_json::Error),
    #[error("state belongs to device {found}, not {expected}")]
    WrongDevice {
        expected: &'static str,
        found: String,
    },
    #[error("state version {0} is newer than this build understands")]
    TooNew(u32),
}

#[derive(Serialize, Deserialize)]
struct Saved {
    device: String,
    version: u32,
    params: BTreeMap<String, f64>,
}

pub(crate) fn save(
    info: &DeviceInfo,
    specs: &[ParamSpec],
    value: impl Fn(ParamId) -> f64,
) -> Vec<u8> {
    let saved = Saved {
        device: info.id.to_owned(),
        version: VERSION,
        params: specs
            .iter()
            .map(|s| (s.key.to_owned(), value(s.id)))
            .collect(),
    };
    serde_json::to_vec(&saved).expect("serializing plain data cannot fail")
}

/// Returns a value for every parameter, in id order.
pub(crate) fn load(
    info: &DeviceInfo,
    specs: &[ParamSpec],
    data: &[u8],
) -> Result<Vec<f64>, StateError> {
    let saved: Saved = serde_json::from_slice(data)?;
    if saved.device != info.id {
        return Err(StateError::WrongDevice {
            expected: info.id,
            found: saved.device,
        });
    }
    if saved.version > VERSION {
        return Err(StateError::TooNew(saved.version));
    }
    Ok(specs
        .iter()
        .map(|s| saved.params.get(s.key).copied().unwrap_or(s.default))
        .collect())
}
