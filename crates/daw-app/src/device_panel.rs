//! The device panel, without naming Qt: one device's parameters, bypass
//! and position in its chain, edited through model commands.
//!
//! What a device's parameters are comes from a [`ParamSource`]. Built-in
//! devices describe theirs with [`BuiltinParams`]; plugins add a source of
//! their own (their parameters are known once the plugin is loaded), and
//! the panel shows them the same way. [`BuiltinParams`] is also the
//! automation lanes' [`ParamCatalog`], so a built-in parameter has one
//! range, taper and unit wherever it shows, and [`ParamDesc::scale`] is the
//! one mapping between values and slider or lane positions.
//!
//! A parameter drag is one undo step: [`DevicePanel::set_value`] runs
//! [`SetParam`], which merges with the previous edit of the same parameter
//! until [`DevicePanel::end_gesture`] seals the history.

use daw_arrange::{LaneScale, ParamCatalog};
use daw_core::event::ParamId;
use daw_devices::{ParamKind, ParamSpec, ParamUnit, Taper};
use daw_model::commands::{DeviceField, MoveDevice, SetDevice, SetParam};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use daw_model::{AutomationTarget, Device, DeviceId, DeviceSource, Document, Project, TrackKind};
use daw_session::PluginStatus;

use crate::double_click::Clicks;
use crate::message::Message;
use crate::mixer::Control;

/// What kind of control a parameter gets.
#[derive(Clone, Debug, PartialEq)]
pub enum ParamControl {
    /// A slider over `min..=max`, logarithmic when `log`.
    Continuous { log: bool },
    /// Whole numbers: a spin box.
    Integer,
    /// Off (0) or on (1): a switch.
    Toggle,
    /// One of these labels, by index: a combo box.
    Choice(Vec<String>),
}

/// One parameter of a device, as the panel shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamDesc {
    pub id: ParamId,
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub control: ParamControl,
    pub unit: ParamUnit,
    /// An automation lane may play it.
    pub automatable: bool,
}

impl ParamDesc {
    /// The description of a built-in device's parameter.
    pub fn from_spec(spec: &ParamSpec) -> Self {
        Self {
            id: spec.id,
            name: spec.name.to_owned(),
            min: spec.min,
            max: spec.max,
            default: spec.default,
            control: match spec.kind {
                ParamKind::Continuous => ParamControl::Continuous {
                    log: spec.taper == Taper::Log && spec.min > 0.0,
                },
                ParamKind::Integer => ParamControl::Integer,
                ParamKind::Toggle => ParamControl::Toggle,
                ParamKind::Choice(labels) => {
                    ParamControl::Choice(labels.iter().map(|l| (*l).to_owned()).collect())
                }
            },
            unit: spec.unit,
            automatable: spec.automatable,
        }
    }

    /// A plugin parameter: a plain linear slider, as `ParamInfo` says no
    /// more. A plugin source may refine the control and unit.
    pub fn from_info(info: &daw_core::plugin::ParamInfo) -> Self {
        Self {
            id: info.id,
            name: info.name.clone(),
            min: info.min,
            max: info.max,
            default: info.default,
            control: ParamControl::Continuous { log: false },
            unit: ParamUnit::None,
            automatable: info.automatable,
        }
    }

    fn is_discrete(&self) -> bool {
        !matches!(self.control, ParamControl::Continuous { .. })
    }

    /// Moving its control during playback can record a take: it is
    /// automatable and continuous (a step at a time would only click).
    pub fn records(&self) -> bool {
        self.automatable && !self.is_discrete()
    }

    /// Clamps to the range and rounds discrete parameters. NaN becomes the
    /// default.
    pub fn sanitize(&self, value: f64) -> f64 {
        if value.is_nan() {
            return self.default;
        }
        let v = value.clamp(self.min.min(self.max), self.max.max(self.min));
        if self.is_discrete() { v.round() } else { v }
    }

    /// How values map to a slider or an automation lane's height.
    pub fn scale(&self) -> LaneScale {
        LaneScale::Range {
            min: self.min,
            max: self.max,
            log: matches!(self.control, ParamControl::Continuous { log: true }),
            steps: self.is_discrete(),
        }
    }

    /// A plain value as a slider position from 0 to 1.
    pub fn to_normalized(&self, value: f64) -> f64 {
        self.scale().to_norm(self.sanitize(value))
    }

    /// How far a recorded take of the parameter may stray from the hand:
    /// a thousandth of a slider's travel (at the bottom of a log taper,
    /// where its steps are finest).
    pub fn tolerance(&self) -> f64 {
        let t = if self.is_discrete() {
            // A whole step would round the thousandth away.
            (self.max - self.min).abs() * 1e-3
        } else {
            (self.scale().from_norm(1e-3) - self.min).abs()
        };
        if t.is_finite() && t > 0.0 { t } else { 1e-6 }
    }

    /// A slider position from 0 to 1 as a plain value.
    pub fn from_normalized(&self, position: f64) -> f64 {
        if position.is_nan() {
            return self.default;
        }
        self.sanitize(self.scale().from_norm(position))
    }

    /// How QML names the unit; it words the value with it.
    pub fn unit_name(&self) -> &'static str {
        match self.unit {
            ParamUnit::None => "",
            ParamUnit::Decibels => "dB",
            ParamUnit::Hertz => "Hz",
            ParamUnit::Milliseconds => "ms",
            ParamUnit::Percent => "percent",
            ParamUnit::Semitones => "semitones",
            ParamUnit::Cents => "cents",
            ParamUnit::Octaves => "octaves",
            ParamUnit::Ratio => "ratio",
            ParamUnit::Pan => "pan",
        }
    }

    /// The labels of a choice parameter; empty for other kinds.
    pub fn choices(&self) -> &[String] {
        match &self.control {
            ParamControl::Choice(labels) => labels,
            _ => &[],
        }
    }

    /// How QML names the kind of control: "continuous", "integer",
    /// "toggle" or "choice".
    pub fn control_name(&self) -> &'static str {
        match self.control {
            ParamControl::Continuous { .. } => "continuous",
            ParamControl::Integer => "integer",
            ParamControl::Toggle => "toggle",
            ParamControl::Choice(_) => "choice",
        }
    }
}

/// Describes the parameters of the devices it knows.
pub trait ParamSource {
    /// The parameters of `device` in display order, or `None` if this
    /// source doesn't know the device.
    fn describe(&self, device: &Device) -> Option<Vec<ParamDesc>>;

    /// The value the device itself holds for a parameter the document
    /// doesn't set (a plugin's own state), if this source knows it. The
    /// default is used otherwise.
    fn current(&self, _device: &Device, _param: ParamId) -> Option<f64> {
        None
    }
}

/// The built-in devices of `daw-devices`.
#[derive(Debug, Default)]
pub struct BuiltinParams;

impl BuiltinParams {
    fn specs(device: &Device) -> &'static [ParamSpec] {
        match &device.source {
            DeviceSource::Builtin { kind } => daw_devices::param_specs(kind).unwrap_or(&[]),
            // Plugins describe their parameters once they are loaded.
            _ => &[],
        }
    }

    fn spec(device: &Device, param: ParamId) -> Option<&'static ParamSpec> {
        Self::specs(device).iter().find(|s| s.id == param)
    }

    /// One parameter of a built-in device.
    pub fn param(&self, device: &Device, param: ParamId) -> Option<ParamDesc> {
        Self::spec(device, param).map(ParamDesc::from_spec)
    }

    /// The name of a parameter, for headers.
    pub fn name(&self, device: &Device, param: ParamId) -> Option<&'static str> {
        Self::spec(device, param).map(|s| s.name)
    }
}

impl ParamCatalog for BuiltinParams {
    fn automatable(&self, device: &Device) -> Vec<ParamId> {
        Self::specs(device)
            .iter()
            .filter(|s| s.automatable)
            .map(|s| s.id)
            .collect()
    }

    fn scale(&self, device: &Device, param: ParamId) -> Option<LaneScale> {
        self.param(device, param).map(|p| p.scale())
    }

    fn default_value(&self, device: &Device, param: ParamId) -> Option<f64> {
        Self::spec(device, param).map(|s| s.default)
    }
}

impl ParamSource for BuiltinParams {
    fn describe(&self, device: &Device) -> Option<Vec<ParamDesc>> {
        let DeviceSource::Builtin { kind } = &device.source else {
            return None;
        };
        let specs = daw_devices::param_specs(kind)?;
        Some(specs.iter().map(ParamDesc::from_spec).collect())
    }
}

/// A parameter and its value in the document.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamView {
    pub desc: ParamDesc,
    pub value: f64,
    /// An automation lane plays this parameter, so it overrides the
    /// value here during playback.
    pub automated: bool,
    /// Moving it during playback records into its lane (the lane is in
    /// write mode, or the mixer writes automation), so a lane doesn't
    /// lock it.
    pub writable: bool,
}

impl ParamView {
    pub fn is_default(&self) -> bool {
        self.value == self.desc.default
    }
}

/// The open device as the panel shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct DeviceView {
    pub id: DeviceId,
    pub name: String,
    /// The built-in device id (such as `daw.eq`) or plugin id.
    pub kind: String,
    pub track_name: String,
    /// The track is the master (which is unnamed by default).
    pub on_master: bool,
    pub enabled: bool,
    /// Position in the track's chain, and the chain's length.
    pub index: usize,
    pub chain_len: usize,
    /// The device is on an instrument track, whose first device plays the
    /// track's notes: the first device stays first.
    pub instrument_track: bool,
    /// Empty when no source describes the device (a plugin that isn't
    /// loaded).
    pub params: Vec<ParamView>,
}

impl DeviceView {
    /// Whether the device can move by `step` places (-1 earlier, 1 later).
    pub fn can_move(&self, step: isize) -> bool {
        // Instrument tracks feed their notes to the first device, so the
        // instrument doesn't move and nothing moves in front of it.
        let first = isize::from(self.instrument_track);
        let to = self.index as isize + step;
        step != 0 && !self.is_instrument() && to >= first && to < self.chain_len as isize
    }

    /// The device plays an instrument track's notes.
    pub fn is_instrument(&self) -> bool {
        self.instrument_track && self.index == 0
    }
}

/// The panel: which device is open, and edits to it. See the module docs.
pub struct DevicePanel {
    sources: Vec<Box<dyn ParamSource>>,
    device: Option<DeviceId>,
    /// The undo step the latest edit started: its parameter, when it
    /// started, and the document revision after its last edit. A reset
    /// right after it (the first click of a double-click moves a slider)
    /// takes it back first.
    click: Cell<Option<Clicks<ParamId>>>,
}

impl Default for DevicePanel {
    fn default() -> Self {
        Self::new(vec![Box::new(BuiltinParams)])
    }
}

impl DevicePanel {
    pub fn new(sources: Vec<Box<dyn ParamSource>>) -> Self {
        Self {
            sources,
            device: None,
            click: Cell::new(None),
        }
    }

    /// Adds a source, asked after the ones already there.
    pub fn add_source(&mut self, source: Box<dyn ParamSource>) {
        self.sources.push(source);
    }

    pub fn open(&mut self, device: DeviceId) {
        self.device = Some(device);
    }

    pub fn close(&mut self) {
        self.device = None;
    }

    pub fn device(&self) -> Option<DeviceId> {
        self.device
    }

    /// The open device, or `None` when none is open or it is gone (removed,
    /// or its addition undone).
    pub fn view(&self, project: &Project) -> Option<DeviceView> {
        let id = self.device?;
        let (track_id, index) = project.device_location(id)?;
        let track = project.track(track_id)?;
        let device = &track.devices[index];
        let (source, descs) = self
            .sources
            .iter()
            .find_map(|s| Some((s, s.describe(device)?)))
            .map_or((None, Vec::new()), |(s, d)| (Some(s), d));
        let params = descs
            .into_iter()
            .map(|desc| {
                let value = device
                    .params
                    .get(&desc.id)
                    .copied()
                    .or_else(|| source?.current(device, desc.id))
                    .map_or(desc.default, |v| desc.sanitize(v));
                let automated = track.automation.iter().any(|l| {
                    l.enabled
                        && !l.points.is_empty()
                        && l.target
                            == (AutomationTarget::DeviceParam {
                                device: id,
                                param: desc.id,
                            })
                });
                ParamView {
                    desc,
                    value,
                    automated,
                    writable: false,
                }
            })
            .collect();
        Some(DeviceView {
            id,
            name: device.name.clone(),
            kind: match &device.source {
                DeviceSource::Builtin { kind } => kind.clone(),
                DeviceSource::Plugin { id, .. } => id.clone(),
            },
            track_name: track.name.clone(),
            on_master: track.kind == TrackKind::Master,
            enabled: device.enabled,
            index,
            chain_len: track.devices.len(),
            instrument_track: track.kind == TrackKind::Instrument,
            params,
        })
    }

    pub(crate) fn param(&self, project: &Project, param: ParamId) -> Option<(DeviceId, ParamView)> {
        let view = self.view(project)?;
        let p = view.params.into_iter().find(|p| p.desc.id == param)?;
        Some((view.id, p))
    }

    /// Sets a parameter. Successive calls for the same parameter merge
    /// into one undo step until [`end_gesture`](Self::end_gesture).
    pub fn set_value(&self, doc: &mut Document, param: ParamId, value: f64) -> Result<(), Message> {
        let Some((device, p)) = self.param(doc.project(), param) else {
            return Ok(());
        };
        let value = p.desc.sanitize(value);
        if value == p.value {
            return Ok(());
        }
        let (before, undo_len) = (doc.revision(), doc.history().undo_len());
        doc.execute(SetParam {
            device,
            param,
            value: Some(value),
        })
        .map_err(Message::from)?;
        self.click.set(Clicks::after_edit(
            self.click.get(),
            param,
            before,
            undo_len,
            doc,
        ));
        Ok(())
    }

    /// Sets a parameter from a slider position (0 to 1, along its taper).
    pub fn set_normalized(
        &self,
        doc: &mut Document,
        param: ParamId,
        position: f64,
    ) -> Result<(), Message> {
        let Some((_, p)) = self.param(doc.project(), param) else {
            return Ok(());
        };
        self.set_value(doc, param, p.desc.from_normalized(position))
    }

    /// The user let go of a control: the next edit is a new undo step.
    pub fn end_gesture(&self, doc: &mut Document) {
        doc.seal();
    }

    /// Returns a parameter to the device's default, as an undo step of its
    /// own.
    pub fn reset(&self, doc: &mut Document, param: ParamId) -> Result<(), Message> {
        let Some(device) = self.device else {
            return Ok(());
        };
        Clicks::take_back(self.click.take(), param, doc).map_err(Message::from)?;
        let Some(d) = doc
            .project()
            .device_location(device)
            .and_then(|(t, i)| doc.project().track(t)?.devices.get(i))
        else {
            return Ok(());
        };
        if !d.params.contains_key(&param) {
            return Ok(());
        }
        // The session sends a built-in device the default of a parameter
        // the document no longer holds; a plugin's default is only known
        // to its source, so store it.
        let value = match &d.source {
            DeviceSource::Builtin { .. } => None,
            DeviceSource::Plugin { .. } => match self.param(doc.project(), param) {
                Some((_, p)) => Some(p.desc.default),
                None => return Ok(()),
            },
        };
        doc.seal();
        let r = doc.execute(SetParam {
            device,
            param,
            value,
        });
        doc.seal();
        r.map(|_| ()).map_err(Message::from)
    }

    /// Switches the device on, or off (bypassed: audio goes around it).
    pub fn set_enabled(&self, doc: &mut Document, on: bool) -> Result<(), Message> {
        let Some(view) = self.view(doc.project()) else {
            return Ok(());
        };
        if view.enabled == on {
            return Ok(());
        }
        doc.seal();
        let r = doc.execute(SetDevice {
            id: view.id,
            field: DeviceField::Enabled(on),
        });
        doc.seal();
        r.map(|_| ()).map_err(Message::from)
    }

    /// Moves the device `step` places along its chain (-1 earlier, 1
    /// later). Does nothing where it can't go.
    pub fn move_by(&self, doc: &mut Document, step: isize) -> Result<(), Message> {
        let Some(view) = self.view(doc.project()) else {
            return Ok(());
        };
        if !view.can_move(step) {
            return Ok(());
        }
        let to = (view.index as isize + step) as usize;
        doc.seal();
        let r = doc.execute(MoveDevice::new(view.id, to));
        doc.seal();
        r.map(|_| ()).map_err(Message::from)
    }
}

/// The parameters of loaded plugins, as the session describes them. The
/// app fills it for the open device ([`PluginParams::update`]), since the
/// session lives in the app state.
#[derive(Clone, Default)]
pub struct PluginParams(Rc<RefCell<Option<PluginSnapshot>>>);

struct PluginSnapshot {
    device: DeviceId,
    params: Vec<ParamDesc>,
    /// The plugin's own values when last read, empty if it couldn't be
    /// read (crashed, or too many parameters).
    values: HashMap<ParamId, f64>,
    /// When the values were read, and the document revision then.
    read_at: Instant,
    revision: u64,
}

/// How often a plugin's own values are read again while the document
/// changes (an undo, its editor's state stored) or the plugin is down.
const PLUGIN_REREAD: Duration = Duration::from_millis(500);

/// Plugins with more parameters than this don't have their own values
/// read: each read is a round trip to the sandbox, and the plugin plays
/// silence while it runs.
const MAX_PLUGIN_READS: usize = 256;

fn read_plugin_values(
    session: &daw_session::Session,
    device: DeviceId,
    params: &[daw_core::plugin::ParamInfo],
) -> HashMap<ParamId, f64> {
    if params.len() > MAX_PLUGIN_READS {
        return HashMap::new();
    }
    session
        .with_plugin(device, |p| {
            params.iter().map(|i| (i.id, p.param_value(i.id))).collect()
        })
        .unwrap_or_default()
}

impl PluginParams {
    /// Describes `device` from `session`, reading the plugin's own values
    /// again (at most every [`PLUGIN_REREAD`]) when the document has
    /// changed since `revision` or they couldn't be read. Returns true if
    /// what the panel shows changed.
    pub fn update(
        &self,
        session: Option<&daw_session::Session>,
        device: Option<DeviceId>,
        revision: u64,
    ) -> bool {
        let mut cache = self.0.borrow_mut();
        let found = device
            .zip(session)
            .and_then(|(d, s)| Some((d, s, s.plugin_params(d)?)));
        let Some((device, session, params)) = found else {
            return cache.take().is_some();
        };
        match &mut *cache {
            Some(c) if c.device == device => {
                let stale = c.revision != revision
                    || (c.values.is_empty() && params.len() <= MAX_PLUGIN_READS);
                if !stale || c.read_at.elapsed() < PLUGIN_REREAD {
                    return false;
                }
                let values = read_plugin_values(session, device, params);
                c.read_at = Instant::now();
                c.revision = revision;
                if values == c.values {
                    return false;
                }
                c.values = values;
                true
            }
            _ => {
                *cache = Some(PluginSnapshot {
                    device,
                    params: params.iter().map(ParamDesc::from_info).collect(),
                    values: read_plugin_values(session, device, params),
                    read_at: Instant::now(),
                    revision,
                });
                true
            }
        }
    }

    /// Forgets the snapshot, so the next update reads the plugin afresh.
    pub fn clear(&self) {
        self.0.borrow_mut().take();
    }
}

impl ParamSource for PluginParams {
    fn describe(&self, device: &Device) -> Option<Vec<ParamDesc>> {
        let cache = self.0.borrow();
        let c = cache.as_ref().filter(|c| c.device == device.id)?;
        Some(c.params.clone())
    }

    fn current(&self, device: &Device, param: ParamId) -> Option<f64> {
        let cache = self.0.borrow();
        let c = cache.as_ref().filter(|c| c.device == device.id)?;
        c.values.get(&param).copied()
    }
}

/// [`DevicePanel`] on the app state: the panel the QML shows.
pub struct AppDevicePanel {
    pub inner: DevicePanel,
    plugins: PluginParams,
    /// Counts plugin snapshot changes, so the view refreshes when a plugin
    /// loads or its own values change.
    loads: Cell<u64>,
    /// [`AppState::plugin_values_epoch`](crate::state::AppState) when the
    /// snapshot was taken.
    epoch: Cell<u64>,
    /// The parameter a drag in progress moves.
    gesture: Option<ParamId>,
}

impl Default for AppDevicePanel {
    fn default() -> Self {
        let plugins = PluginParams::default();
        let mut inner = DevicePanel::default();
        inner.add_source(Box::new(plugins.clone()));
        Self {
            inner,
            plugins,
            loads: Cell::new(0),
            epoch: Cell::new(0),
            gesture: None,
        }
    }
}

impl AppDevicePanel {
    /// Opens the device whose id reads `id`. Returns false if there is
    /// none.
    pub fn open(&mut self, id: &str) -> bool {
        let found = crate::state::with(|s| {
            s.doc
                .project()
                .all_tracks()
                .flat_map(|t| t.devices.iter())
                .find(|d| d.id.to_string() == id)
                .map(|d| d.id)
        });
        match found {
            Some(d) => {
                // A control still held on the device open now lets go.
                self.end_gesture();
                self.inner.open(d);
                // Read the plugin's values afresh: they may have changed
                // since it was last open.
                self.plugins.clear();
                self.update_plugins();
                true
            }
            None => false,
        }
    }

    pub fn close(&mut self) {
        self.end_gesture();
        self.inner.close();
    }

    pub fn view(&self) -> Option<DeviceView> {
        crate::state::with(|s| {
            let mut view = self.inner.view(s.doc.project())?;
            let project = s.doc.project();
            let track = project
                .device_location(view.id)
                .and_then(|(t, _)| project.track(t));
            for p in &mut view.params {
                let target = AutomationTarget::DeviceParam {
                    device: view.id,
                    param: p.desc.id,
                };
                p.writable = p.desc.records()
                    && (s.mixer.write()
                        || track.is_some_and(|t| {
                            t.automation
                                .iter()
                                .any(|l| l.target == target && s.lanes.write.contains(&l.id))
                        }));
            }
            Some(view)
        })
    }

    /// Describes the open device's plugin once it has loaded.
    fn update_plugins(&self) {
        let device = self.inner.device();
        let changed = crate::state::with(|s| {
            // A plugin's editor may have moved its values without the
            // document changing.
            if self.epoch.replace(s.plugin_values_epoch) != s.plugin_values_epoch {
                self.plugins.clear();
            }
            let revision = (s.doc_serial << 40) ^ s.doc.revision();
            self.plugins.update(s.engine.session(), device, revision)
        });
        if changed {
            self.loads.set(self.loads.get() + 1);
        }
    }

    /// How the open device's plugin is doing, `None` for a built-in device
    /// (or no device open).
    pub fn plugin_status(&self) -> Option<PluginStatus> {
        let device = self.inner.device()?;
        crate::state::with(|s| s.engine.session()?.plugin_status(device))
    }

    /// Restarts the open device's crashed plugin with its state; it plays
    /// again after the next tick. Returns false if it has no sandbox.
    pub fn reload_plugin(&self) -> bool {
        let Some(device) = self.inner.device() else {
            return false;
        };
        let reloading = crate::state::with(|s| {
            s.engine
                .session_mut()
                .is_some_and(|session| session.reload_plugin(device))
        });
        if reloading {
            // Its values are read again once it is back.
            self.plugins.clear();
        }
        reloading
    }

    /// Changes whenever the view may have changed from elsewhere.
    pub fn revision(&self) -> u64 {
        self.update_plugins();
        let (doc, writes) = crate::state::with(|s| {
            (
                (s.doc_serial << 40) ^ s.doc.revision(),
                ((s.lanes.write.len() as u64) << 1) | u64::from(s.mixer.write()),
            )
        });
        doc ^ (self.loads.get() << 56) ^ (writes << 48)
    }

    /// Sets a parameter from a control. During playback, when the
    /// parameter's lane is in write mode (or the mixer writes automation),
    /// a drag records into the lane instead of changing the stored value,
    /// as the mixer's faders do.
    pub fn set_param(&mut self, param: ParamId, value: f64, dragging: bool) {
        let Some(device) = self.inner.device() else {
            return;
        };
        // Like a fader, a drag starts recording only when it starts.
        let start = dragging && self.gesture != Some(param);
        self.gesture = dragging.then_some(param);
        crate::state::with(|s| {
            s.end_drag();
            let mut r = self.write_param(s, device, param, value, start);
            if !dragging {
                r = r.and(self.release(s));
            }
            s.sync();
            if let Err(e) = r {
                s.report(e);
            }
        });
    }

    fn write_param(
        &self,
        s: &mut crate::state::AppState,
        device: DeviceId,
        param: ParamId,
        value: f64,
        start: bool,
    ) -> Result<(), Message> {
        let Some((_, p)) = self.inner.param(s.doc.project(), param) else {
            return Ok(());
        };
        let value = p.desc.sanitize(value);
        let Some(track) = s.doc.project().device_location(device).map(|(t, _)| t) else {
            return Ok(());
        };
        let id = crate::session_mixer::strip_id(track);
        let control = Control::Param { device, param };
        if start && p.desc.records() && !s.mixer.recording_control(id, control) {
            s.mixer.write_lanes.clone_from(&s.lanes.write);
            let start = crate::session_mixer::ParamStart {
                value: p.value,
                tolerance: p.desc.tolerance(),
            };
            s.mixer
                .touch_param(&mut s.doc, s.engine.session_mut(), id, control, start)?;
        }
        if s.mixer.recording_control(id, control) {
            s.mixer
                .set(&mut s.doc, s.engine.session_mut(), id, control, value)
        } else {
            self.inner.set_value(&mut s.doc, param, value)
        }
    }

    /// Ends the takes of every device parameter. A take that was only a
    /// click sets its value instead, as a click does when not recording
    /// (so a double-click's reset can take it back).
    fn release(&self, s: &mut crate::state::AppState) -> Result<(), Message> {
        let clicks = s.mixer.release_params(&mut s.doc, s.engine.session_mut())?;
        let mut r = Ok(());
        for (control, value) in clicks {
            if let Control::Param { device, param } = control
                && self.inner.device() == Some(device)
            {
                r = r.and(self.inner.set_value(&mut s.doc, param, value));
            }
        }
        self.inner.end_gesture(&mut s.doc);
        r
    }

    /// The user let go of the open device's controls.
    pub fn end_gesture(&mut self) {
        self.gesture = None;
        crate::state::with(|s| {
            s.end_drag();
            let r = self.release(s);
            s.sync();
            if let Err(e) = r {
                s.report(e);
            }
        });
    }

    /// Runs an edit, plays the change and reports errors on the status
    /// line.
    pub fn edit(&mut self, f: impl FnOnce(&DevicePanel, &mut Document) -> Result<(), Message>) {
        crate::state::with(|s| {
            s.end_drag();
            let r = f(&self.inner, &mut s.doc);
            s.sync();
            if let Err(e) = r {
                s.report(e);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use daw_devices::devices::eq;
    use daw_model::commands::AddDevice;
    use daw_model::{Track, TrackId};

    use super::*;

    fn builtin(kind: &str) -> Device {
        Device::new(
            kind,
            DeviceSource::Builtin {
                kind: kind.to_owned(),
            },
        )
    }

    /// An audio track with an EQ and a compressor, the EQ open.
    fn rig(kind: TrackKind) -> (Document, DevicePanel, TrackId, Vec<DeviceId>) {
        let mut project = Project::new("Panel");
        let track = Track::new(kind, "Bass");
        let tid = track.id;
        project.tracks = vec![Arc::new(track)];
        let mut doc = Document::new(project);
        let mut ids = Vec::new();
        for (i, k) in ["daw.eq", "daw.compressor"].into_iter().enumerate() {
            let device = builtin(k);
            ids.push(device.id);
            doc.execute(AddDevice {
                track: tid,
                index: i,
                device,
            })
            .unwrap();
        }
        doc.seal();
        let mut panel = DevicePanel::default();
        panel.open(ids[0]);
        (doc, panel, tid, ids)
    }

    fn value(doc: &Document, panel: &DevicePanel, param: ParamId) -> f64 {
        let view = panel.view(doc.project()).unwrap();
        view.params
            .iter()
            .find(|p| p.desc.id == param)
            .unwrap()
            .value
    }

    #[test]
    fn view_describes_builtin_params() {
        let (doc, panel, _, _) = rig(TrackKind::Audio);
        let view = panel.view(doc.project()).unwrap();
        assert_eq!(view.kind, "daw.eq");
        assert_eq!(view.track_name, "Bass");
        assert_eq!((view.index, view.chain_len), (0, 2));
        assert_eq!(
            view.params.len(),
            daw_devices::param_specs("daw.eq").unwrap().len()
        );
        let hp = &view.params[eq::HP_ON as usize];
        assert_eq!(hp.desc.control, ParamControl::Toggle);
        let freq = &view.params[eq::HP_FREQ as usize].desc;
        assert_eq!(freq.control, ParamControl::Continuous { log: true });
        assert_eq!(freq.unit, ParamUnit::Hertz);
        // The log taper puts the geometric middle in the middle.
        let mid = (freq.min * freq.max).sqrt();
        assert!((freq.to_normalized(mid) - 0.5).abs() < 1e-9);
        assert!((freq.from_normalized(0.5) - mid).abs() < 1e-6);
        assert!(view.params.iter().all(ParamView::is_default));
    }

    #[test]
    fn a_drag_is_one_undo_step() {
        let (mut doc, panel, _, _) = rig(TrackKind::Audio);
        for db in [-1.0, -2.0, -3.0, -4.5] {
            panel.set_value(&mut doc, eq::OUTPUT, db).unwrap();
        }
        panel.end_gesture(&mut doc);
        assert_eq!(value(&doc, &panel, eq::OUTPUT), -4.5);
        assert_eq!(
            doc.history().undo_label().as_deref(),
            Some("Change Parameter")
        );
        // A second drag of the same knob is a step of its own.
        panel.set_normalized(&mut doc, eq::OUTPUT, 1.0).unwrap();
        panel.end_gesture(&mut doc);
        let max = daw_devices::param_specs("daw.eq").unwrap()[eq::OUTPUT as usize].max;
        assert_eq!(value(&doc, &panel, eq::OUTPUT), max);
        doc.undo().unwrap();
        assert_eq!(value(&doc, &panel, eq::OUTPUT), -4.5);
        doc.undo().unwrap();
        assert_eq!(value(&doc, &panel, eq::OUTPUT), 0.0);
        assert!(
            !doc.history().can_undo()
                || doc.history().undo_label().as_deref() == Some("Add Device")
        );
    }

    #[test]
    fn values_are_clamped_and_rounded() {
        let (mut doc, panel, _, _) = rig(TrackKind::Audio);
        panel.set_value(&mut doc, eq::OUTPUT, 1e9).unwrap();
        let max = daw_devices::param_specs("daw.eq").unwrap()[eq::OUTPUT as usize].max;
        assert_eq!(value(&doc, &panel, eq::OUTPUT), max);
        panel.set_value(&mut doc, eq::HP_ON, 0.7).unwrap();
        assert_eq!(value(&doc, &panel, eq::HP_ON), 1.0);
        // NaN is the default, which is no change.
        panel.end_gesture(&mut doc);
        let revision = doc.revision();
        panel.set_value(&mut doc, eq::LOW_GAIN, f64::NAN).unwrap();
        assert_eq!(doc.revision(), revision);
    }

    #[test]
    fn reset_returns_to_the_default_as_its_own_step() {
        let (mut doc, panel, _, ids) = rig(TrackKind::Audio);
        panel.set_value(&mut doc, eq::OUTPUT, -6.0).unwrap();
        // Long after the edit (not a double-click's first click).
        panel.click.set(None);
        // Reset while the drag hasn't been sealed still undoes on its own.
        panel.reset(&mut doc, eq::OUTPUT).unwrap();
        assert_eq!(value(&doc, &panel, eq::OUTPUT), 0.0);
        let device = &doc.project().tracks[0].devices[0];
        assert_eq!(device.id, ids[0]);
        assert!(device.params.is_empty(), "stored as the default");
        doc.undo().unwrap();
        assert_eq!(value(&doc, &panel, eq::OUTPUT), -6.0);
        // Resetting a parameter already at its default records nothing.
        doc.undo().unwrap();
        let revision = doc.revision();
        panel.reset(&mut doc, eq::OUTPUT).unwrap();
        assert_eq!(doc.revision(), revision);
    }

    #[test]
    fn a_double_click_reset_takes_back_its_first_click() {
        let (mut doc, panel, _, _) = rig(TrackKind::Audio);
        panel.set_value(&mut doc, eq::OUTPUT, -6.0).unwrap();
        panel.end_gesture(&mut doc);
        // Long before the double-click.
        panel.click.set(None);
        // Both clicks of the double-click moved the slider.
        panel.set_value(&mut doc, eq::OUTPUT, -3.0).unwrap();
        panel.end_gesture(&mut doc);
        panel.set_value(&mut doc, eq::OUTPUT, -2.0).unwrap();
        panel.end_gesture(&mut doc);
        panel.reset(&mut doc, eq::OUTPUT).unwrap();
        assert_eq!(value(&doc, &panel, eq::OUTPUT), 0.0);
        doc.undo().unwrap();
        assert_eq!(value(&doc, &panel, eq::OUTPUT), -6.0, "one undo");
    }

    #[test]
    fn automated_params_are_marked() {
        let (mut doc, panel, track, ids) = rig(TrackKind::Audio);
        let mut lane = daw_model::AutomationLane::new(AutomationTarget::DeviceParam {
            device: ids[0],
            param: eq::OUTPUT,
        });
        lane.points.push(daw_model::AutomationPoint {
            tick: daw_core::time::TickTime(0),
            value: -3.0,
            curve: 0.0,
            step: true,
        });
        doc.execute(daw_model::commands::AddLane { track, lane })
            .unwrap();
        let view = panel.view(doc.project()).unwrap();
        assert!(view.params[eq::OUTPUT as usize].automated);
        assert!(!view.params[eq::LOW_GAIN as usize].automated);
    }

    #[test]
    fn bypass_and_reorder_undo() {
        let (mut doc, mut panel, _, ids) = rig(TrackKind::Audio);
        panel.set_enabled(&mut doc, false).unwrap();
        assert!(!panel.view(doc.project()).unwrap().enabled);
        assert_eq!(doc.history().undo_label().as_deref(), Some("Bypass Device"));
        panel.set_enabled(&mut doc, true).unwrap();
        doc.undo().unwrap();
        assert!(
            !panel.view(doc.project()).unwrap().enabled,
            "each click undoes"
        );
        doc.undo().unwrap();
        assert!(panel.view(doc.project()).unwrap().enabled);

        assert!(!panel.view(doc.project()).unwrap().can_move(-1));
        panel.move_by(&mut doc, -1).unwrap();
        assert_eq!(panel.view(doc.project()).unwrap().index, 0, "first stays");
        panel.move_by(&mut doc, 1).unwrap();
        let order: Vec<_> = doc.project().tracks[0]
            .devices
            .iter()
            .map(|d| d.id)
            .collect();
        assert_eq!(order, [ids[1], ids[0]]);
        assert_eq!(panel.view(doc.project()).unwrap().index, 1);
        panel.move_by(&mut doc, 1).unwrap();
        assert_eq!(panel.view(doc.project()).unwrap().index, 1, "last stays");
        doc.undo().unwrap();
        assert_eq!(panel.view(doc.project()).unwrap().index, 0);

        // A removed device closes the view.
        panel.open(ids[1]);
        doc.execute(daw_model::commands::RemoveDevice::new(ids[1]))
            .unwrap();
        assert!(panel.view(doc.project()).is_none());
    }

    #[test]
    fn the_instrument_stays_first() {
        let (mut doc, mut panel, _, ids) = rig(TrackKind::Instrument);
        let view = panel.view(doc.project()).unwrap();
        assert!(view.is_instrument());
        assert!(!view.can_move(1));
        panel.open(ids[1]);
        assert!(!panel.view(doc.project()).unwrap().can_move(-1));
        panel.move_by(&mut doc, -1).unwrap();
        assert_eq!(doc.project().tracks[0].devices[0].id, ids[0]);
    }

    #[test]
    fn take_tolerance_follows_the_range_and_taper() {
        let info = daw_core::plugin::ParamInfo {
            id: 1,
            name: "Cutoff".into(),
            min: 0.0,
            max: 2000.0,
            default: 0.0,
            automatable: true,
        };
        let linear = ParamDesc::from_info(&info);
        assert_eq!(linear.tolerance(), 2.0);
        let mut log = linear.clone();
        log.min = 20.0;
        log.max = 20000.0;
        log.control = ParamControl::Continuous { log: true };
        // About 0.7% of the value at the bottom of the range.
        assert!(
            (log.tolerance() - 0.138).abs() < 1e-3,
            "{}",
            log.tolerance()
        );
        let mut flat = linear;
        flat.max = flat.min;
        assert!(flat.tolerance() > 0.0);
    }

    #[test]
    fn other_sources_describe_other_devices() {
        struct Fake;
        impl ParamSource for Fake {
            fn describe(&self, device: &Device) -> Option<Vec<ParamDesc>> {
                matches!(device.source, DeviceSource::Plugin { .. }).then(|| {
                    vec![ParamDesc::from_info(&daw_core::plugin::ParamInfo {
                        id: 7,
                        name: "Cutoff".into(),
                        min: 0.0,
                        max: 1.0,
                        default: 0.25,
                        automatable: true,
                    })]
                })
            }

            // The plugin's own state holds 0.75.
            fn current(&self, _: &Device, param: ParamId) -> Option<f64> {
                (param == 7).then_some(0.75)
            }
        }
        let (mut doc, mut panel, track, _) = rig(TrackKind::Audio);
        let plugin = Device::new(
            "Plug",
            DeviceSource::Plugin {
                format: daw_core::plugin::PluginFormatKind::Clap,
                id: "org.example.plug".into(),
            },
        );
        let id = plugin.id;
        doc.execute(AddDevice {
            track,
            index: 2,
            device: plugin,
        })
        .unwrap();
        panel.open(id);
        assert!(panel.view(doc.project()).unwrap().params.is_empty());
        panel.add_source(Box::new(Fake));
        // The plugin's value until the document sets one.
        assert_eq!(value(&doc, &panel, 7), 0.75);
        panel.set_value(&mut doc, 7, 0.5).unwrap();
        assert_eq!(value(&doc, &panel, 7), 0.5);
        // A plugin's default is stored, as the plugin keeps its own value.
        doc.seal();
        panel.click.set(None);
        panel.reset(&mut doc, 7).unwrap();
        assert_eq!(value(&doc, &panel, 7), 0.25);
        // Ids it doesn't describe are refused.
        let revision = doc.revision();
        panel.set_value(&mut doc, 8, 0.5).unwrap();
        assert_eq!(doc.revision(), revision);
    }
}
