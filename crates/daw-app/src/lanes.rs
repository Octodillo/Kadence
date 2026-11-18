//! Automation lanes as the track headers show them, without Qt.
//!
//! The arrange view (`daw_arrange`) draws and edits the lanes' points; this
//! names lanes and their targets for the headers, adds and removes lanes and
//! keeps each lane's mode: off (the control keeps its own value), read (the
//! lane plays) or write (it plays, and moving its control during playback
//! records over it, as with the mixer's Write Automation switch). The
//! control is the same wherever it is moved: on the Mix page, in the device
//! panel or in the lane's header, which goes through the same mixer.
//!
//! [`LaneParams`] is what the lanes know about device parameters (their
//! names, ranges and tapers), for built-in devices and loaded plugins
//! alike, so a lane's row, its header control and the device panel's
//! slider all move along one scale.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Instant;

use daw_arrange::automation::{lane_scale, static_value};
use daw_arrange::{LaneScale, ParamCatalog};
use daw_core::event::ParamId;
use daw_model::{
    AutomationLane, AutomationTarget, Device, DeviceId, DeviceSource, LaneId, Project, Track,
};

use crate::device_panel::{BuiltinParams, PLUGIN_REREAD, ParamDesc};
use crate::message::Message;
use crate::mixer::{Control, StripId};
use crate::session_mixer::{ParamStart, strip_id};
use crate::state::AppState;

/// What a lane automates, as the header words it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LaneKind {
    Volume,
    Pan,
    Send,
    DeviceParam,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LaneMode {
    Off,
    Read,
    Write,
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

/// The device parameters the automation lanes know: every built-in
/// device's, and each loaded plugin's as it described them when it loaded
/// ([`LaneParams::set_plugins`]). The timeline and the piano roll ask it
/// for a lane's scale ([`ParamCatalog`]), and the headers for its name and
/// unit.
///
/// A plugin that is not loaded (missing, or no audio engine) is unknown
/// here, and so is a parameter whose range a lane can't be drawn along
/// (not finite, or empty). Their lanes show the parameter's number on a
/// plain 0 to 1 scale, and their header control is locked
/// ([`LaneControl::editable`]): a value set along a guessed scale would be
/// wrong once the plugin is back.
#[derive(Debug, Default)]
pub struct LaneParams {
    builtin: BuiltinParams,
    /// Behind a lock only because the views hold the catalog as a shared
    /// `Send + Sync` object; it is written on the thread that reads it.
    plugins: RwLock<HashMap<DeviceId, PluginLanes>>,
}

/// One loaded plugin, as the lanes know it.
#[derive(Debug, Default)]
struct PluginLanes {
    /// How many parameters the plugin described, and those among them a
    /// lane can show.
    described: usize,
    params: HashMap<ParamId, ParamDesc>,
    /// The plugin's own values for parameters the document doesn't store,
    /// where they were read ([`LaneParams::set_value`]).
    values: HashMap<ParamId, f64>,
}

impl PluginLanes {
    fn new(infos: &[daw_core::plugin::ParamInfo]) -> Self {
        Self {
            described: infos.len(),
            params: infos
                .iter()
                .map(ParamDesc::from_info)
                .filter(ParamDesc::has_range)
                .map(|p| (p.id, p))
                .collect(),
            values: HashMap::new(),
        }
    }

    /// Whether this is still what `infos` describes. A plugin's parameters
    /// stay as they are while it is loaded (and there may be thousands), so
    /// they are described again only when they differ.
    fn describes(&self, infos: &[daw_core::plugin::ParamInfo]) -> bool {
        self.described == infos.len()
            && infos.iter().all(|i| match self.params.get(&i.id) {
                Some(p) => p.describes(i),
                // Left out for its range: still left out?
                None => !(i.min.is_finite() && i.max.is_finite() && i.min != i.max),
            })
    }
}

impl LaneParams {
    /// Runs `f` on one parameter of a loaded plugin.
    fn with_plugin<R>(
        &self,
        device: &Device,
        param: ParamId,
        f: impl FnOnce(&ParamDesc, Option<f64>) -> R,
    ) -> Option<R> {
        if matches!(device.source, DeviceSource::Builtin { .. }) {
            return None;
        }
        let plugins = self.plugins.read().unwrap_or_else(PoisonError::into_inner);
        let plugin = plugins.get(&device.id)?;
        Some(f(
            plugin.params.get(&param)?,
            plugin.values.get(&param).copied(),
        ))
    }

    /// One parameter of `device`, if it is known.
    pub fn param(&self, device: &Device, param: ParamId) -> Option<ParamDesc> {
        self.builtin
            .param(device, param)
            .or_else(|| self.with_plugin(device, param, |p, _| p.clone()))
    }

    /// The name of a parameter, for headers.
    pub fn name(&self, device: &Device, param: ParamId) -> Option<String> {
        match self.builtin.name(device, param) {
            Some(name) => Some(name.to_owned()),
            None => self.with_plugin(device, param, |p, _| p.name.clone()),
        }
    }

    /// Takes the parameters of the loaded plugins among `devices`:
    /// `describe` gives a loaded plugin's, as the session does. Plugins no
    /// longer among them are forgotten, with their values.
    pub fn set_plugins<'a>(
        &self,
        devices: impl Iterator<Item = &'a Device>,
        describe: impl Fn(DeviceId) -> Option<&'a [daw_core::plugin::ParamInfo]>,
    ) {
        let mut plugins = self.plugins.write().unwrap_or_else(PoisonError::into_inner);
        let mut known = std::mem::take(&mut *plugins);
        for device in devices {
            let Some(infos) = describe(device.id) else {
                continue;
            };
            let plugin = known
                .remove(&device.id)
                .filter(|p| p.describes(infos))
                .unwrap_or_else(|| PluginLanes::new(infos));
            plugins.insert(device.id, plugin);
        }
    }

    /// Notes the value a loaded plugin itself holds for a parameter the
    /// document doesn't store: what its lane rests at.
    pub fn set_value(&self, device: DeviceId, param: ParamId, value: f64) {
        let mut plugins = self.plugins.write().unwrap_or_else(PoisonError::into_inner);
        if let Some(p) = plugins.get_mut(&device)
            && value.is_finite()
        {
            p.values.insert(param, value);
        }
    }
}

impl ParamCatalog for LaneParams {
    /// The built-in devices' only: a plugin may have thousands of
    /// parameters, too many to offer in a menu. A plugin parameter gets
    /// its lane when it is moved with Write Automation on.
    fn automatable(&self, device: &Device) -> Vec<ParamId> {
        self.builtin.automatable(device)
    }

    fn scale(&self, device: &Device, param: ParamId) -> Option<LaneScale> {
        self.builtin
            .scale(device, param)
            .or_else(|| self.with_plugin(device, param, |p, _| p.scale()))
    }

    /// A plugin's own value where it was read, else the default.
    fn default_value(&self, device: &Device, param: ParamId) -> Option<f64> {
        self.builtin
            .default_value(device, param)
            .or_else(|| self.with_plugin(device, param, |p, value| value.unwrap_or(p.default)))
    }
}

/// Names `target` on `track`.
pub fn target_name(
    project: &Project,
    params: &LaneParams,
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
                .unwrap_or_else(|| param.to_string());
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
    /// The control can be moved from the header: not a parameter nothing
    /// describes (see [`LaneParams`]).
    pub editable: bool,
}

/// The lane modes and parameter names the app keeps beside the document.
#[derive(Default)]
pub struct Lanes {
    /// Lanes in write mode. Not saved with the project: writing is armed
    /// for this session only, like a record button.
    pub write: HashSet<LaneId>,
    pub params: Arc<LaneParams>,
    /// When the plugins' own values were last read for the lanes.
    values_read: Option<Instant>,
}

impl AppState {
    /// Brings what the lanes know of plugin parameters up to date with the
    /// session: which plugins are loaded and, for parameters a lane
    /// automates and the document doesn't store, the value the plugin
    /// holds (what the device panel shows for it). Each value is a round
    /// trip to the plugin, so they are read at most every
    /// [`PLUGIN_REREAD`], as for the panel.
    pub(crate) fn refresh_lane_params(&mut self) {
        let session = self.engine.session();
        let project = self.doc.project();
        self.lanes.params.set_plugins(
            project.all_tracks().flat_map(|t| t.devices.iter()),
            |device| session?.plugin_params(device),
        );
        if self
            .lanes
            .values_read
            .is_some_and(|at| at.elapsed() < PLUGIN_REREAD)
        {
            return;
        }
        self.lanes.values_read = Some(Instant::now());
        for t in project.all_tracks() {
            for l in &t.automation {
                let AutomationTarget::DeviceParam { device, param } = l.target else {
                    continue;
                };
                let unstored = t.devices.iter().any(|d| {
                    d.id == device
                        && matches!(d.source, DeviceSource::Plugin { .. })
                        && !d.params.contains_key(&param)
                });
                if unstored {
                    self.read_plugin_value(device, param);
                }
            }
        }
    }

    /// Notes the value a loaded plugin holds for `param` now.
    fn read_plugin_value(&self, device: DeviceId, param: ParamId) {
        let value = self
            .engine
            .session()
            .and_then(|s| s.with_plugin(device, |p| p.param_value(param)));
        if let Some(value) = value {
            self.lanes.params.set_value(device, param, value);
        }
    }

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

    /// The value lane `lane` of track `track` controls, as it is now.
    pub fn lane_control(&self, track: i32, lane: i32) -> Option<LaneControl> {
        let (t, l) = self.lane_at(track, lane)?;
        let scale = lane_scale(self.arrange.params.as_ref(), t, l.target);
        let following = l.enabled && !l.points.is_empty();
        let value = if following {
            l.value_at(self.playhead())
        } else {
            None
        }
        .unwrap_or_else(|| static_value(self.arrange.params.as_ref(), t, l.target));
        Some(LaneControl {
            norm: scale.to_norm(value),
            value,
            following,
            writes: self.mixer.write() || (l.enabled && self.lanes.write.contains(&l.id)),
            editable: self.lane_mixer_control(track, lane).is_some(),
        })
    }

    /// The mixer control a lane's header drives: its strip and control,
    /// and for a device parameter, its description. `None` for a parameter
    /// nothing describes: there is no scale to move it along.
    fn lane_mixer_control(
        &self,
        track: i32,
        lane: i32,
    ) -> Option<(StripId, Control, Option<ParamDesc>)> {
        let (t, l) = self.lane_at(track, lane)?;
        let control = match l.target {
            AutomationTarget::Volume => Control::Volume,
            AutomationTarget::Pan => Control::Pan,
            AutomationTarget::SendLevel { send } => {
                let send = t.sends.iter().find(|s| s.id == send)?;
                Control::Send(strip_id(send.target))
            }
            AutomationTarget::DeviceParam { device, param } => Control::Param { device, param },
        };
        let param = self.lane_param(track, lane);
        if matches!(control, Control::Param { .. }) && param.is_none() {
            return None;
        }
        Some((strip_id(t.id), control, param))
    }

    /// Runs a change to a lane's control as the mixer runs its own: after
    /// pending edits elsewhere, with the lanes' write modes, then plays it.
    fn control_lane(
        &mut self,
        track: i32,
        lane: i32,
        f: impl FnOnce(&mut Self, StripId, Control, Option<ParamDesc>) -> Result<(), Message>,
    ) {
        let Some((id, control, param)) = self.lane_mixer_control(track, lane) else {
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

    /// The user grabbed a lane's control: a new undo step, and during
    /// playback with writing armed, a take, as grabbing the same control on
    /// the Mix page or device panel would start.
    pub fn begin_lane_control(&mut self, track: i32, lane: i32) {
        // A take starts from what the plugin holds now.
        if let Some((_, l)) = self.lane_at(track, lane)
            && let AutomationTarget::DeviceParam { device, param } = l.target
        {
            self.read_plugin_value(device, param);
        }
        let static_value = self
            .lane_at(track, lane)
            .map(|(t, l)| static_value(self.arrange.params.as_ref(), t, l.target));
        self.control_lane(track, lane, |s, id, control, param| {
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

    /// Moves a lane's control to height `norm` on the lane's scale. Like
    /// the mixer's controls, it records while a take runs and otherwise
    /// sets the static value; a lane playing its points ignores it unless
    /// it writes. Moves until
    /// [`end_lane_control`](Self::end_lane_control) are one undo step.
    pub fn set_lane_control(&mut self, track: i32, lane: i32, norm: f64) {
        if !norm.is_finite() {
            return;
        }
        let Some(c) = self.lane_control(track, lane) else {
            return;
        };
        if c.following && !c.writes {
            return;
        }
        let Some(scale) = self
            .lane_at(track, lane)
            .map(|(t, l)| lane_scale(self.arrange.params.as_ref(), t, l.target))
        else {
            return;
        };
        self.control_lane(track, lane, |s, id, control, param| {
            let value = param.map_or_else(|| scale.from_norm(norm), |p| p.from_normalized(norm));
            let session = s.engine.session_mut();
            s.mixer.set(&mut s.doc, session, id, control, value)
        });
    }

    /// The user let go of a lane's control: ends its take and the undo
    /// step.
    pub fn end_lane_control(&mut self, track: i32, lane: i32) {
        self.control_lane(track, lane, |s, id, control, _| {
            let session = s.engine.session_mut();
            s.mixer.touch(&mut s.doc, session, id, control, false)
        });
    }

    /// How a device-parameter lane's parameter is described (its unit,
    /// control and choices); `None` for other lanes and unknown devices.
    pub fn lane_param(&self, track: i32, lane: i32) -> Option<ParamDesc> {
        let (t, l) = self.lane_at(track, lane)?;
        let AutomationTarget::DeviceParam { device, param } = l.target else {
            return None;
        };
        let d = t.devices.iter().find(|d| d.id == device)?;
        self.lanes.params.param(d, param)
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
    use daw_model::commands::AddDevice;
    use daw_model::{Document, TrackKind};

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

    /// A plugin's parameters have the plugin's own ranges: a lane for one
    /// is drawn and edited along that range, not from 0 to 1.
    #[test]
    fn a_loaded_plugins_params_have_their_own_ranges_and_names() {
        use daw_core::plugin::ParamInfo;
        let plugin = Device::new(
            "Filter",
            DeviceSource::Plugin {
                format: daw_core::plugin::PluginFormatKind::Clap,
                id: "com.example.filter".into(),
            },
        );
        let cutoff = ParamInfo {
            id: 7,
            name: "Cutoff".into(),
            min: 20.0,
            max: 20_000.0,
            default: 1_000.0,
            automatable: true,
        };
        let infos = [cutoff];
        let p = LaneParams::default();
        assert_eq!(p.scale(&plugin, 7), None, "unknown until it loads");

        let devices = [plugin.clone(), eq_device()];
        let loaded = |id| (id == plugin.id).then_some(&infos[..]);
        p.set_plugins(devices.iter(), loaded);
        let scale = p.scale(&plugin, 7).expect("a range");
        assert_eq!(scale.from_norm(0.0), 20.0);
        assert_eq!(scale.from_norm(1.0), 20_000.0);
        assert_eq!(p.name(&plugin, 7).as_deref(), Some("Cutoff"));
        assert_eq!(p.default_value(&plugin, 7), Some(1_000.0));
        // What the plugin itself holds, once read, is where a lane rests;
        // it survives the next look at the session.
        p.set_value(plugin.id, 7, 5_000.0);
        p.set_value(plugin.id, 7, f64::NAN);
        p.set_plugins(devices.iter(), loaded);
        assert_eq!(p.default_value(&plugin, 7), Some(5_000.0));
        assert_eq!(p.scale(&plugin, 8), None, "not one of its parameters");
        // Too many to offer in the Add Automation Lane menu.
        assert!(p.automatable(&plugin).is_empty());
        // Built-in devices are known as before.
        assert!(!p.automatable(&devices[1]).is_empty());

        // The lane's scale as the timeline asks for it.
        let mut track = Track::new(TrackKind::Audio, "A");
        track.devices.push(plugin.clone());
        let catalog: Arc<dyn ParamCatalog> = Arc::new(p);
        let target = AutomationTarget::DeviceParam {
            device: plugin.id,
            param: 7,
        };
        assert_eq!(lane_scale(Some(&catalog), &track, target), scale);
        assert_eq!(static_value(Some(&catalog), &track, target), 5_000.0);
    }

    #[test]
    fn an_unloaded_plugin_is_forgotten() {
        use daw_core::plugin::ParamInfo;
        let plugin = Device::new(
            "Filter",
            DeviceSource::Plugin {
                format: daw_core::plugin::PluginFormatKind::Clap,
                id: "com.example.filter".into(),
            },
        );
        let infos = [ParamInfo {
            id: 1,
            name: "Gain".into(),
            min: -24.0,
            max: 24.0,
            default: 0.0,
            automatable: true,
        }];
        let p = LaneParams::default();
        p.set_plugins(std::iter::once(&plugin), |_| Some(&infos[..]));
        assert!(p.scale(&plugin, 1).is_some());
        // Removed from the project, or its plugin is gone.
        p.set_plugins(std::iter::once(&plugin), |_| None);
        assert_eq!(p.scale(&plugin, 1), None);
    }

    /// A plugin may describe any range: one given high end first is turned
    /// round, and one no value can be placed along is not known at all.
    #[test]
    fn plugin_ranges_are_ordered_and_unusable_ones_left_out() {
        use daw_core::plugin::ParamInfo;
        let plugin = Device::new(
            "Odd",
            DeviceSource::Plugin {
                format: daw_core::plugin::PluginFormatKind::Clap,
                id: "com.example.odd".into(),
            },
        );
        let info = |id, min, max| ParamInfo {
            id,
            name: format!("P{id}"),
            min,
            max,
            default: 0.0,
            automatable: true,
        };
        let infos = [
            info(1, 1.0, -1.0),
            info(2, 0.5, 0.5),
            info(3, 0.0, f64::INFINITY),
            info(4, f64::NAN, 1.0),
        ];
        let p = LaneParams::default();
        p.set_plugins(std::iter::once(&plugin), |_| Some(&infos[..]));
        let scale = p.scale(&plugin, 1).expect("turned round");
        assert_eq!((scale.from_norm(0.0), scale.from_norm(1.0)), (-1.0, 1.0));
        for id in 2..=4 {
            assert_eq!(p.scale(&plugin, id), None, "{id}");
            assert_eq!(p.param(&plugin, id), None, "{id}");
        }
        // The same description again keeps what is known.
        p.set_value(plugin.id, 1, 0.25);
        p.set_plugins(std::iter::once(&plugin), |_| Some(&infos[..]));
        assert_eq!(p.default_value(&plugin, 1), Some(0.25));
        // A different one (the plugin was swapped or loaded anew) does not.
        let other = [info(1, 0.0, 10.0)];
        p.set_plugins(std::iter::once(&plugin), |_| Some(&other[..]));
        assert_eq!(p.default_value(&plugin, 1), Some(0.0));
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
        let p = LaneParams::default();
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
