//! Saved device state: a small JSON object of parameter values by key.

use std::collections::BTreeMap;

use daw_core::event::ParamId;
use serde::{Deserialize, Serialize};

use crate::{DeviceInfo, ParamSpec};

const VERSION: u32 = 2;
/// Version written when there is no extra data, so state from devices that
/// don't use it still loads in builds from before it existed.
const VERSION_PARAMS_ONLY: u32 = 1;

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
    /// Device-specific data beyond parameter values, such as the sampler's
    /// zone map.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    extra: Option<serde_json::Value>,
}

pub(crate) fn save(
    info: &DeviceInfo,
    specs: &[ParamSpec],
    value: impl Fn(ParamId) -> f64,
) -> Vec<u8> {
    save_with(info, specs, value, None)
}

pub(crate) fn save_with(
    info: &DeviceInfo,
    specs: &[ParamSpec],
    value: impl Fn(ParamId) -> f64,
    extra: Option<serde_json::Value>,
) -> Vec<u8> {
    let saved = Saved {
        device: info.id.to_owned(),
        version: if extra.is_some() {
            VERSION
        } else {
            VERSION_PARAMS_ONLY
        },
        params: specs
            .iter()
            .map(|s| (s.key.to_owned(), value(s.id)))
            .collect(),
        extra,
    };
    serde_json::to_vec(&saved).expect("serializing plain data cannot fail")
}

/// Returns a value for every parameter, in id order.
pub(crate) fn load(
    info: &DeviceInfo,
    specs: &[ParamSpec],
    data: &[u8],
) -> Result<Vec<f64>, StateError> {
    load_with(info, specs, data).map(|(values, _)| values)
}

/// Like [`load`], and also returns the device-specific extra data, if any.
pub(crate) fn load_with(
    info: &DeviceInfo,
    specs: &[ParamSpec],
    data: &[u8],
) -> Result<(Vec<f64>, Option<serde_json::Value>), StateError> {
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
    let values = specs
        .iter()
        .map(|s| saved.params.get(s.key).copied().unwrap_or(s.default))
        .collect();
    Ok((values, saved.extra))
}
