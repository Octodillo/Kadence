//! The mixer on the open project.
//!
//! [`SessionMixer`] turns the project's tracks into channel strips and turns
//! strip edits into model commands, so every mixer move undoes like any
//! other edit and is saved with the project. Meters come from the running
//! [`Session`]. [`AppMixer`] is the same mixer on the app state, as the Mix
//! page uses it.
//!
//! Touch writing: with [`write automation`](SessionMixer::set_write) on (or
//! the control's lane in write mode), a fader, pan, send or device
//! parameter (from the device panel) grabbed during playback is recorded with an
//! [`AutomationRecorder`] instead of changing the control's static value.
//! While held, the control's existing lane is left out of playback so the
//! user hears the hand, not the old curve. On release the take is merged
//! into the lane (creating it if needed) as one undoable edit. That half of
//! the mixer is in [`automation_write`].

mod automation_write;

pub use automation_write::ParamStart;
use automation_write::Take;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::double_click::Clicks;
use crate::instrument::{InstrumentChoice, effects_start, instrument_of, set_instrument};
use crate::message::Message;
use daw_core::level::{self, FADER_FLOOR_DB};
use daw_core::plugin::PluginFormatKind;
use daw_model::commands::{
    AddDevice, AddSend, RemoveDevice, RemoveSend, SendField, SetParam, SetSend, SetTrack,
    TrackField,
};
use daw_model::{
    Command, Device, DeviceSource, Document, LaneId, Project, Send, SendId, Track, TrackId,
    TrackKind, TrackOutput,
};
use daw_session::{Session, effective_mutes};

use crate::mixer::{
    Control, EffectChoice, InsertInfo, RouteTarget, SendInfo, StripId, StripInfo, StripKind,
    StripLevels, effect_choices,
};
use crate::state;

pub(crate) fn strip_id(track: TrackId) -> StripId {
    track.0.as_u128()
}

/// The mixer on a [`Document`] and the [`Session`] playing it. See the
/// module docs.
///
/// Methods that edit the document leave syncing the session to the caller
/// (the app state does it after every call). Only touch writing syncs the
/// session itself, since it changes playback without changing the document.
pub struct SessionMixer {
    write: bool,
    /// Lanes in write mode (set from the track headers): their controls
    /// record even with `write` off.
    pub write_lanes: HashSet<LaneId>,
    takes: Vec<Take>,
    effects: Vec<EffectChoice>,
    /// The control held now, with the document's revision and undo length
    /// when it was grabbed.
    held: Option<((StripId, Control), u64, usize)>,
    /// Presses that just edited a control, which a double-click's reset
    /// takes back.
    clicks: Option<Clicks<(StripId, Control)>>,
}

impl Default for SessionMixer {
    fn default() -> Self {
        Self {
            write: false,
            write_lanes: HashSet::new(),
            takes: Vec::new(),
            effects: effect_choices(),
            held: None,
            clicks: None,
        }
    }
}

/// Appends `device` to the chain of strip `id`, as one undo step.
fn add_device(doc: &mut Document, id: StripId, device: Device) -> bool {
    let Some(track) = find(doc.project(), id) else {
        return false;
    };
    let command = AddDevice {
        track: track.id,
        index: track.devices.len(),
        device,
    };
    let added = doc.execute(command).is_ok();
    doc.seal();
    added
}

/// Strips in display order: tracks, then buses, then the master.
fn ordered(project: &Project) -> Vec<&Arc<Track>> {
    let mut v: Vec<_> = project
        .tracks
        .iter()
        .filter(|t| t.kind != TrackKind::Bus)
        .collect();
    v.extend(project.tracks.iter().filter(|t| t.kind == TrackKind::Bus));
    v.push(&project.master);
    v
}

/// The row of `track`'s strip, in the order the mixer shows strips.
pub fn strip_row(project: &Project, track: TrackId) -> Option<usize> {
    ordered(project).iter().position(|t| t.id == track)
}

/// Every track's strip row (see [`strip_row`]), worked out at once.
pub fn strip_rows(project: &Project) -> HashMap<TrackId, usize> {
    ordered(project)
        .iter()
        .enumerate()
        .map(|(row, t)| (t.id, row))
        .collect()
}

fn find(project: &Project, id: StripId) -> Option<&Arc<Track>> {
    project.all_tracks().find(|t| strip_id(t.id) == id)
}

fn send_to(track: &Track, target: StripId) -> Option<SendId> {
    track
        .sends
        .iter()
        .find(|s| strip_id(s.target) == target)
        .map(|s| s.id)
}

fn insert_info(d: &Device, session: Option<&Session>) -> InsertInfo {
    let device_id = match &d.source {
        DeviceSource::Builtin { kind } => kind.clone(),
        DeviceSource::Plugin { id, .. } => id.clone(),
    };
    InsertInfo {
        uid: d.id.to_string(),
        device_id,
        enabled: d.enabled,
        name: d.name.clone(),
        has_gain_reduction: session.is_some_and(|s| s.device_meter(d.id).is_some()),
    }
}

fn route_target(track: &Track) -> RouteTarget {
    RouteTarget {
        id: strip_id(track.id),
        name: track.name.clone(),
    }
}

/// Which buses each track can route to without a loop.
struct Routing<'a> {
    /// Each bus with every track its audio reaches.
    buses: Vec<(&'a Arc<Track>, HashSet<TrackId>)>,
}

impl<'a> Routing<'a> {
    fn of(project: &'a Project) -> Self {
        let buses = project
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Bus)
            .map(|b| (b, project.downstream(b.id)))
            .collect();
        Self { buses }
    }

    /// Buses `source` can feed: not itself, nor any it is fed by.
    fn targets(&self, source: TrackId) -> impl Iterator<Item = &'a Arc<Track>> + '_ {
        self.buses
            .iter()
            .filter(move |(b, reach)| b.id != source && !reach.contains(&source))
            .map(|(b, _)| *b)
    }
}

impl SessionMixer {
    /// The strips of `project`. Gain reduction meters show once `session`
    /// plays the project.
    pub fn strips(&self, project: &Project, session: Option<&Session>) -> Vec<StripInfo> {
        let silenced = effective_mutes(project);
        let routing = Routing::of(project);
        let master = &project.master;
        let held = |track: TrackId, control: Control| self.held_value(track, control);
        ordered(project)
            .into_iter()
            .map(|t| StripInfo {
                id: strip_id(t.id),
                name: t.name.clone(),
                kind: match t.kind {
                    TrackKind::Master => StripKind::Master,
                    TrackKind::Bus => StripKind::Bus,
                    _ => StripKind::Track,
                },
                volume_db: held(t.id, Control::Volume)
                    .unwrap_or(t.volume_db)
                    .max(FADER_FLOOR_DB),
                pan: held(t.id, Control::Pan).unwrap_or(t.pan),
                muted: t.mute,
                soloed: t.solo,
                silenced: silenced.contains(&t.id),
                has_instrument: t.kind == TrackKind::Instrument,
                instrument: instrument_of(t).map(|d| insert_info(d, session)),
                instrument_choice: None,
                inserts: t.devices[effects_start(t)..]
                    .iter()
                    .map(|d| insert_info(d, session))
                    .collect(),
                sends: t
                    .sends
                    .iter()
                    .map(|s| {
                        let target = strip_id(s.target);
                        SendInfo {
                            target,
                            target_name: project
                                .track(s.target)
                                .map(|x| x.name.clone())
                                .unwrap_or_default(),
                            level_db: held(t.id, Control::Send(target))
                                .unwrap_or(s.level_db)
                                .max(FADER_FLOOR_DB),
                            pre_fader: s.pre_fader,
                        }
                    })
                    .collect(),
                output: match t.output {
                    _ if t.kind == TrackKind::Master => None,
                    TrackOutput::Master => Some(route_target(master)),
                    TrackOutput::Bus(b) => project.track(b).map(|b| route_target(b)),
                },
                output_choices: if t.kind == TrackKind::Master {
                    Vec::new()
                } else {
                    std::iter::once(master)
                        .chain(routing.targets(t.id))
                        .map(|b| route_target(b))
                        .collect()
                },
                send_choices: if t.kind == TrackKind::Master {
                    Vec::new()
                } else {
                    routing
                        .targets(t.id)
                        .filter(|b| t.sends.iter().all(|s| s.target != b.id))
                        .map(|b| route_target(b))
                        .collect()
                },
            })
            .collect()
    }

    /// Adds a send from the strip to bus `target`, at 0 dB after the fader.
    pub fn add_send(&mut self, doc: &mut Document, id: StripId, target: StripId) -> bool {
        let project = doc.project();
        let (Some(track), Some(bus)) = (find(project, id), find(project, target)) else {
            return false;
        };
        if bus.kind != TrackKind::Bus || send_to(track, target).is_some() {
            return false;
        }
        let command = AddSend {
            track: track.id,
            send: Send::new(bus.id),
        };
        let added = doc.execute(command).is_ok();
        doc.seal();
        added
    }

    /// Removes the strip's send to `target` and its automation. A take
    /// running on it is written first.
    pub fn remove_send(
        &mut self,
        doc: &mut Document,
        session: Option<&mut Session>,
        id: StripId,
        target: StripId,
    ) -> Result<(), Message> {
        let Some(track) = find(doc.project(), id) else {
            return Ok(());
        };
        let Some(send) = send_to(track, target) else {
            return Ok(());
        };
        let track = track.id;
        let result = self.end_control(doc, session, track, Control::Send(target));
        let r = doc.execute(RemoveSend::new(send));
        doc.seal();
        result.and(r.map(|_| ()).map_err(Message::from))
    }

    /// Taps the strip's send to `target` before or after the fader.
    pub fn set_send_pre_fader(
        &mut self,
        doc: &mut Document,
        id: StripId,
        target: StripId,
        pre_fader: bool,
    ) -> Result<(), Message> {
        let Some(send) = find(doc.project(), id).and_then(|t| send_to(t, target)) else {
            return Ok(());
        };
        let r = doc.execute(SetSend {
            id: send,
            field: SendField::PreFader(pre_fader),
        });
        doc.seal();
        r.map(|_| ()).map_err(Message::from)
    }

    /// Routes the strip's output to `target`: a bus, or the master.
    pub fn set_output(
        &mut self,
        doc: &mut Document,
        id: StripId,
        target: StripId,
    ) -> Result<(), Message> {
        let project = doc.project();
        let (Some(track), Some(to)) = (find(project, id), find(project, target)) else {
            return Ok(());
        };
        let output = match to.kind {
            TrackKind::Master => TrackOutput::Master,
            _ => TrackOutput::Bus(to.id),
        };
        if track.kind == TrackKind::Master || track.output == output {
            return Ok(());
        }
        let r = doc.execute(SetTrack::new(track.id, TrackField::Output(output)));
        doc.seal();
        r.map(|_| ()).map_err(Message::from)
    }

    /// Moves a control: records it while a take for it runs, otherwise
    /// sets its static value.
    pub fn set(
        &mut self,
        doc: &mut Document,
        session: Option<&mut Session>,
        id: StripId,
        control: Control,
        value: f64,
    ) -> Result<(), Message> {
        let Some(track) = find(doc.project(), id).cloned() else {
            return Ok(());
        };
        let value = match control {
            Control::Volume | Control::Send(_) => level::fader_to_stored_db(value),
            Control::Pan => value.clamp(-1.0, 1.0),
            Control::Param { .. } => value,
        };
        if self.record_move(doc.project(), session, track.id, control, value)? {
            return Ok(());
        }
        let command: Box<dyn Command> = match control {
            Control::Volume => Box::new(SetTrack::new(track.id, TrackField::VolumeDb(value))),
            Control::Pan if track.kind == TrackKind::Master => return Ok(()),
            Control::Pan => Box::new(SetTrack::new(track.id, TrackField::Pan(value))),
            Control::Send(target) => {
                let Some(send) = send_to(&track, target) else {
                    return Ok(());
                };
                Box::new(SetSend {
                    id: send,
                    field: SendField::LevelDb(value),
                })
            }
            Control::Param { device, param } => Box::new(SetParam {
                device,
                param,
                value: Some(value),
            }),
        };
        doc.execute_boxed(command).map_err(Message::from)?;
        Ok(())
    }

    pub fn set_muted(&mut self, doc: &mut Document, id: StripId, on: bool) -> Result<(), Message> {
        self.set_flag(doc, id, TrackField::Mute(on))
    }

    pub fn set_soloed(&mut self, doc: &mut Document, id: StripId, on: bool) -> Result<(), Message> {
        self.set_flag(doc, id, TrackField::Solo(on))
    }

    fn set_flag(
        &mut self,
        doc: &mut Document,
        id: StripId,
        field: TrackField,
    ) -> Result<(), Message> {
        let Some(track) = find(doc.project(), id) else {
            return Ok(());
        };
        // The master always plays.
        if track.kind == TrackKind::Master {
            return Ok(());
        }
        let id = track.id;
        let r = doc.execute(SetTrack::new(id, field));
        // A click is a gesture of its own.
        doc.seal();
        r.map(|_| ()).map_err(Message::from)
    }

    /// Appends a built-in effect to the strip's chain.
    pub fn add_insert(&mut self, doc: &mut Document, id: StripId, device_id: &str) -> bool {
        let Some(effect) = self.effects.iter().find(|e| e.id == device_id) else {
            return false;
        };
        let source = DeviceSource::Builtin {
            kind: effect.id.clone(),
        };
        let name = effect.name.clone();
        add_device(doc, id, Device::new(name, source))
    }

    /// Appends plugin `plugin` of `format` to the strip's chain, as a
    /// device called `name`. The session loads it on the next sync.
    pub fn add_plugin_insert(
        &mut self,
        doc: &mut Document,
        id: StripId,
        format: PluginFormatKind,
        plugin: &str,
        name: &str,
    ) -> bool {
        if plugin.is_empty() {
            return false;
        }
        let source = DeviceSource::Plugin {
            format,
            id: plugin.to_owned(),
        };
        add_device(doc, id, Device::new(name, source))
    }

    pub fn remove_insert(
        &mut self,
        doc: &mut Document,
        id: StripId,
        index: usize,
    ) -> Result<(), Message> {
        // `index` counts effects, which come after the instrument.
        let Some(device) =
            find(doc.project(), id).and_then(|t| t.devices.get(effects_start(t) + index))
        else {
            return Ok(());
        };
        let device = device.id;
        let r = doc.execute(RemoveDevice::new(device));
        doc.seal();
        r.map(|_| ()).map_err(Message::from)
    }

    pub fn effects(&self) -> Vec<EffectChoice> {
        self.effects.clone()
    }

    /// Puts `choice` in an instrument track's instrument slot.
    pub fn set_instrument(
        &mut self,
        doc: &mut Document,
        id: StripId,
        choice: &InstrumentChoice,
    ) -> Result<(), Message> {
        let Some(track) = find(doc.project(), id) else {
            return Ok(());
        };
        let Some(command) = set_instrument(doc.project(), track.id, choice) else {
            return Ok(());
        };
        let r = doc.execute_boxed(command);
        doc.seal();
        r.map(|_| ()).map_err(Message::from)
    }

    /// Meter readings in strip order, reset as they are read.
    pub fn levels(&self, project: &Project, session: Option<&Session>) -> Vec<StripLevels> {
        ordered(project)
            .into_iter()
            .map(|t| {
                let Some(s) = session else {
                    return StripLevels::default();
                };
                let peaks = s.track_meter(t.id).map(|m| m.take()).unwrap_or_default();
                let l = peaks.first().copied().unwrap_or(0.0);
                let r = peaks.get(1).copied().unwrap_or(l);
                let gain_reduction_db = t
                    .devices
                    .iter()
                    .filter_map(|d| s.device_meter(d.id))
                    .map(|m| m.take())
                    .reduce(f32::max);
                StripLevels {
                    peak: [l, r],
                    gain_reduction_db,
                }
            })
            .collect()
    }
}

/// The app state's [`SessionMixer`]: the mixer the Mix page shows. The
/// mixer itself lives in the state so the device panel records into the
/// same takes. Takes are ticked with the project (see
/// `AppState::tick_mixer`), whichever page is showing. Called on the UI
/// thread only.
#[derive(Default)]
pub struct AppMixer;

impl AppMixer {
    /// Runs `f`, then plays any document change and reports errors on the
    /// status line.
    fn edit<R>(
        &mut self,
        f: impl FnOnce(&mut SessionMixer, &mut Document, Option<&mut Session>) -> R,
    ) -> R {
        state::with(|s| {
            s.end_drag();
            s.mixer.write_lanes.clone_from(&s.lanes.write);
            let r = f(&mut s.mixer, &mut s.doc, s.engine.session_mut());
            s.sync();
            r
        })
    }

    fn report(
        &mut self,
        f: impl FnOnce(&mut SessionMixer, &mut Document, Option<&mut Session>) -> Result<(), Message>,
    ) {
        if let Err(e) = self.edit(f) {
            state::with(|s| s.report(e));
        }
    }
}

impl AppMixer {
    /// Strips in display order: tracks, then buses, then master.
    pub fn strips(&self) -> Vec<StripInfo> {
        state::with(|s| {
            let mut strips = s.mixer.strips(s.doc.project(), s.engine.session());
            for (strip, track) in strips.iter_mut().zip(ordered(s.doc.project())) {
                strip.instrument_choice =
                    instrument_of(track).and_then(|d| s.instruments.position(&d.source));
            }
            strips
        })
    }

    pub fn set_volume(&mut self, id: StripId, db: f64) {
        self.report(|m, d, s| m.set(d, s, id, Control::Volume, db));
    }

    pub fn set_pan(&mut self, id: StripId, pan: f64) {
        self.report(|m, d, s| m.set(d, s, id, Control::Pan, pan));
    }

    pub fn set_muted(&mut self, id: StripId, muted: bool) {
        self.report(|m, d, _| m.set_muted(d, id, muted));
    }

    pub fn set_soloed(&mut self, id: StripId, soloed: bool) {
        self.report(|m, d, _| m.set_soloed(d, id, soloed));
    }

    pub fn set_send_level(&mut self, id: StripId, target: StripId, db: f64) {
        self.report(|m, d, s| m.set(d, s, id, Control::Send(target), db));
    }

    /// Adds a send to bus `target`, at 0 dB after the fader.
    pub fn add_send(&mut self, id: StripId, target: StripId) -> bool {
        self.edit(|m, d, _| m.add_send(d, id, target))
    }

    pub fn remove_send(&mut self, id: StripId, target: StripId) {
        self.report(|m, d, s| m.remove_send(d, s, id, target));
    }

    pub fn set_send_pre_fader(&mut self, id: StripId, target: StripId, pre_fader: bool) {
        self.report(|m, d, _| m.set_send_pre_fader(d, id, target, pre_fader));
    }

    /// Routes the strip's output to `target`, a bus or the master.
    pub fn set_output(&mut self, id: StripId, target: StripId) {
        self.report(|m, d, _| m.set_output(d, id, target));
    }

    /// Appends a built-in effect to the strip's insert chain.
    pub fn add_insert(&mut self, id: StripId, device_id: &str) -> bool {
        self.edit(|m, d, _| m.add_insert(d, id, device_id))
    }

    /// Appends a plugin to the strip's insert chain, as a device called
    /// `name`.
    pub fn add_plugin_insert(
        &mut self,
        id: StripId,
        format: PluginFormatKind,
        plugin: &str,
        name: &str,
    ) -> bool {
        self.edit(|m, d, _| m.add_plugin_insert(d, id, format, plugin, name))
    }

    /// Removes the effect at `index` in [`StripInfo::inserts`].
    pub fn remove_insert(&mut self, id: StripId, index: usize) {
        self.report(|m, d, _| m.remove_insert(d, id, index));
    }

    pub fn effects(&self) -> Vec<EffectChoice> {
        state::with(|s| s.mixer.effects())
    }

    /// Puts `choice` in an instrument track's instrument slot.
    pub fn set_instrument(&mut self, id: StripId, choice: &InstrumentChoice) {
        self.report(|m, d, _| m.set_instrument(d, id, choice));
    }

    /// Meter readings, parallel to [`strips`](Self::strips). Resets them.
    pub fn levels(&mut self) -> Vec<StripLevels> {
        state::with(|s| s.mixer.levels(s.doc.project(), s.engine.session()))
    }

    /// Changes whenever the strips may have changed from elsewhere (an
    /// edit in another view, undo, another project opened).
    pub fn revision(&self) -> u64 {
        // Strips depend on the document, and on which plugins a scan found
        // to be effects (that moves where an instrument track's inserts
        // start). A new document restarts its revision count, so its serial
        // goes in the high bits, the plugin list's in the middle.
        state::with(|s| {
            let plugins = s.known_plugins.map_or(0, |r| r + 1);
            (s.doc_serial << 40) ^ (plugins << 24) ^ s.doc.revision()
        })
    }

    /// Touch writing: while on, moving a control during playback records
    /// it as automation.
    pub fn set_write_automation(&mut self, on: bool) {
        self.report(|m, d, s| m.set_write(d, s, on));
    }

    pub fn write_automation(&self) -> bool {
        state::with(|s| s.mixer.write())
    }

    /// The user grabbed (`pressed`) or let go of a control.
    pub fn touch(&mut self, id: StripId, control: Control, pressed: bool) {
        self.report(|m, d, s| m.touch(d, s, id, control, pressed));
    }

    /// A double-click on a control: back to its default.
    pub fn reset(&mut self, id: StripId, control: Control) {
        self.report(|m, d, s| m.reset(d, s, id, control));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daw_core::time::PPQ;
    use daw_core::time::{SampleTime, TickTime};
    use daw_engine::EngineProcessor;
    use daw_model::Send;
    use daw_model::commands::AddLane;
    use daw_model::{AutomationLane, AutomationPoint, AutomationTarget};
    use daw_session::SessionConfig;

    struct Rig {
        doc: Document,
        session: Session,
        proc: EngineProcessor,
        mixer: SessionMixer,
        out: [Vec<f32>; 2],
    }

    const BLOCK: usize = 256;

    /// Two tracks, a reverb bus they send to, and the master.
    fn rig() -> Rig {
        let mut project = Project::new("Mix");
        let bus = Track::new(TrackKind::Bus, "Reverb");
        let mut a = Track::new(TrackKind::Audio, "Drums");
        a.sends.push(Send::new(bus.id));
        let b = Track::new(TrackKind::Instrument, "Keys");
        project.tracks = vec![Arc::new(bus), Arc::new(a), Arc::new(b)];
        let mut doc = Document::new(project);
        let (mut session, proc) = Session::new(SessionConfig {
            inputs: 0,
            ..Default::default()
        });
        session.sync(&doc.snapshot()).unwrap();
        Rig {
            doc,
            session,
            proc,
            mixer: SessionMixer::default(),
            out: [vec![0.0; BLOCK], vec![0.0; BLOCK]],
        }
    }

    impl Rig {
        fn sync(&mut self) {
            self.session.sync(&self.doc.snapshot()).unwrap();
        }

        fn run(&mut self, blocks: usize) {
            for _ in 0..blocks {
                let [l, r] = &mut self.out;
                self.proc.process(
                    BLOCK,
                    &[],
                    &mut [&mut l[..], &mut r[..]],
                    &daw_core::event::EventList::with_capacity(0),
                );
            }
            self.session.collect_garbage();
        }

        fn id(&self, name: &str) -> StripId {
            self.mixer
                .strips(self.doc.project(), Some(&self.session))
                .iter()
                .find(|s| s.name == name)
                .unwrap()
                .id
        }

        fn track(&self, name: &str) -> Arc<Track> {
            self.doc
                .project()
                .all_tracks()
                .find(|t| t.name == name)
                .unwrap()
                .clone()
        }

        fn set(&mut self, name: &str, control: Control, v: f64) {
            let id = self.id(name);
            self.mixer
                .set(&mut self.doc, Some(&mut self.session), id, control, v)
                .unwrap();
            self.sync();
        }

        fn touch(&mut self, name: &str, control: Control, pressed: bool) {
            let id = self.id(name);
            self.mixer
                .touch(&mut self.doc, Some(&mut self.session), id, control, pressed)
                .unwrap();
            self.sync();
        }

        fn tick(&mut self) {
            self.mixer
                .tick(&mut self.doc, Some(&mut self.session))
                .unwrap();
            self.sync();
        }
    }

    #[test]
    fn strips_follow_the_project() {
        let r = rig();
        let strips = r.mixer.strips(r.doc.project(), Some(&r.session));
        let names: Vec<_> = strips.iter().map(|s| (s.name.as_str(), s.kind)).collect();
        assert_eq!(
            names,
            [
                ("Drums", StripKind::Track),
                ("Keys", StripKind::Track),
                ("Reverb", StripKind::Bus),
                ("", StripKind::Master),
            ]
        );
        assert_eq!(strips[0].sends.len(), 1);
        assert_eq!(strips[0].sends[0].target_name, "Reverb");
        // A track's row is where its strip is, not where the track is.
        let project = r.doc.project();
        for name in ["Drums", "Keys", "Reverb"] {
            let row = strip_row(project, r.track(name).id).unwrap();
            assert_eq!(strips[row].name, name);
        }
        assert_eq!(
            strip_row(project, project.master.id),
            Some(strips.len() - 1)
        );
        assert_eq!(strip_row(project, TrackId::new()), None);
    }

    #[test]
    fn edits_are_commands_that_undo() {
        let mut r = rig();
        r.set("Drums", Control::Volume, -12.0);
        r.set("Drums", Control::Volume, -70.0);
        assert_eq!(r.track("Drums").volume_db, level::SILENCE_DB);
        assert_eq!(
            r.mixer.strips(r.doc.project(), Some(&r.session))[0].volume_db,
            FADER_FLOOR_DB
        );
        r.set("Drums", Control::Pan, 0.5);
        let reverb = r.id("Reverb");
        r.set("Drums", Control::Send(reverb), -6.0);
        assert_eq!(r.track("Drums").sends[0].level_db, -6.0);
        // A fader drag is one undo step.
        r.doc.undo().unwrap();
        r.doc.undo().unwrap();
        r.doc.undo().unwrap();
        assert_eq!(r.track("Drums").volume_db, 0.0);

        let drums = r.id("Drums");
        r.mixer.set_soloed(&mut r.doc, drums, true).unwrap();
        let strips = r.mixer.strips(r.doc.project(), Some(&r.session));
        let silenced: Vec<_> = strips.iter().map(|s| s.silenced).collect();
        // Keys is silenced; the bus Drums feeds and the master are not.
        assert_eq!(silenced, [false, true, false, false]);
        let master = r.id("");
        r.mixer.set_muted(&mut r.doc, master, true).unwrap();
        assert!(!r.track("").mute, "the master can't be muted");
    }

    #[test]
    fn inserts_are_devices_with_meters() {
        let mut r = rig();
        let keys = r.id("Keys");
        assert!(r.mixer.add_insert(&mut r.doc, keys, "daw.compressor"));
        assert!(!r.mixer.add_insert(&mut r.doc, keys, "daw.synth"));
        r.sync();
        let strips = r.mixer.strips(r.doc.project(), Some(&r.session));
        assert!(strips[1].inserts[0].has_gain_reduction);
        let device = r.track("Keys").devices[0].id;
        assert!(r.session.device_meter(device).is_some());
        assert_eq!(r.mixer.levels(r.doc.project(), Some(&r.session)).len(), 4);
        r.mixer.remove_insert(&mut r.doc, keys, 0).unwrap();
        assert!(r.track("Keys").devices.is_empty());
    }

    #[test]
    fn plugin_inserts_are_plugin_devices_that_undo() {
        let mut r = rig();
        let drums = r.id("Drums");
        assert!(
            !r.mixer
                .add_plugin_insert(&mut r.doc, drums, PluginFormatKind::Clap, "", "x")
        );
        assert!(r.mixer.add_plugin_insert(
            &mut r.doc,
            drums,
            PluginFormatKind::Lv2,
            "http://example.org/eq",
            "Para EQ",
        ));
        let device = &r.track("Drums").devices[0];
        assert_eq!(device.name, "Para EQ");
        assert_eq!(
            device.source,
            DeviceSource::Plugin {
                format: PluginFormatKind::Lv2,
                id: "http://example.org/eq".into(),
            }
        );
        assert_eq!(r.doc.history().undo_label().as_deref(), Some("Add Device"));
        // Without a plugin loader the session plays it bypassed.
        r.sync();
        r.run(2);
        r.doc.undo().unwrap();
        assert!(r.track("Drums").devices.is_empty());
    }

    #[test]
    fn the_instrument_has_its_own_slot_before_the_effects() {
        let mut r = rig();
        let keys = r.id("Keys");
        let choices = crate::instrument::InstrumentChoices::builtin();
        let synth = choices.default_choice().unwrap();
        r.mixer.set_instrument(&mut r.doc, keys, synth).unwrap();
        assert!(r.mixer.add_insert(&mut r.doc, keys, "daw.compressor"));
        let strips = r.mixer.strips(r.doc.project(), Some(&r.session));
        assert!(strips[1].has_instrument);
        assert_eq!(
            strips[1].instrument.as_ref().map(|i| i.device_id.as_str()),
            Some("daw.synth")
        );
        let inserts: Vec<_> = strips[1].inserts.iter().map(|i| &i.device_id).collect();
        assert_eq!(inserts, ["daw.compressor"]);
        assert!(!strips[0].has_instrument && strips[0].instrument.is_none());

        // Effect 0 is the compressor, not the synth.
        r.mixer.remove_insert(&mut r.doc, keys, 0).unwrap();
        let devices = &r.track("Keys").devices;
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].source, synth.source);

        let sampler = choices
            .as_slice()
            .iter()
            .find(|c| c.name != synth.name)
            .unwrap();
        r.mixer.set_instrument(&mut r.doc, keys, sampler).unwrap();
        assert_eq!(r.track("Keys").devices.len(), 1);
        assert_eq!(r.track("Keys").devices[0].source, sampler.source);
        r.doc.undo().unwrap();
        assert_eq!(r.track("Keys").devices[0].source, synth.source);
        // Audio tracks have no instrument slot.
        let drums = r.id("Drums");
        r.mixer.set_instrument(&mut r.doc, drums, synth).unwrap();
        assert!(r.track("Drums").devices.is_empty());
    }

    #[test]
    fn reset_takes_back_the_first_click_and_is_one_step() {
        let mut r = rig();
        r.set("Drums", Control::Volume, -6.0);
        r.doc.seal();
        // A double-click's first click jumps the fader.
        r.touch("Drums", Control::Volume, true);
        r.set("Drums", Control::Volume, -40.0);
        r.touch("Drums", Control::Volume, false);
        let id = r.id("Drums");
        r.mixer
            .reset(&mut r.doc, Some(&mut r.session), id, Control::Volume)
            .unwrap();
        assert_eq!(r.track("Drums").volume_db, 0.0);
        // A drag after the reset is its own step.
        r.touch("Drums", Control::Volume, true);
        r.set("Drums", Control::Volume, -10.0);
        r.touch("Drums", Control::Volume, false);
        r.doc.undo().unwrap();
        assert_eq!(r.track("Drums").volume_db, 0.0, "undo the drag");
        r.doc.undo().unwrap();
        assert_eq!(
            r.track("Drums").volume_db,
            -6.0,
            "undo the reset, not the click"
        );
    }

    #[test]
    fn reset_takes_back_both_clicks_of_a_double_click() {
        for second in [-40.0, -39.0] {
            let mut r = rig();
            r.set("Drums", Control::Volume, -6.0);
            // A wheel turn: no press, so not a click.
            r.set("Drums", Control::Volume, -5.0);
            // Both clicks reach the mixer before the reset; the second
            // edits only if it lands somewhere else.
            for v in [-40.0, second] {
                r.touch("Drums", Control::Volume, true);
                r.set("Drums", Control::Volume, v);
                r.touch("Drums", Control::Volume, false);
            }
            let id = r.id("Drums");
            r.mixer
                .reset(&mut r.doc, Some(&mut r.session), id, Control::Volume)
                .unwrap();
            assert_eq!(r.track("Drums").volume_db, 0.0);
            r.doc.undo().unwrap();
            assert_eq!(
                r.track("Drums").volume_db,
                -5.0,
                "one undo back to before the clicks, keeping the wheel turn"
            );
        }
    }

    #[test]
    fn reset_keeps_an_older_edit() {
        let mut r = rig();
        // Not a double-click: the edit was not the last press.
        r.touch("Drums", Control::Volume, true);
        r.set("Drums", Control::Volume, -6.0);
        r.touch("Drums", Control::Volume, false);
        r.set("Drums", Control::Pan, 0.5);
        let id = r.id("Drums");
        r.mixer
            .reset(&mut r.doc, Some(&mut r.session), id, Control::Volume)
            .unwrap();
        assert_eq!(r.track("Drums").volume_db, 0.0);
        assert_eq!(r.track("Drums").pan, 0.5);
        r.doc.undo().unwrap();
        assert_eq!(r.track("Drums").volume_db, -6.0);
    }

    #[test]
    fn touch_write_records_a_lane() {
        let mut r = rig();
        r.set("Drums", Control::Volume, -6.0);
        r.mixer
            .set_write(&mut r.doc, Some(&mut r.session), true)
            .unwrap();
        // Not playing: a touch is an ordinary edit.
        r.touch("Drums", Control::Volume, true);
        r.set("Drums", Control::Volume, -3.0);
        r.touch("Drums", Control::Volume, false);
        assert!(r.track("Drums").automation.is_empty());
        assert_eq!(r.track("Drums").volume_db, -3.0);

        r.session.play().unwrap();
        r.run(40);
        let start = r.session.controller().position();
        r.touch("Drums", Control::Volume, true);
        for step in 0..20 {
            r.set("Drums", Control::Volume, -3.0 - step as f64);
            r.run(4);
            r.tick();
        }
        let strips = r.mixer.strips(r.doc.project(), Some(&r.session));
        assert_eq!(strips[0].volume_db, -22.0, "the fader shows the hand");
        assert_eq!(r.track("Drums").volume_db, -3.0, "static value untouched");
        let end = r.session.controller().position();
        r.touch("Drums", Control::Volume, false);

        let drums = r.track("Drums");
        assert_eq!(drums.automation.len(), 1);
        let lane = &drums.automation[0];
        assert_eq!(lane.target, AutomationTarget::Volume);
        let engine = r.session.engine_lane(&lane.points);
        let v = |t: i64| engine.value_at(SampleTime(t)).unwrap();
        assert_eq!(v(0), -3.0, "before the take: the static value");
        assert!((v(start.0) - -3.0).abs() < 0.5);
        assert!(
            (v(end.0 - BLOCK as i64) - -22.0).abs() < 1.0,
            "{}",
            v(end.0)
        );
        assert_eq!(v(end.0 + 48_000), -3.0, "after the take: back to static");
        // One undo step removes the take.
        r.doc.undo().unwrap();
        assert!(r.track("Drums").automation.is_empty());
    }

    #[test]
    fn device_params_record_into_a_lane_in_write_mode() {
        let mut r = rig();
        let drums = r.track("Drums").id;
        let eq = Device::new(
            "EQ",
            DeviceSource::Builtin {
                kind: "daw.eq".into(),
            },
        );
        let device = eq.id;
        r.doc
            .execute(AddDevice {
                track: drums,
                index: 0,
                device: eq,
            })
            .unwrap();
        let spec = daw_devices::param_specs("daw.eq")
            .unwrap()
            .iter()
            .find(|p| p.automatable && p.kind == daw_devices::ParamKind::Continuous)
            .copied()
            .unwrap();
        let control = Control::Param {
            device,
            param: spec.id,
        };
        let target = AutomationTarget::DeviceParam {
            device,
            param: spec.id,
        };
        // The lane is in write mode; the mixer's own switch stays off.
        let lane = AutomationLane::new(target);
        let lane_id = lane.id;
        r.doc.execute(AddLane { track: drums, lane }).unwrap();
        r.mixer.write_lanes.insert(lane_id);
        r.sync();

        r.session.play().unwrap();
        r.run(40);
        r.touch("Drums", control, true);
        let (lo, hi) = (spec.min, spec.max);
        for step in 0..10 {
            r.set("Drums", control, lo + (hi - lo) * f64::from(step) / 10.0);
            r.run(4);
            r.tick();
        }
        r.touch("Drums", control, false);

        let drums = r.track("Drums");
        let d = drums.devices.iter().find(|d| d.id == device).unwrap();
        assert!(!d.params.contains_key(&spec.id), "stored value untouched");
        let lane = drums.automation.iter().find(|l| l.id == lane_id).unwrap();
        assert!(lane.points.len() >= 2, "{:?}", lane.points);
        let last = lane.points.iter().map(|p| p.value).fold(f64::MIN, f64::max);
        assert!((last - (lo + (hi - lo) * 0.9)).abs() < 1e-6 * (hi - lo).abs().max(1.0));
        r.doc.undo().unwrap();
        assert!(r.track("Drums").automation[0].points.is_empty());
    }

    #[test]
    fn plugin_params_record_from_the_panels_value_and_a_click_is_not_a_take() {
        let mut r = rig();
        let drums = r.track("Drums").id;
        let plugin = Device::new(
            "Plug",
            DeviceSource::Plugin {
                format: PluginFormatKind::Clap,
                id: "org.example.plug".into(),
            },
        );
        let device = plugin.id;
        r.doc
            .execute(AddDevice {
                track: drums,
                index: 0,
                device: plugin,
            })
            .unwrap();
        r.sync();
        r.mixer.set_write(&mut r.doc, None, true).unwrap();
        r.session.play().unwrap();
        r.run(8);
        let id = r.id("Drums");
        let control = Control::Param { device, param: 3 };
        let start = ParamStart {
            value: 0.25,
            tolerance: 1e-3,
        };

        // A press without a drag: dropped, and handed back as a click.
        r.mixer
            .touch_param(&mut r.doc, Some(&mut r.session), id, control, start)
            .unwrap();
        assert!(
            r.mixer.recording_control(id, control),
            "no stored value needed"
        );
        r.mixer
            .set(&mut r.doc, Some(&mut r.session), id, control, 0.5)
            .unwrap();
        let clicks = r
            .mixer
            .release_params(&mut r.doc, Some(&mut r.session))
            .unwrap();
        assert_eq!(clicks, vec![(control, 0.5)]);
        assert!(r.track("Drums").automation.is_empty());
        assert!(r.session.suspended_lanes().is_empty());

        // A drag: written from the first event on, over the panel's value.
        r.mixer
            .touch_param(&mut r.doc, Some(&mut r.session), id, control, start)
            .unwrap();
        for step in 1..=5 {
            r.mixer
                .set(
                    &mut r.doc,
                    Some(&mut r.session),
                    id,
                    control,
                    f64::from(step) / 10.0,
                )
                .unwrap();
            r.run(4);
            r.mixer.tick(&mut r.doc, Some(&mut r.session)).unwrap();
        }
        let clicks = r
            .mixer
            .release_params(&mut r.doc, Some(&mut r.session))
            .unwrap();
        assert!(clicks.is_empty());
        let drums = r.track("Drums");
        assert!(drums.devices[0].params.is_empty(), "stored value untouched");
        let lane = &drums.automation[0];
        assert_eq!(lane.points[0].value, 0.25, "{:?}", lane.points);
        assert!(lane.points.iter().any(|p| (p.value - 0.5).abs() < 1e-9));
        assert!(!r.mixer.recording());
    }

    #[test]
    fn touch_overwrites_and_suspends_an_existing_lane() {
        let mut r = rig();
        let drums = r.track("Drums").id;
        let mut lane = AutomationLane::new(AutomationTarget::Pan);
        lane.points = vec![AutomationPoint {
            tick: TickTime(0),
            value: -1.0,
            curve: 0.0,
            step: true,
        }];
        r.doc.execute(AddLane { track: drums, lane }).unwrap();
        r.sync();
        r.mixer
            .set_write(&mut r.doc, Some(&mut r.session), true)
            .unwrap();
        r.session.play().unwrap();
        r.run(8);
        r.touch("Drums", Control::Pan, true);
        assert_eq!(r.session.suspended_lanes().len(), 1);
        assert_eq!(
            r.mixer.strips(r.doc.project(), Some(&r.session))[0].pan,
            -1.0,
            "the take starts from what plays"
        );
        r.set("Drums", Control::Pan, 0.5);
        r.run(8);
        // Stopping ends the take.
        r.session.stop().unwrap();
        r.tick();
        assert!(r.session.suspended_lanes().is_empty());
        let lane = &r.track("Drums").automation[0];
        assert!(lane.points.iter().any(|p| p.value == 0.5));
        assert_eq!(lane.points[0].value, -1.0);
        assert!(lane.points.windows(2).all(|w| w[0].tick < w[1].tick));
        assert_eq!(lane.points.last().unwrap().value, -1.0, "restored after");
    }

    #[test]
    fn separate_gestures_undo_separately() {
        let mut r = rig();
        r.set("Drums", Control::Volume, -6.0);
        r.touch("Drums", Control::Volume, false);
        r.set("Drums", Control::Volume, -12.0);
        r.touch("Drums", Control::Volume, false);
        r.doc.undo().unwrap();
        assert_eq!(r.track("Drums").volume_db, -6.0);
        let drums = r.id("Drums");
        r.mixer.set_muted(&mut r.doc, drums, true).unwrap();
        r.mixer.set_muted(&mut r.doc, drums, false).unwrap();
        r.doc.undo().unwrap();
        assert!(r.track("Drums").mute, "each click undoes on its own");
    }

    #[test]
    fn a_held_control_survives_other_edits_and_loop_wraps() {
        let mut r = rig();
        r.mixer
            .set_write(&mut r.doc, Some(&mut r.session), true)
            .unwrap();
        r.session
            .set_loop(Some((TickTime(0), TickTime(4 * PPQ))))
            .unwrap();
        r.session.play().unwrap();
        r.run(4);
        r.touch("Drums", Control::Volume, true);
        r.set("Drums", Control::Volume, -20.0);
        r.run(4);
        r.tick();
        // Another edit syncs the session; the held value stays live.
        r.set("Keys", Control::Pan, 0.3);
        let drums = r.track("Drums").id;
        let key = r.session.strip_key(drums).unwrap();
        r.session.sync(&r.doc.snapshot()).unwrap();
        assert!(r.session.controller().has_node(key), "the strip still runs");
        // Jump back as a loop wrap does: the take is written, and its new
        // lane is held out of playback while the hand stays on.
        r.session.locate(TickTime(0)).unwrap();
        r.run(1);
        r.tick();
        assert_eq!(r.track("Drums").automation.len(), 1);
        assert_eq!(r.session.suspended_lanes().len(), 1);
        assert_eq!(
            r.mixer.strips(r.doc.project(), Some(&r.session))[0].volume_db,
            -20.0
        );
        r.touch("Drums", Control::Volume, false);
        assert!(r.session.suspended_lanes().is_empty());
    }

    #[test]
    fn writing_keeps_curves_outside_the_take() {
        let mut r = rig();
        let drums = r.track("Drums").id;
        let mut lane = AutomationLane::new(AutomationTarget::Pan);
        let curved = AutomationPoint {
            tick: TickTime(0),
            value: -1.0,
            curve: 0.6,
            step: false,
        };
        lane.points = vec![
            curved.clone(),
            AutomationPoint {
                tick: TickTime(PPQ),
                value: 1.0,
                curve: 0.0,
                step: true,
            },
        ];
        r.doc.execute(AddLane { track: drums, lane }).unwrap();
        r.sync();
        r.mixer
            .set_write(&mut r.doc, Some(&mut r.session), true)
            .unwrap();
        r.session.play().unwrap();
        // Well after the last point: 1 beat at 120 bpm is 24000 samples.
        r.run(200);
        r.touch("Drums", Control::Pan, true);
        r.set("Drums", Control::Pan, 0.0);
        r.run(8);
        r.touch("Drums", Control::Pan, false);
        let lane = &r.track("Drums").automation[0];
        assert_eq!(lane.points[0], curved, "the curve before the take is kept");
        assert!(lane.points.iter().any(|p| p.value == 0.0));
    }

    #[test]
    fn writing_over_a_switched_off_lane_starts_it_over() {
        let mut r = rig();
        let drums = r.track("Drums").id;
        let mut lane = AutomationLane::new(AutomationTarget::Pan);
        lane.enabled = false;
        lane.points = vec![AutomationPoint {
            tick: TickTime(0),
            value: -1.0,
            curve: 0.0,
            step: true,
        }];
        r.doc.execute(AddLane { track: drums, lane }).unwrap();
        r.sync();
        r.mixer
            .set_write(&mut r.doc, Some(&mut r.session), true)
            .unwrap();
        r.session.play().unwrap();
        r.run(8);
        r.touch("Drums", Control::Pan, true);
        r.set("Drums", Control::Pan, 0.5);
        r.run(8);
        r.touch("Drums", Control::Pan, false);
        let lane = &r.track("Drums").automation[0];
        assert!(lane.enabled);
        assert!(
            lane.points.iter().all(|p| p.value != -1.0),
            "the switched-off curve doesn't come back: {:?}",
            lane.points
        );
        assert_eq!(lane.points[0].value, 0.0, "static pan before the take");
    }

    fn strip(r: &Rig, name: &str) -> StripInfo {
        r.mixer
            .strips(r.doc.project(), Some(&r.session))
            .into_iter()
            .find(|s| s.name == name)
            .unwrap()
    }

    fn names(targets: &[RouteTarget]) -> Vec<&str> {
        targets.iter().map(|t| t.name.as_str()).collect()
    }

    /// The rig plus a second bus, "Delay".
    fn two_buses() -> Rig {
        let mut r = rig();
        let delay = Track::new(TrackKind::Bus, "Delay");
        r.doc
            .execute(daw_model::commands::AddTrack::new(3, delay))
            .unwrap();
        r.sync();
        r
    }

    #[test]
    fn sends_are_added_tapped_and_removed_as_commands() {
        let mut r = two_buses();
        let keys = r.id("Keys");
        assert_eq!(names(&strip(&r, "Keys").send_choices), ["Reverb", "Delay"]);
        // Drums already sends to Reverb.
        assert_eq!(names(&strip(&r, "Drums").send_choices), ["Delay"]);
        let delay = r.id("Delay");
        assert!(r.mixer.add_send(&mut r.doc, keys, delay));
        assert!(
            !r.mixer.add_send(&mut r.doc, keys, delay),
            "one send per bus"
        );
        r.sync();
        let s = strip(&r, "Keys");
        assert_eq!(s.sends.len(), 1);
        assert_eq!(s.sends[0].target_name, "Delay");
        assert!(!s.sends[0].pre_fader);
        assert_eq!(names(&s.send_choices), ["Reverb"]);

        r.mixer
            .set_send_pre_fader(&mut r.doc, keys, delay, true)
            .unwrap();
        assert!(r.track("Keys").sends[0].pre_fader);
        r.mixer
            .remove_send(&mut r.doc, Some(&mut r.session), keys, delay)
            .unwrap();
        r.sync();
        assert!(r.track("Keys").sends.is_empty());

        // Each is its own undo step.
        r.doc.undo().unwrap();
        assert!(r.track("Keys").sends[0].pre_fader);
        r.doc.undo().unwrap();
        assert!(!r.track("Keys").sends[0].pre_fader);
        r.doc.undo().unwrap();
        assert!(r.track("Keys").sends.is_empty());
        // Sends go only to buses.
        let drums = r.id("Drums");
        assert!(!r.mixer.add_send(&mut r.doc, keys, drums));
        let master = r.id("");
        assert!(!r.mixer.add_send(&mut r.doc, keys, master));
    }

    #[test]
    fn outputs_route_to_buses_and_back() {
        let mut r = two_buses();
        let keys = r.id("Keys");
        let s = strip(&r, "Keys");
        assert_eq!(s.output.as_ref().unwrap().name, "");
        assert_eq!(names(&s.output_choices), ["", "Reverb", "Delay"]);
        let delay = r.id("Delay");
        r.mixer.set_output(&mut r.doc, keys, delay).unwrap();
        r.sync();
        assert_eq!(
            r.track("Keys").output,
            daw_model::TrackOutput::Bus(r.track("Delay").id)
        );
        assert_eq!(strip(&r, "Keys").output.unwrap().name, "Delay");
        let master = r.id("");
        r.mixer.set_output(&mut r.doc, keys, master).unwrap();
        assert_eq!(r.track("Keys").output, daw_model::TrackOutput::Master);
        r.doc.undo().unwrap();
        r.doc.undo().unwrap();
        assert_eq!(r.track("Keys").output, daw_model::TrackOutput::Master);
        assert_eq!(
            r.doc.history().undo_label().as_deref(),
            Some("Add Track"),
            "two steps, both undone"
        );
        // The master has nowhere to go but the hardware.
        let m = strip(&r, "");
        assert!(m.output.is_none() && m.output_choices.is_empty() && m.send_choices.is_empty());
    }

    #[test]
    fn routing_choices_leave_out_loops() {
        let mut r = two_buses();
        let reverb = r.id("Reverb");
        let delay = r.id("Delay");
        // Reverb -> Delay, so Delay can't send or output to Reverb.
        r.mixer.set_output(&mut r.doc, reverb, delay).unwrap();
        r.sync();
        let d = strip(&r, "Delay");
        assert_eq!(names(&d.output_choices), [""]);
        assert!(d.send_choices.is_empty());
        // Reverb doesn't offer itself.
        assert_eq!(names(&strip(&r, "Reverb").output_choices), ["", "Delay"]);
        // Going around the choices is refused too, and nothing changes.
        assert!(!r.mixer.add_send(&mut r.doc, delay, reverb));
        assert!(r.mixer.set_output(&mut r.doc, delay, reverb).is_err());
        assert_eq!(r.track("Delay").output, daw_model::TrackOutput::Master);
    }
}
