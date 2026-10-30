//! The device panel, without naming Qt: one device's parameters, bypass
//! and position in its chain, edited through model commands.
//!
//! What a device's parameters are comes from a [`ParamSource`]. Built-in
//! devices describe theirs with [`BuiltinParams`]; plugins add a source of
//! their own (their parameters are known once the plugin is loaded), and
//! the panel shows them the same way.
//!
//! A parameter drag is one undo step: [`DevicePanel::set_value`] runs
//! [`SetParam`], which merges with the previous edit of the same parameter
//! until [`DevicePanel::end_gesture`] seals the history.

use daw_core::event::ParamId;
use daw_devices::{ParamKind, ParamSpec, ParamUnit, Taper};
use daw_model::commands::{DeviceField, MoveDevice, SetDevice, SetParam};
use daw_model::{Device, DeviceId, DeviceSource, Document, Project, TrackKind};

use crate::message::Message;

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
        }
    }

    fn is_discrete(&self) -> bool {
        !matches!(self.control, ParamControl::Continuous { .. })
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

    /// A plain value as a slider position from 0 to 1.
    pub fn to_normalized(&self, value: f64) -> f64 {
        let v = self.sanitize(value);
        if self.max <= self.min {
            return 0.0;
        }
        match self.control {
            ParamControl::Continuous { log: true } => {
                (v / self.min).ln() / (self.max / self.min).ln()
            }
            _ => (v - self.min) / (self.max - self.min),
        }
    }

    /// A slider position from 0 to 1 as a plain value.
    pub fn from_normalized(&self, position: f64) -> f64 {
        let n = if position.is_nan() {
            return self.default;
        } else {
            position.clamp(0.0, 1.0)
        };
        let v = match self.control {
            ParamControl::Continuous { log: true } => self.min * (self.max / self.min).powf(n),
            _ => self.min + n * (self.max - self.min),
        };
        self.sanitize(v)
    }
}

/// Describes the parameters of the devices it knows.
pub trait ParamSource {
    /// The parameters of `device` in display order, or `None` if this
    /// source doesn't know the device.
    fn describe(&self, device: &Device) -> Option<Vec<ParamDesc>>;
}

/// The built-in devices of `daw-devices`.
pub struct BuiltinParams;

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
        let params = self
            .sources
            .iter()
            .find_map(|s| s.describe(device))
            .unwrap_or_default()
            .into_iter()
            .map(|desc| {
                let value = device
                    .params
                    .get(&desc.id)
                    .map_or(desc.default, |&v| desc.sanitize(v));
                ParamView { desc, value }
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

    fn param(&self, project: &Project, param: ParamId) -> Option<(DeviceId, ParamView)> {
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
        doc.execute(SetParam {
            device,
            param,
            value: Some(value),
        })
        .map(|_| ())
        .map_err(Message::from)
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

/// [`DevicePanel`] on the app state: the panel the QML shows.
#[derive(Default)]
pub struct AppDevicePanel {
    pub inner: DevicePanel,
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
                self.inner.open(d);
                true
            }
            None => false,
        }
    }

    pub fn view(&self) -> Option<DeviceView> {
        crate::state::with(|s| self.inner.view(s.doc.project()))
    }

    /// Changes whenever the view may have changed from elsewhere.
    pub fn revision(&self) -> u64 {
        crate::state::with(|s| (s.doc_serial << 40) ^ s.doc.revision())
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
        assert_eq!(value(&doc, &panel, 7), 0.25);
        panel.set_value(&mut doc, 7, 0.5).unwrap();
        assert_eq!(value(&doc, &panel, 7), 0.5);
        // Ids it doesn't describe are refused.
        let revision = doc.revision();
        panel.set_value(&mut doc, 8, 0.5).unwrap();
        assert_eq!(doc.revision(), revision);
    }
}
