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
//!
//! [`LaneParams`] is what the lanes know about device parameters (their
//! names, ranges and tapers), for built-in devices and loaded plugins
//! alike, so a lane's row, its header control and the device panel's
//! slider all move along one scale.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Instant;

use daw_arrange::automation::{lane_scale, static_value};
use daw_arrange::rows::{Group, LaneRow};
use daw_arrange::{LaneScale, ParamCatalog};
use daw_core::event::ParamId;
use daw_model::{AutomationTarget, Device, DeviceId, DeviceSource, LaneId, Project, Track};

use crate::device_panel::{BuiltinParams, PLUGIN_REREAD, ParamDesc};
use crate::message::Message;
use crate::mixer::{Control, StripId};
use crate::session_mixer::{ParamStart, strip_id};
use crate::state::AppState;

/// What a row under a track is, as its header words it: a control (which
/// is also what a lane automates) or the heading of a group of them.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LaneKind {
    Volume,
    Pan,
    Send,
    DeviceParam,
    Instrument,
    Effects,
    Sends,
    Device,
    More,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LaneMode {
    Off,
    Read,
    Write,
}

/// A row's or target's name in parts: the kind, the send's target track
/// or the device, and the parameter.
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
/// plain 0 to 1 scale, read-only on the timeline
/// ([`LaneFrame::editable`](daw_arrange::LaneFrame::editable)), and their
/// header control is locked ([`LaneControl::editable`]): a value set along
/// a guessed scale would be wrong once the plugin is back.
#[derive(Debug, Default)]
pub struct LaneParams {
    builtin: BuiltinParams,
    /// Behind a lock only because the views hold the catalog as a shared
    /// `Send + Sync` object; it is written on the thread that reads it.
    plugins: RwLock<HashMap<DeviceId, PluginLanes>>,
    /// Counts the changes to `plugins` (see [`ParamCatalog::revision`]).
    revision: AtomicU64,
}

/// One loaded plugin, as the lanes know it.
#[derive(Debug, Default)]
struct PluginLanes {
    /// How many parameters the plugin described, and those among them a
    /// lane can show.
    described: usize,
    params: HashMap<ParamId, ParamDesc>,
    /// Those the plugin lets a host automate, in its own order.
    automatable: Vec<ParamId>,
    /// The plugin's own values for parameters the document doesn't store,
    /// where they were read ([`LaneParams::set_value`]).
    values: HashMap<ParamId, f64>,
}

impl PluginLanes {
    fn new(infos: &[daw_core::plugin::ParamInfo]) -> Self {
        let params: HashMap<ParamId, ParamDesc> = infos
            .iter()
            .map(ParamDesc::from_info)
            .filter(ParamDesc::has_range)
            .map(|p| (p.id, p))
            .collect();
        let automatable = Self::automatable_of(infos, &params);
        Self {
            described: infos.len(),
            params,
            automatable,
            values: HashMap::new(),
        }
    }

    /// The parameters of `infos` a host may automate and a lane can show
    /// (those in `params`), in the plugin's order, each once.
    fn automatable_of(
        infos: &[daw_core::plugin::ParamInfo],
        params: &HashMap<ParamId, ParamDesc>,
    ) -> Vec<ParamId> {
        let mut seen = HashSet::new();
        infos
            .iter()
            .filter(|i| i.automatable && params.contains_key(&i.id) && seen.insert(i.id))
            .map(|i| i.id)
            .collect()
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
            // In the same order: the rows list them so.
            && self.automatable == Self::automatable_of(infos, &self.params)
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
        let mut changed = false;
        for device in devices {
            let Some(infos) = describe(device.id) else {
                continue;
            };
            let plugin = match known.remove(&device.id).filter(|p| p.describes(infos)) {
                Some(p) => p,
                None => {
                    changed = true;
                    PluginLanes::new(infos)
                }
            };
            plugins.insert(device.id, plugin);
        }
        if changed || !known.is_empty() {
            self.changed();
        }
    }

    /// Says that what `automatable` answers may have changed for some
    /// device, so rows made from it are made again.
    pub fn changed(&self) {
        self.revision.fetch_add(1, Ordering::Relaxed);
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
    /// A built-in device's, or those a loaded plugin lets a host automate
    /// (and that have a range), in the plugin's order. An unloaded
    /// plugin's are unknown: none.
    fn automatable(&self, device: &Device) -> Vec<ParamId> {
        if matches!(device.source, DeviceSource::Builtin { .. }) {
            return self.builtin.automatable(device);
        }
        let plugins = self.plugins.read().unwrap_or_else(PoisonError::into_inner);
        plugins
            .get(&device.id)
            .map_or_else(Vec::new, |p| p.automatable.clone())
    }

    fn revision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }

    fn is_instrument(&self, device: &Device) -> bool {
        self.builtin.is_instrument(device)
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

/// Names the group `group` of `track`.
pub fn group_name(track: &Track, group: Group) -> TargetName {
    let (kind, detail) = match group {
        Group::Instrument => (LaneKind::Instrument, String::new()),
        Group::Effects => (LaneKind::Effects, String::new()),
        Group::Sends => (LaneKind::Sends, String::new()),
        Group::More(_) => (LaneKind::More, String::new()),
        Group::Device(id) => {
            let name = track.devices.iter().find(|d| d.id == id);
            (
                LaneKind::Device,
                name.map(|d| d.name.clone()).unwrap_or_default(),
            )
        }
    };
    TargetName {
        kind,
        detail,
        param: String::new(),
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

    // -----------------------------------------------------------------------
    // Rows under a track, by index (see `daw_arrange::rows`)

    /// The rows track `track` shows under it when expanded, as the
    /// timeline lays them out.
    pub fn rows_under(&self, track: i32) -> Vec<LaneRow> {
        self.track_at_index(track)
            .map_or_else(Vec::new, |t| self.arrange.rows_of(t))
    }

    fn row_at(&self, track: i32, row: i32) -> Option<LaneRow> {
        let row = usize::try_from(row).ok()?;
        self.rows_under(track).get(row).copied()
    }

    /// The control row `row` of the track is for; `None` for a group.
    pub fn row_target(&self, track: i32, row: i32) -> Option<AutomationTarget> {
        self.row_at(track, row)?.target()
    }

    /// The index among the track's lanes of the lane in row `row`.
    pub fn row_lane(&self, track: i32, row: i32) -> Option<usize> {
        let t = self.track_at_index(track)?;
        let id = self.row_at(track, row)?.lane?;
        t.automation.iter().position(|l| l.id == id)
    }

    /// Opens or closes the group row `row` of the track is the heading of.
    pub fn set_row_open(&mut self, track: i32, row: i32, on: bool) {
        let Some(id) = self.track_at_index(track).map(|t| t.id) else {
            return;
        };
        let Some(group) = self.row_at(track, row).and_then(|r| r.group()) else {
            return;
        };
        let project = self.doc.project().clone();
        let out = self.arrange.set_group_open(&project, id, group, on);
        self.apply(out);
    }

    /// Sets the mode of row `row`'s lane. Arming a control that has no
    /// lane for writing gives it one, to record into.
    pub fn set_row_mode(&mut self, track: i32, row: i32, mode: LaneMode) {
        let Some(target) = self.row_target(track, row) else {
            return;
        };
        let Some(t) = self.track_at_index(track).map(|t| t.id) else {
            return;
        };
        if self.row_lane(track, row).is_none() && mode == LaneMode::Write {
            let out = self.arrange.add_lane(&mut self.doc, t, target);
            self.apply(out);
        }
        // By target: the row stays where it is, but its lane is new.
        let lane = self
            .track_at_index(track)
            .and_then(|t| t.automation.iter().position(|l| l.target == target));
        if let Some(l) = lane {
            self.set_lane_mode(track, l as i32, mode);
        }
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
            editable: self.mixer_control(track, target).is_some(),
        })
    }

    /// The mixer control behind `target`: its strip and control, and for a
    /// device parameter, its description. `None` for a parameter nothing
    /// describes: there is no scale to move it along.
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
        let param = self.target_param(track, target);
        if matches!(control, Control::Param { .. }) && param.is_none() {
            return None;
        }
        Some((strip_id(t.id), control, param))
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
        // A take starts from what the plugin holds now.
        if let AutomationTarget::DeviceParam { device, param } = target {
            self.read_plugin_value(device, param);
        }
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
        // One a host may not automate: known, but not listed as a row.
        let meter = ParamInfo {
            id: 8,
            name: "Meter".into(),
            automatable: false,
            ..cutoff.clone()
        };
        let infos = [meter, cutoff];
        let p = LaneParams::default();
        assert_eq!(p.scale(&plugin, 7), None, "unknown until it loads");
        assert!(p.automatable(&plugin).is_empty());

        let devices = [plugin.clone(), eq_device()];
        let loaded = |id| (id == plugin.id).then_some(&infos[..]);
        let before = p.revision();
        p.set_plugins(devices.iter(), loaded);
        assert_ne!(
            p.revision(),
            before,
            "rows made before it loaded are made again"
        );
        assert_eq!(p.automatable(&plugin), [7]);
        let loaded_at = p.revision();
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
        assert_eq!(p.revision(), loaded_at, "the same plugin, the same rows");
        assert_eq!(p.default_value(&plugin, 7), Some(5_000.0));
        assert_eq!(p.scale(&plugin, 9), None, "not one of its parameters");
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
        assert_eq!(p.automatable(&plugin), [1]);
        let loaded = p.revision();
        // Removed from the project, or its plugin is gone.
        p.set_plugins(std::iter::once(&plugin), |_| None);
        assert_eq!(p.scale(&plugin, 1), None);
        assert!(p.automatable(&plugin).is_empty());
        let forgotten = p.revision();
        assert_ne!(forgotten, loaded, "its rows go");
        // Looking again with it still gone changes nothing.
        p.set_plugins(std::iter::once(&plugin), |_| None);
        assert_eq!(p.revision(), forgotten);
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
        // A parameter named twice is one row; the same ones in another
        // order are listed in that order.
        let twice = [info(5, 0.0, 1.0), info(6, 0.0, 1.0), info(5, 0.0, 1.0)];
        p.set_plugins(std::iter::once(&plugin), |_| Some(&twice[..]));
        assert_eq!(p.automatable(&plugin), [5, 6]);
        let turned = [info(6, 0.0, 1.0), info(5, 0.0, 1.0), info(5, 0.0, 1.0)];
        p.set_plugins(std::iter::once(&plugin), |_| Some(&turned[..]));
        assert_eq!(p.automatable(&plugin), [6, 5]);
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
        let last = AutomationTarget::DeviceParam {
            device: track.devices[0].id,
            param: *p.automatable(&track.devices[0]).last().unwrap(),
        };
        let n = target_name(doc.project(), &p, &track, last);
        assert_eq!(n.kind, LaneKind::DeviceParam);
        assert_eq!(n.detail, "EQ");
        assert!(!n.param.is_empty());
        let v = target_name(doc.project(), &p, &track, AutomationTarget::Volume);
        assert_eq!(v.kind, LaneKind::Volume);
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

    /// The headers' view of the rows: names, folds, and a mode that makes
    /// the lane it needs.
    #[test]
    fn rows_are_named_fold_and_arm() {
        use crate::engine::StubEngine;
        let mut s = AppState::with_engine(Box::new(StubEngine::default()));
        let mut t = Track::new(TrackKind::Audio, "A");
        t.devices.push(eq_device());
        s.doc
            .execute(daw_model::commands::AddTrack::new(0, t))
            .unwrap();
        let kinds = |s: &AppState| -> Vec<LaneKind> {
            s.header_rows(0).iter().map(|r| r.name.kind).collect()
        };
        use LaneKind::{Device, DeviceParam, Effects, Pan, Sends, Volume};
        assert_eq!(kinds(&s), [Volume, Pan, Effects, Sends]);
        let effects = &s.header_rows(0)[2];
        assert_eq!(
            (effects.depth, effects.open, effects.lane),
            (0, false, None)
        );

        // Opening Effects lists the EQ; opening the EQ lists its parameters
        // by their own names.
        s.set_row_open(0, 2, true);
        assert_eq!(kinds(&s), [Volume, Pan, Effects, Device, Sends]);
        assert_eq!(s.header_rows(0)[3].name.detail, "EQ");
        s.set_row_open(0, 3, true);
        let rows = s.header_rows(0);
        assert_eq!((rows[3].depth, rows[3].open), (1, true));
        let first = rows[4].clone();
        assert_eq!(first.name.kind, DeviceParam);
        assert!(first.name.detail.is_empty() && !first.name.param.is_empty());
        assert_eq!((first.depth, first.open), (2, false));
        assert!(first.param.is_some());
        // Opening a control does nothing.
        s.set_row_open(0, 4, true);
        assert_eq!(s.header_rows(0).len(), rows.len());

        // Arming a control without a lane gives it one; closing the groups
        // then leaves that control in view, named with its device.
        assert_eq!(first.mode, LaneMode::Read);
        s.set_row_mode(0, 4, LaneMode::Off);
        assert_eq!(s.header_rows(0)[4].lane, None);
        s.set_row_mode(0, 4, LaneMode::Write);
        let armed = &s.header_rows(0)[4];
        assert_eq!((armed.lane, armed.mode), (Some(0), LaneMode::Write));
        s.set_row_open(0, 2, false);
        assert_eq!(kinds(&s), [Volume, Pan, Effects, DeviceParam, Sends]);
        let kept = &s.header_rows(0)[3];
        assert_eq!(
            (kept.name.detail.as_str(), &kept.name.param),
            ("EQ", &first.name.param)
        );
        assert_eq!(kept.mode, LaneMode::Write);
    }
}
