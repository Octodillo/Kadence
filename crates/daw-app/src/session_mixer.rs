//! The mixer on the open project.
//!
//! [`SessionMixer`] turns the project's tracks into channel strips and turns
//! strip edits into model commands, so every mixer move undoes like any
//! other edit and is saved with the project. Meters come from the running
//! [`Session`]. [`AppMixer`] is the same mixer on the app state, as the Mix
//! page uses it.
//!
//! Touch writing: with [`write automation`](SessionMixer::set_write) on, a
//! fader, pan or send grabbed during playback is recorded with an
//! [`AutomationRecorder`] instead of changing the control's static value.
//! While held, the control's existing lane is left out of playback so the
//! user hears the hand, not the old curve. On release the take is merged
//! into the lane (creating it if needed) as one undoable edit.

use std::collections::HashSet;
use std::sync::Arc;

use crate::instrument::{InstrumentChoice, effects_start, instrument_of, set_instrument};
use crate::message::Message;
use daw_core::time::{SampleTime, TickTime};
use daw_engine::nodes::Strip;
use daw_engine::{AutomationRecorder, Curve, NodeKey};
use daw_model::commands::{
    AddDevice, AddLane, Batch, LaneField, RemoveDevice, SendField, SetLane, SetSend, SetTrack,
    TrackField,
};
use daw_model::{
    AutomationLane, AutomationPoint, AutomationTarget, Command, Device, DeviceSource, Document,
    LaneId, Project, SendId, Track, TrackId, TrackKind,
};
use daw_session::{Session, db_to_gain, effective_mutes};

use crate::mixer::{
    Control, EffectChoice, InsertInfo, MAX_DB, MixerHandle, SILENT_DB, SendInfo, StripId,
    StripInfo, StripKind, StripLevels, effect_choices,
};
use crate::state;

/// The fader's bottom stored as a level the session plays as silence.
const SILENCE_DB: f64 = -144.0;

/// How far (in the control's unit) a thinned take may stray from what the
/// hand did.
fn tolerance(control: Control) -> f64 {
    match control {
        Control::Pan => 0.005,
        Control::Volume | Control::Send(_) => 0.2,
    }
}

fn strip_id(track: TrackId) -> StripId {
    track.0.as_u128()
}

/// A fader position as the level stored in the model.
fn fader_db(db: f64) -> f64 {
    if db <= SILENT_DB {
        SILENCE_DB
    } else {
        db.min(MAX_DB)
    }
}

/// A control being recorded.
struct Take {
    track: TrackId,
    control: Control,
    target: AutomationTarget,
    recorder: AutomationRecorder,
    /// Where the hand is now, in the control's unit.
    value: f64,
}

/// The mixer on a [`Document`] and the [`Session`] playing it. See the
/// module docs.
///
/// Methods that edit the document leave syncing the session to the caller
/// (the app state does it after every call). Only touch writing syncs the
/// session itself, since it changes playback without changing the document.
pub struct SessionMixer {
    write: bool,
    takes: Vec<Take>,
    effects: Vec<EffectChoice>,
}

impl Default for SessionMixer {
    fn default() -> Self {
        Self {
            write: false,
            takes: Vec::new(),
            effects: effect_choices(),
        }
    }
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
        device_id,
        name: d.name.clone(),
        has_gain_reduction: session.is_some_and(|s| s.device_meter(d.id).is_some()),
    }
}

impl SessionMixer {
    /// The strips of `project`. Gain reduction meters show once `session`
    /// plays the project.
    pub fn strips(&self, project: &Project, session: Option<&Session>) -> Vec<StripInfo> {
        let silenced = effective_mutes(project);
        let held = |track: TrackId, control: Control| {
            self.takes
                .iter()
                .find(|t| t.track == track && t.control == control)
                .map(|t| t.value)
        };
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
                    .max(SILENT_DB),
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
                                .max(SILENT_DB),
                        }
                    })
                    .collect(),
            })
            .collect()
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
            Control::Volume | Control::Send(_) => fader_db(value),
            Control::Pan => value.clamp(-1.0, 1.0),
        };
        if let Some(take) = self
            .takes
            .iter_mut()
            .find(|t| t.track == track.id && t.control == control)
        {
            take.value = value;
            if let Some(s) = session {
                record(take, s);
                if let Some((key, param, v)) = live_param(doc.project(), s, take) {
                    s.set_live_param(key, param, Some(v))
                        .map_err(Message::from)?;
                }
            }
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
        let Some(track) = find(doc.project(), id) else {
            return false;
        };
        let device = Device::new(
            effect.name.clone(),
            DeviceSource::Builtin {
                kind: effect.id.clone(),
            },
        );
        let command = AddDevice {
            track: track.id,
            index: track.devices.len(),
            device,
        };
        let added = doc.execute(command).is_ok();
        doc.seal();
        added
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

    pub fn write(&self) -> bool {
        self.write
    }

    /// Turns touch writing on or off. Turning it off ends every take.
    pub fn set_write(
        &mut self,
        doc: &mut Document,
        session: Option<&mut Session>,
        on: bool,
    ) -> Result<(), Message> {
        self.write = on;
        if on {
            return Ok(());
        }
        self.finish_all(doc, session)
    }

    /// The user grabbed or let go of a control. Grabbing starts a take when
    /// writing is on and the transport plays; letting go ends it.
    pub fn touch(
        &mut self,
        doc: &mut Document,
        session: Option<&mut Session>,
        id: StripId,
        control: Control,
        pressed: bool,
    ) -> Result<(), Message> {
        let Some(session) = session else {
            return Ok(());
        };
        let Some(track) = find(doc.project(), id).cloned() else {
            return Ok(());
        };
        let running = self
            .takes
            .iter()
            .position(|t| t.track == track.id && t.control == control);
        match (pressed, running) {
            (true, None) if self.write && session.is_playing() => {
                let Some((target, value)) = target_of(&track, control) else {
                    return Ok(());
                };
                // Start from what is heard: the lane's value, if it has one.
                let value = lane_of(&track, target)
                    .filter(|l| l.enabled)
                    .and_then(|l| {
                        session
                            .engine_lane(&l.points)
                            .value_at(session.controller().position())
                    })
                    .unwrap_or(value);
                let mut take = Take {
                    track: track.id,
                    control,
                    target,
                    recorder: AutomationRecorder::new(tolerance(control)),
                    value,
                };
                record(&mut take, session);
                self.takes.push(take);
                self.suspend(doc, session)?;
                if let Some((key, param, v)) =
                    live_param(doc.project(), session, self.takes.last().expect("pushed"))
                {
                    session
                        .set_live_param(key, param, Some(v))
                        .map_err(Message::from)?;
                }
                Ok(())
            }
            (false, running) => {
                let mut r = Ok(());
                if let Some(i) = running {
                    let take = self.takes.remove(i);
                    r = self.end(doc, session, take);
                }
                // Letting go ends the gesture, written or not: the next drag
                // is its own undo step.
                doc.seal();
                r
            }
            _ => Ok(()),
        }
    }

    /// Ends a take: writes it, lets the document's value play again and
    /// resumes its lane.
    fn end(
        &mut self,
        doc: &mut Document,
        session: &mut Session,
        take: Take,
    ) -> Result<(), Message> {
        let live = live_param(doc.project(), session, &take);
        let r = commit(doc, session, take);
        if let Some((key, param, _)) = live {
            session
                .set_live_param(key, param, None)
                .map_err(Message::from)?;
        }
        self.suspend(doc, session)?;
        r
    }

    /// Records held controls on the clock. A take ends when the transport
    /// stops, and starts over when the play position jumps back (a loop
    /// wrapped, the user located).
    pub fn tick(
        &mut self,
        doc: &mut Document,
        session: Option<&mut Session>,
    ) -> Result<(), Message> {
        let Some(session) = session else {
            return Ok(());
        };
        if self.takes.is_empty() {
            return Ok(());
        }
        if !session.is_playing() {
            return self.finish_all(doc, Some(session));
        }
        let now = session.controller().position();
        let mut result = Ok(());
        let mut wrote = false;
        for i in 0..self.takes.len() {
            let take = &mut self.takes[i];
            let behind = take.recorder.range().is_some_and(|(_, end)| now < end);
            if behind {
                // Close the take at the jump and carry on with a new one.
                let next = Take {
                    track: take.track,
                    control: take.control,
                    target: take.target,
                    recorder: AutomationRecorder::new(tolerance(take.control)),
                    value: take.value,
                };
                let done = std::mem::replace(take, next);
                if let Err(e) = commit(doc, session, done) {
                    result = Err(e);
                }
                wrote = true;
            }
            record(&mut self.takes[i], session);
        }
        if wrote {
            // A lane written just now must not drown out the hand that is
            // still on the control.
            self.suspend(doc, session)?;
        }
        result
    }

    fn finish_all(
        &mut self,
        doc: &mut Document,
        session: Option<&mut Session>,
    ) -> Result<(), Message> {
        let Some(session) = session else {
            self.takes.clear();
            return Ok(());
        };
        let mut result = Ok(());
        while let Some(take) = self.takes.pop() {
            if let Err(e) = self.end(doc, session, take) {
                result = Err(e);
            }
        }
        doc.seal();
        result
    }

    /// Leaves the lanes of running takes out of playback.
    fn suspend(&self, doc: &mut Document, session: &mut Session) -> Result<(), Message> {
        let project = doc.project();
        let lanes: HashSet<LaneId> = self
            .takes
            .iter()
            .filter_map(|t| lane_of(project.track(t.track)?, t.target).map(|l| l.id))
            .collect();
        if &lanes == session.suspended_lanes() {
            return Ok(());
        }
        session.set_suspended_lanes(lanes);
        session.sync(&doc.snapshot()).map_err(Message::from)
    }
}

/// The automation target of a control and its static value.
fn target_of(track: &Track, control: Control) -> Option<(AutomationTarget, f64)> {
    match control {
        Control::Volume => Some((AutomationTarget::Volume, track.volume_db)),
        Control::Pan if track.kind == TrackKind::Master => None,
        Control::Pan => Some((AutomationTarget::Pan, track.pan)),
        Control::Send(target) => {
            let send = track.sends.iter().find(|s| strip_id(s.target) == target)?;
            Some((AutomationTarget::SendLevel { send: send.id }, send.level_db))
        }
    }
}

fn lane_of(track: &Track, target: AutomationTarget) -> Option<&AutomationLane> {
    track.automation.iter().find(|l| l.target == target)
}

/// The engine parameter a take drives, and its value there.
fn live_param(project: &Project, session: &Session, take: &Take) -> Option<(NodeKey, u32, f64)> {
    match take.target {
        AutomationTarget::Volume => Some((
            session.strip_key(take.track)?,
            Strip::GAIN,
            db_to_gain(take.value),
        )),
        AutomationTarget::Pan => Some((session.strip_key(take.track)?, Strip::PAN, take.value)),
        AutomationTarget::SendLevel { send } => {
            project.send_location(send)?;
            Some((Session::send_key(send), Strip::GAIN, db_to_gain(take.value)))
        }
        AutomationTarget::DeviceParam { .. } => None,
    }
}

fn record(take: &mut Take, session: &Session) {
    take.recorder
        .record(session.controller().position(), take.value);
}

/// Merges a finished take into its lane, creating the lane if needed.
fn commit(doc: &mut Document, session: &Session, take: Take) -> Result<(), Message> {
    if take.recorder.is_empty() {
        return Ok(());
    }
    let project = doc.project();
    let Some(track) = project.track(take.track) else {
        return Ok(());
    };
    let Some((_, static_value)) = target_of(track, take.control) else {
        return Ok(());
    };
    let existing = lane_of(track, take.target);
    let base = match existing {
        Some(l) if l.enabled && !l.points.is_empty() => session.engine_lane(&l.points),
        // A new lane, or one that was switched off (and so not heard while
        // writing), starts over from the static value around the take.
        _ => daw_engine::AutomationLane::new(vec![daw_engine::AutomationPoint {
            time: SampleTime(0),
            value: static_value,
            curve: Curve::Step,
        }]),
    };
    let merged = take.recorder.finish(&base);
    // Points the take left alone keep their exact tick and curve shape,
    // which the engine's lane doesn't carry.
    let kept: Vec<&AutomationPoint> = existing
        .filter(|l| l.enabled)
        .map(|l| l.points.iter().collect())
        .unwrap_or_default();
    let mut points: Vec<AutomationPoint> = Vec::with_capacity(merged.points().len());
    for p in merged.points() {
        if let Some(&orig) = kept
            .iter()
            .find(|o| session.ticks_to_samples(o.tick) == p.time && o.value == p.value)
            && points.last().is_none_or(|prev| prev.tick < orig.tick)
        {
            points.push(orig.clone());
            continue;
        }
        let mut tick = session.samples_to_ticks(p.time);
        // Samples finer than a tick: keep points apart and in order.
        if let Some(prev) = points.last() {
            tick = TickTime(tick.0.max(prev.tick.0 + 1));
        }
        points.push(AutomationPoint {
            tick,
            value: p.value,
            curve: 0.0,
            step: p.curve == Curve::Step,
        });
    }
    let command: Box<dyn Command> = match existing {
        Some(l) => {
            let mut edits: Vec<Box<dyn Command>> = vec![Box::new(SetLane {
                id: l.id,
                field: LaneField::Points(points),
            })];
            if !l.enabled {
                edits.push(Box::new(SetLane {
                    id: l.id,
                    field: LaneField::Enabled(true),
                }));
            }
            Box::new(Batch::new("Write Automation", edits))
        }
        None => {
            let mut lane = AutomationLane::new(take.target);
            lane.points = points;
            Box::new(AddLane {
                track: take.track,
                lane,
            })
        }
    };
    doc.execute_boxed(command)
        .map(|_| ())
        .map_err(Message::from)
}

/// [`SessionMixer`] on the app state: the mixer the Mix page shows.
#[derive(Default)]
pub struct AppMixer {
    inner: SessionMixer,
}

impl AppMixer {
    /// Runs `f`, then plays any document change and reports errors on the
    /// status line.
    fn edit<R>(
        &mut self,
        f: impl FnOnce(&mut SessionMixer, &mut Document, Option<&mut Session>) -> R,
    ) -> R {
        state::with(|s| {
            s.end_drag();
            let r = f(&mut self.inner, &mut s.doc, s.engine.session_mut());
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

impl MixerHandle for AppMixer {
    fn strips(&self) -> Vec<StripInfo> {
        state::with(|s| {
            let mut strips = self.inner.strips(s.doc.project(), s.engine.session());
            for (strip, track) in strips.iter_mut().zip(ordered(s.doc.project())) {
                strip.instrument_choice =
                    instrument_of(track).and_then(|d| s.instruments.position(&d.source));
            }
            strips
        })
    }

    fn set_volume(&mut self, id: StripId, db: f64) {
        self.report(|m, d, s| m.set(d, s, id, Control::Volume, db));
    }

    fn set_pan(&mut self, id: StripId, pan: f64) {
        self.report(|m, d, s| m.set(d, s, id, Control::Pan, pan));
    }

    fn set_muted(&mut self, id: StripId, muted: bool) {
        self.report(|m, d, _| m.set_muted(d, id, muted));
    }

    fn set_soloed(&mut self, id: StripId, soloed: bool) {
        self.report(|m, d, _| m.set_soloed(d, id, soloed));
    }

    fn set_send_level(&mut self, id: StripId, target: StripId, db: f64) {
        self.report(|m, d, s| m.set(d, s, id, Control::Send(target), db));
    }

    fn add_insert(&mut self, id: StripId, device_id: &str) -> bool {
        self.edit(|m, d, _| m.add_insert(d, id, device_id))
    }

    fn remove_insert(&mut self, id: StripId, index: usize) {
        self.report(|m, d, _| m.remove_insert(d, id, index));
    }

    fn effects(&self) -> Vec<EffectChoice> {
        self.inner.effects()
    }

    fn set_instrument(&mut self, id: StripId, choice: usize) {
        let Some(choice) = state::with(|s| s.instruments.get(choice).cloned()) else {
            return;
        };
        self.report(|m, d, _| m.set_instrument(d, id, &choice));
    }

    fn levels(&mut self) -> Vec<StripLevels> {
        state::with(|s| self.inner.levels(s.doc.project(), s.engine.session()))
    }

    fn tick(&mut self, _playing: bool, _seconds: f64) {
        if !self.inner.takes.is_empty() {
            self.report(|m, d, s| m.tick(d, s));
        }
    }

    fn revision(&self) -> u64 {
        // Strips depend only on the document; a new document restarts its
        // revision count, so its serial goes in the high bits.
        state::with(|s| (s.doc_serial << 40) ^ s.doc.revision())
    }

    fn set_write_automation(&mut self, on: bool) {
        self.report(|m, d, s| m.set_write(d, s, on));
    }

    fn write_automation(&self) -> bool {
        self.inner.write()
    }

    fn touch(&mut self, id: StripId, control: Control, pressed: bool) {
        self.report(|m, d, s| m.touch(d, s, id, control, pressed));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daw_core::time::PPQ;
    use daw_engine::EngineProcessor;
    use daw_model::Send;
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
    }

    #[test]
    fn edits_are_commands_that_undo() {
        let mut r = rig();
        r.set("Drums", Control::Volume, -12.0);
        r.set("Drums", Control::Volume, -70.0);
        assert_eq!(r.track("Drums").volume_db, SILENCE_DB);
        assert_eq!(
            r.mixer.strips(r.doc.project(), Some(&r.session))[0].volume_db,
            SILENT_DB
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
}
