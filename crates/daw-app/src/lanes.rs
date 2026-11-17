//! Automation lanes as the track headers show them, without Qt.
//!
//! The arrange view (`daw_arrange`) draws and edits the lanes' points; this
//! names lanes and their targets for the headers, adds and removes lanes and
//! keeps each lane's mode: off (the control keeps its own value), read (the
//! lane plays) or write (it plays, and moving its control during playback
//! records over it, as with the mixer's Write Automation switch). The
//! control is the same wherever it is moved: on the Mix page, in the device
//! panel or in the lane's header, which goes through the same mixer. A
//! control is addressed by its target, so it works with or without a lane.

use std::collections::HashSet;

use daw_arrange::automation::{lane_scale, static_value};
use daw_arrange::rows::{LaneRow, track_rows};
use daw_model::{AutomationLane, AutomationTarget, LaneId, Project, Track};

use crate::device_panel::{BuiltinParams, ParamDesc};
use crate::message::Message;
use crate::mixer::{Control, StripId};
use crate::session_mixer::{ParamStart, strip_id};
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

/// A lane's control as its header shows it: what the target is set to,
/// or what the lane plays at the playhead.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct LaneControl {
    /// Along the lane's scale, 0 (bottom) to 1 (top), as the timeline
    /// draws it.
    pub norm: f64,
    /// In the target's own unit: dB, pan position or plain value.
    pub value: f64,
    /// The lane is on and has points, so it sets the value as it plays and
    /// the control follows it.
    pub following: bool,
    /// Moving the control during playback records over the lane (it is in
    /// write mode, or the mixer writes automation), as the mixer's and
    /// device panel's controls do.
    pub writes: bool,
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

    fn lane_at(&self, track: i32, lane: i32) -> Option<(&std::sync::Arc<Track>, &AutomationLane)> {
        let t = self.track_at_index(track)?;
        let l = t.automation.get(usize::try_from(lane).ok()?)?;
        Some((t, l))
    }

    /// What lane `lane` of track `track` automates.
    fn lane_target(&self, track: i32, lane: i32) -> Option<AutomationTarget> {
        self.lane_at(track, lane).map(|(_, l)| l.target)
    }

    /// The value lane `lane` of track `track` controls, as it is now.
    pub fn lane_control(&self, track: i32, lane: i32) -> Option<LaneControl> {
        self.target_control(track, self.lane_target(track, lane)?)
    }

    /// The rows track `track` shows under it when expanded, as the
    /// timeline lays them out.
    pub fn track_rows(&self, track: i32) -> Vec<LaneRow> {
        self.track_at_index(track)
            .map_or_else(Vec::new, |t| track_rows(t))
    }

    /// The index among the track's lanes of the lane in row `row`.
    pub fn row_lane(&self, track: i32, row: i32) -> Option<usize> {
        let t = self.track_at_index(track)?;
        let id = self
            .track_rows(track)
            .get(usize::try_from(row).ok()?)?
            .lane?;
        t.automation.iter().position(|l| l.id == id)
    }

    /// See [`begin_control`](Self::begin_control).
    pub fn begin_lane_control(&mut self, track: i32, lane: i32) {
        if let Some(target) = self.lane_target(track, lane) {
            self.begin_control(track, target);
        }
    }

    /// See [`set_control`](Self::set_control).
    pub fn set_lane_control(&mut self, track: i32, lane: i32, norm: f64) {
        if let Some(target) = self.lane_target(track, lane) {
            self.set_control(track, target, norm);
        }
    }

    /// See [`end_control`](Self::end_control).
    pub fn end_lane_control(&mut self, track: i32, lane: i32) {
        if let Some(target) = self.lane_target(track, lane) {
            self.end_control(track, target);
        }
    }

    /// How a device-parameter lane's parameter is described (its unit,
    /// control and choices); `None` for other lanes and unknown devices.
    pub fn lane_param(&self, track: i32, lane: i32) -> Option<ParamDesc> {
        self.target_param(track, self.lane_target(track, lane)?)
    }

    // -----------------------------------------------------------------------
    // Controls by target. A control is there whether or not a lane
    // automates it, so these take the target and look for its lane.

    /// `target` on track `track` as it is now: what it is set to, or what
    /// its lane plays at the playhead. `None` if the track has no such
    /// control.
    pub fn target_control(&self, track: i32, target: AutomationTarget) -> Option<LaneControl> {
        let t = self.track_at_index(track)?.as_ref();
        if !t.has_target(target) {
            return None;
        }
        let lane = t.lane_for(target);
        let scale = lane_scale(self.arrange.params.as_ref(), t, target);
        let following = lane.is_some_and(|l| l.enabled && !l.points.is_empty());
        let value = lane
            .filter(|_| following)
            .and_then(|l| l.value_at(self.playhead()))
            .unwrap_or_else(|| static_value(self.arrange.params.as_ref(), t, target));
        Some(LaneControl {
            norm: scale.to_norm(value),
            value,
            following,
            writes: self.mixer.write()
                || lane.is_some_and(|l| l.enabled && self.lanes.write.contains(&l.id)),
        })
    }

    /// The mixer control behind `target`: its strip and control, and for a
    /// built-in device's parameter, its description.
    fn mixer_control(
        &self,
        track: i32,
        target: AutomationTarget,
    ) -> Option<(StripId, Control, Option<ParamDesc>)> {
        let t = self.track_at_index(track)?;
        let control = match target {
            AutomationTarget::Volume => Control::Volume,
            AutomationTarget::Pan => Control::Pan,
            AutomationTarget::SendLevel { send } => {
                let send = t.sends.iter().find(|s| s.id == send)?;
                Control::Send(strip_id(send.target))
            }
            AutomationTarget::DeviceParam { device, param } => Control::Param { device, param },
        };
        Some((strip_id(t.id), control, self.target_param(track, target)))
    }

    /// Runs a change to a control as the mixer runs its own: after pending
    /// edits elsewhere, with the lanes' write modes, then plays it.
    fn run_control(
        &mut self,
        track: i32,
        target: AutomationTarget,
        f: impl FnOnce(&mut Self, StripId, Control, Option<ParamDesc>) -> Result<(), Message>,
    ) {
        let Some((id, control, param)) = self.mixer_control(track, target) else {
            return;
        };
        self.end_drag();
        self.flush_plugin_changes();
        self.mixer.write_lanes.clone_from(&self.lanes.write);
        let r = f(self, id, control, param);
        self.sync();
        if let Err(e) = r {
            self.report(e);
        }
    }

    /// The user grabbed a control: a new undo step, and during playback
    /// with writing armed, a take, as grabbing the same control on the Mix
    /// page or device panel would start.
    pub fn begin_control(&mut self, track: i32, target: AutomationTarget) {
        let static_value = self
            .track_at_index(track)
            .map(|t| static_value(self.arrange.params.as_ref(), t, target));
        self.run_control(track, target, |s, id, control, param| {
            s.doc.seal();
            let session = s.engine.session_mut();
            match param {
                None => s.mixer.touch(&mut s.doc, session, id, control, true),
                Some(p) if p.records() => {
                    let start = ParamStart {
                        value: static_value.unwrap_or(p.default),
                        tolerance: p.tolerance(),
                    };
                    s.mixer.touch_param(&mut s.doc, session, id, control, start)
                }
                Some(_) => Ok(()),
            }
        });
    }

    /// Moves a control to height `norm` on its scale (the one its lane is
    /// drawn on). Like the mixer's controls, it records while a take runs and otherwise sets
    /// the static value; a control whose lane plays its points ignores it
    /// unless the lane writes. Moves until
    /// [`end_control`](Self::end_control) are one undo step.
    pub fn set_control(&mut self, track: i32, target: AutomationTarget, norm: f64) {
        if !norm.is_finite() {
            return;
        }
        let Some(c) = self.target_control(track, target) else {
            return;
        };
        if c.following && !c.writes {
            return;
        }
        let Some(scale) = self
            .track_at_index(track)
            .map(|t| lane_scale(self.arrange.params.as_ref(), t, target))
        else {
            return;
        };
        self.run_control(track, target, |s, id, control, param| {
            let value = param.map_or_else(|| scale.from_norm(norm), |p| p.from_normalized(norm));
            let session = s.engine.session_mut();
            s.mixer.set(&mut s.doc, session, id, control, value)
        });
    }

    /// The user let go of a control: ends its take and the undo step.
    pub fn end_control(&mut self, track: i32, target: AutomationTarget) {
        self.run_control(track, target, |s, id, control, _| {
            let session = s.engine.session_mut();
            s.mixer.touch(&mut s.doc, session, id, control, false)
        });
    }

    /// How a device parameter is described (its unit, control and
    /// choices); `None` for other targets and unknown devices.
    pub fn target_param(&self, track: i32, target: AutomationTarget) -> Option<ParamDesc> {
        let AutomationTarget::DeviceParam { device, param } = target else {
            return None;
        };
        let t = self.track_at_index(track)?;
        let d = t.devices.iter().find(|d| d.id == device)?;
        self.lanes.params.param(d, param)
    }

    // -----------------------------------------------------------------------
    // Lanes

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

    /// A control answers by target whether or not a lane automates it, and
    /// not at all for something the track doesn't have.
    #[test]
    fn controls_work_without_a_lane() {
        use crate::engine::StubEngine;
        let mut s = AppState::with_engine(Box::new(StubEngine::default()));
        let mut t = Track::new(TrackKind::Audio, "A");
        let d = eq_device();
        // A slider's parameter; a switch would round the position.
        let param = BuiltinParams
            .automatable(&d)
            .into_iter()
            .find(|&p| BuiltinParams.param(&d, p).is_some_and(|p| p.records()))
            .unwrap();
        let eq = AutomationTarget::DeviceParam {
            device: d.id,
            param,
        };
        t.devices.push(d);
        s.doc
            .execute(daw_model::commands::AddTrack::new(0, t))
            .unwrap();
        assert!(s.doc.project().tracks[0].automation.is_empty());

        for target in [AutomationTarget::Volume, eq] {
            let before = s.target_control(0, target).expect("a control");
            assert!(!before.following && !before.writes);
            let to = if before.norm < 0.5 { 0.75 } else { 0.25 };
            s.begin_control(0, target);
            s.set_control(0, target, to);
            s.end_control(0, target);
            let after = s.target_control(0, target).unwrap();
            assert!((after.norm - to).abs() < 1e-6, "{target:?}");
            // Moving a control makes no lane.
            assert!(s.doc.project().tracks[0].automation.is_empty());
        }
        assert_eq!(s.track_rows(0).len(), 1);
        assert_eq!(s.row_lane(0, 0), None);

        // Not on the track: no control, and nothing moves.
        let gone = AutomationTarget::DeviceParam {
            device: Device::new(
                "x",
                DeviceSource::Builtin {
                    kind: "daw.eq".into(),
                },
            )
            .id,
            param,
        };
        assert!(s.target_control(0, gone).is_none());
        assert!(s.target_control(1, AutomationTarget::Volume).is_none());
        let revision = s.doc.revision();
        s.set_control(0, gone, 0.5);
        assert_eq!(s.doc.revision(), revision);
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
