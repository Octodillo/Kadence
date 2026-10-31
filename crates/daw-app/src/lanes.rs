//! Automation lanes as the track headers show them, without Qt.
//!
//! The arrange view (`daw_arrange`) draws and edits the lanes' points; this
//! names lanes and their targets for the headers, adds and removes lanes and
//! keeps each lane's mode: off (the control keeps its own value), read (the
//! lane plays) or write (it plays, and moving its control on the Mix page
//! during playback records over it, as with the mixer's Write Automation
//! switch).

use std::collections::HashSet;

use daw_model::{AutomationTarget, LaneId, Project, Track};

use crate::device_panel::BuiltinParams;
use crate::state::AppState;

/// What a lane automates, as the header words it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LaneKind {
    Volume = 0,
    Pan = 1,
    Send = 2,
    DeviceParam = 3,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LaneMode {
    Off = 0,
    Read = 1,
    Write = 2,
}

impl LaneMode {
    pub fn from_index(i: i32) -> Self {
        match i {
            0 => Self::Off,
            2 => Self::Write,
            _ => Self::Read,
        }
    }
}

/// A target's name in parts: the kind, the send's target track or the
/// device, and the parameter.
#[derive(Clone, Debug, PartialEq)]
pub struct TargetName {
    pub kind: LaneKind,
    /// The bus a send goes to, or the device's name.
    pub detail: String,
    /// The parameter's name (device parameters only).
    pub param: String,
}

/// Names `target` on `track`.
pub fn target_name(
    project: &Project,
    params: &BuiltinParams,
    track: &Track,
    target: AutomationTarget,
) -> TargetName {
    let (kind, detail, param) = match target {
        AutomationTarget::Volume => (LaneKind::Volume, String::new(), String::new()),
        AutomationTarget::Pan => (LaneKind::Pan, String::new(), String::new()),
        AutomationTarget::SendLevel { send } => {
            let to = track
                .sends
                .iter()
                .find(|s| s.id == send)
                .and_then(|s| project.track(s.target))
                .map(|t| t.name.clone())
                .unwrap_or_default();
            (LaneKind::Send, to, String::new())
        }
        AutomationTarget::DeviceParam { device, param } => {
            let d = track.devices.iter().find(|d| d.id == device);
            let name = d.map(|d| d.name.clone()).unwrap_or_default();
            let p = d
                .and_then(|d| params.name(d, param))
                .map_or_else(|| param.to_string(), str::to_owned);
            (LaneKind::DeviceParam, name, p)
        }
    };
    TargetName {
        kind,
        detail,
        param,
    }
}

/// The lane modes and parameter names the app keeps beside the document.
#[derive(Default)]
pub struct Lanes {
    /// Lanes in write mode. Not saved with the project: writing is armed
    /// for this session only, like a record button.
    pub write: HashSet<LaneId>,
    pub params: std::sync::Arc<BuiltinParams>,
}

impl AppState {
    fn track_at_index(&self, index: i32) -> Option<&std::sync::Arc<Track>> {
        usize::try_from(index)
            .ok()
            .and_then(|i| self.doc.project().tracks.get(i))
    }

    fn lane_id(&self, track: i32, lane: i32) -> Option<LaneId> {
        let t = self.track_at_index(track)?;
        Some(t.automation.get(usize::try_from(lane).ok()?)?.id)
    }

    pub fn lane_count(&self, track: i32) -> usize {
        self.track_at_index(track).map_or(0, |t| t.automation.len())
    }

    pub fn lane_name(&self, track: i32, lane: i32) -> Option<TargetName> {
        let t = self.track_at_index(track)?;
        let l = t.automation.get(usize::try_from(lane).ok()?)?;
        Some(target_name(
            self.doc.project(),
            &self.lanes.params,
            t,
            l.target,
        ))
    }

    pub fn lane_mode(&self, track: i32, lane: i32) -> LaneMode {
        let Some(t) = self.track_at_index(track) else {
            return LaneMode::Read;
        };
        let Some(l) = usize::try_from(lane).ok().and_then(|i| t.automation.get(i)) else {
            return LaneMode::Read;
        };
        if !l.enabled {
            LaneMode::Off
        } else if self.lanes.write.contains(&l.id) {
            LaneMode::Write
        } else {
            LaneMode::Read
        }
    }

    /// Sets a lane's mode. Off and on are an undoable edit; write is not
    /// part of the document.
    pub fn set_lane_mode(&mut self, track: i32, lane: i32, mode: LaneMode) {
        let Some(id) = self.lane_id(track, lane) else {
            return;
        };
        if mode == LaneMode::Write {
            self.lanes.write.insert(id);
        } else {
            self.lanes.write.remove(&id);
        }
        let out = self
            .arrange
            .set_lane_enabled(&mut self.doc, id, mode != LaneMode::Off);
        self.apply(out);
        self.touch();
    }

    pub fn remove_lane(&mut self, track: i32, lane: i32) {
        let Some(id) = self.lane_id(track, lane) else {
            return;
        };
        self.lanes.write.remove(&id);
        let out = self.arrange.remove_lane(&mut self.doc, id);
        self.apply(out);
    }

    /// Targets on track `track` that have no lane yet, in menu order.
    pub fn lane_targets(&self, track: i32) -> Vec<(AutomationTarget, TargetName)> {
        let Some(t) = self.track_at_index(track) else {
            return Vec::new();
        };
        daw_arrange::automation::lane_targets(t, self.arrange.params.as_ref())
            .into_iter()
            .map(|target| {
                let name = target_name(self.doc.project(), &self.lanes.params, t, target);
                (target, name)
            })
            .collect()
    }

    /// Adds a lane for the `target`th entry of [`lane_targets`](Self::lane_targets).
    pub fn add_lane(&mut self, track: i32, target: i32) {
        let Some(t) = self.track_at_index(track).map(|t| t.id) else {
            return;
        };
        let Some((target, _)) = usize::try_from(target)
            .ok()
            .and_then(|i| self.lane_targets(track).into_iter().nth(i))
        else {
            return;
        };
        let out = self.arrange.add_lane(&mut self.doc, t, target);
        self.apply(out);
    }

    pub fn lanes_expanded(&self, track: i32) -> bool {
        self.track_at_index(track)
            .is_some_and(|t| self.arrange.expanded.contains(&t.id))
    }

    pub fn set_lanes_expanded(&mut self, track: i32, on: bool) {
        let Some(id) = self.track_at_index(track).map(|t| t.id) else {
            return;
        };
        let project = self.doc.project().clone();
        let out = self.arrange.set_expanded(&project, id, on);
        self.apply(out);
    }

    /// Forgets write mode for lanes that are gone.
    pub(crate) fn prune_lane_modes(&mut self) {
        let p = self.doc.project();
        self.lanes.write.retain(|&l| p.lane_location(l).is_some());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daw_arrange::{LaneScale, ParamCatalog};
    use daw_model::commands::AddDevice;
    use daw_model::{Device, DeviceSource, Document, TrackKind};

    fn eq_device() -> Device {
        Device::new(
            "EQ",
            DeviceSource::Builtin {
                kind: "daw.eq".into(),
            },
        )
    }

    #[test]
    fn builtin_params_have_ranges_and_names() {
        let p = BuiltinParams;
        let d = eq_device();
        let ids = p.automatable(&d);
        assert!(!ids.is_empty());
        for id in ids {
            let s = p.scale(&d, id).expect("range");
            match s {
                LaneScale::Range { min, max, .. } => assert!(max > min),
                LaneScale::Db { .. } => panic!("device params are plain"),
            }
            assert!(p.name(&d, id).is_some());
            assert!(p.default_value(&d, id).is_some());
        }
        let unknown = Device::new(
            "?",
            DeviceSource::Builtin {
                kind: "nope".into(),
            },
        );
        assert!(p.automatable(&unknown).is_empty());
    }

    /// Every built-in parameter's slider and lane run along its taper:
    /// halfway is the geometric mean on a log taper, the midpoint
    /// otherwise, and discrete parameters land on whole numbers.
    #[test]
    fn every_builtin_param_maps_along_its_taper() {
        use daw_devices::{ParamKind, Taper};
        let p = BuiltinParams;
        for entry in daw_devices::devices() {
            let d = Device::new(
                entry.info.name,
                DeviceSource::Builtin {
                    kind: entry.info.id.into(),
                },
            );
            for spec in entry.params {
                let desc = p.param(&d, spec.id).expect("described");
                let half = desc.from_normalized(0.5);
                let expected =
                    if spec.taper == Taper::Log && matches!(spec.kind, ParamKind::Continuous) {
                        (spec.min * spec.max).sqrt()
                    } else if matches!(spec.kind, ParamKind::Continuous) {
                        (spec.min + spec.max) / 2.0
                    } else {
                        ((spec.min + spec.max) / 2.0).round()
                    };
                assert!(
                    (half - expected).abs() < 1e-9 * expected.abs().max(1.0),
                    "{} {}",
                    entry.info.id,
                    spec.name
                );
                assert!((desc.from_normalized(0.0) - spec.min).abs() < 1e-9);
                assert!(
                    (desc.from_normalized(1.0) - spec.max).abs() < 1e-9 * spec.max.abs().max(1.0)
                );
                assert!(
                    (desc.to_normalized(half) - 0.5).abs() < 1e-9
                        || !matches!(spec.kind, ParamKind::Continuous)
                );
                // The lanes' catalog uses the same scale.
                assert_eq!(p.scale(&d, spec.id), Some(desc.scale()));
            }
        }
    }

    #[test]
    fn target_names_name_devices_and_params() {
        let mut doc = Document::new(Project::new("t"));
        let t = Track::new(TrackKind::Audio, "A");
        let id = t.id;
        doc.execute(daw_model::commands::AddTrack::new(0, t))
            .unwrap();
        let d = eq_device();
        doc.execute(AddDevice {
            track: id,
            index: 0,
            device: d,
        })
        .unwrap();
        let p = BuiltinParams;
        let track = doc.project().track(id).unwrap().clone();
        let targets = daw_arrange::automation::lane_targets(
            &track,
            Some(&(std::sync::Arc::new(BuiltinParams) as std::sync::Arc<dyn ParamCatalog>)),
        );
        let last = *targets.last().unwrap();
        let n = target_name(doc.project(), &p, &track, last);
        assert_eq!(n.kind, LaneKind::DeviceParam);
        assert_eq!(n.detail, "EQ");
        assert!(!n.param.is_empty());
        let v = target_name(doc.project(), &p, &track, AutomationTarget::Volume);
        assert_eq!(v.kind, LaneKind::Volume);
    }
}
