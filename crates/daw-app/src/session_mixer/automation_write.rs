//! Touch writing: recording a held control as automation.
//!
//! The part of [`SessionMixer`] that runs takes. Grabbing a control starts
//! a [`Take`] if writing is armed for it and the transport is playing. The
//! clock records the take ([`SessionMixer::tick`]), and letting go merges
//! it into the control's lane as one undoable edit. See the parent
//! module's docs.

use std::collections::HashSet;

use daw_arrange::ParamCatalog;
use daw_core::time::{SampleTime, TickTime};
use daw_engine::nodes::Strip;
use daw_engine::{AutomationRecorder, Curve, NodeKey};
use daw_model::commands::{AddLane, Batch, LaneField, SetLane};
use daw_model::{
    AutomationLane, AutomationPoint, AutomationTarget, Command, Document, LaneId, Project, Track,
    TrackId, TrackKind,
};
use daw_session::{Session, db_to_gain};

use super::{SessionMixer, find, strip_id};
use crate::device_panel::BuiltinParams;
use crate::double_click::Clicks;
use crate::message::Message;
use crate::mixer::{Control, StripId};

/// How far (in the control's unit) a thinned take may stray from what the
/// hand did.
fn tolerance(control: Control) -> f64 {
    match control {
        Control::Pan => 0.005,
        Control::Volume | Control::Send(_) => 0.2,
        // Plain values of any range: thin only what is flat, unless the
        // caller knows the range (see [`ParamStart`]).
        Control::Param { .. } => 1e-6,
    }
}

/// What the device panel knows about a parameter it starts recording: the
/// value it shows (a plugin's own value, when the document holds none) and
/// how far a take of it may stray, from the parameter's range.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamStart {
    pub value: f64,
    pub tolerance: f64,
}

/// A control being recorded.
pub(super) struct Take {
    track: TrackId,
    control: Control,
    target: AutomationTarget,
    recorder: AutomationRecorder,
    /// Where the hand is now, in the control's unit.
    value: f64,
    /// The control's static value when the document holds none.
    fallback: Option<f64>,
    tolerance: f64,
    /// How often the hand moved the control since the take started (a
    /// take moved only once is a click).
    moves: u32,
}

impl SessionMixer {
    fn take_of(&self, track: TrackId, control: Control) -> Option<usize> {
        self.takes
            .iter()
            .position(|t| t.track == track && t.control == control)
    }

    /// Where the hand holds `control`, while a take for it runs.
    pub(super) fn held_value(&self, track: TrackId, control: Control) -> Option<f64> {
        self.take_of(track, control).map(|i| self.takes[i].value)
    }

    /// Ends the take for `control`, if one runs, writing it when a session
    /// plays (the control is about to go away).
    pub(super) fn end_control(
        &mut self,
        doc: &mut Document,
        session: Option<&mut Session>,
        track: TrackId,
        control: Control,
    ) -> Result<(), Message> {
        let Some(i) = self.take_of(track, control) else {
            return Ok(());
        };
        let take = self.takes.remove(i);
        match session {
            Some(session) => self.end(doc, session, take),
            None => Ok(()),
        }
    }

    /// Records a move of `control` if a take for it runs. `false` means no
    /// take does, and the move is an ordinary edit.
    pub(super) fn record_move(
        &mut self,
        project: &Project,
        session: Option<&mut Session>,
        track: TrackId,
        control: Control,
        value: f64,
    ) -> Result<bool, Message> {
        let Some(i) = self.take_of(track, control) else {
            return Ok(false);
        };
        let take = &mut self.takes[i];
        take.value = value;
        take.moves += 1;
        if let Some(s) = session {
            record(take, s);
            if let Some((key, param, v)) = live_param(project, s, take) {
                s.set_live_param(key, param, Some(v))
                    .map_err(Message::from)?;
            }
        }
        Ok(true)
    }

    /// Lets go of every device parameter a take records (the device
    /// panel's controls were released, or the panel closed). A take the
    /// hand moved only once was a click, not a gesture: it is dropped
    /// unwritten and returned, with the value clicked, for the caller to
    /// set as the parameter's static value.
    pub fn release_params(
        &mut self,
        doc: &mut Document,
        mut session: Option<&mut Session>,
    ) -> Result<Vec<(Control, f64)>, Message> {
        let mut clicks = Vec::new();
        let mut r = Ok(());
        while let Some(i) = self
            .takes
            .iter()
            .position(|t| matches!(t.control, Control::Param { .. }))
        {
            let take = self.takes.remove(i);
            let Some(session) = session.as_deref_mut() else {
                continue;
            };
            let done = if take.moves <= 1 {
                clicks.push((take.control, take.value));
                self.drop_take(doc, session, take)
            } else {
                self.end(doc, session, take)
            };
            r = r.and(done);
        }
        doc.seal();
        r.map(|()| clicks)
    }

    /// Whether a take is running.
    pub fn recording(&self) -> bool {
        !self.takes.is_empty()
    }

    /// Whether a take for `control` of strip `id` is running.
    pub fn recording_control(&self, id: StripId, control: Control) -> bool {
        self.takes
            .iter()
            .any(|t| strip_id(t.track) == id && t.control == control)
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
        if pressed {
            // A press is its own undo step, apart from wheel or key edits.
            doc.seal();
            self.held = Some(((id, control), doc.revision(), doc.history().undo_len()));
        }
        let r = self.touch_with(doc, session, id, control, pressed, None);
        if !pressed {
            if let Some((key, before, undo_len)) = self.held.take()
                && key == (id, control)
            {
                self.clicks = Clicks::after_edit(self.clicks, key, before, undo_len, doc);
            }
            // Also without a session, the next edit is its own step.
            doc.seal();
        }
        r
    }

    /// Puts a control back to its default (0 dB, centre) as one undo step,
    /// written into automation like a touch while writing. The edits of a
    /// double-click's clicks on it just before are taken back.
    pub fn reset(
        &mut self,
        doc: &mut Document,
        mut session: Option<&mut Session>,
        id: StripId,
        control: Control,
    ) -> Result<(), Message> {
        let value = match control {
            Control::Volume | Control::Pan | Control::Send(_) => 0.0,
            Control::Param { .. } => return Ok(()),
        };
        Clicks::take_back(self.clicks.take(), (id, control), doc).map_err(Message::from)?;
        doc.seal();
        self.touch_with(doc, session.as_deref_mut(), id, control, true, None)?;
        let r = self.set(doc, session.as_deref_mut(), id, control, value);
        let ended = self.touch_with(doc, session, id, control, false, None);
        doc.seal();
        r.and(ended)
    }

    /// Grabs a device parameter, as [`touch`](Self::touch) does, with what
    /// the device panel knows about it.
    pub fn touch_param(
        &mut self,
        doc: &mut Document,
        session: Option<&mut Session>,
        id: StripId,
        control: Control,
        start: ParamStart,
    ) -> Result<(), Message> {
        self.touch_with(doc, session, id, control, true, Some(start))
    }

    fn touch_with(
        &mut self,
        doc: &mut Document,
        session: Option<&mut Session>,
        id: StripId,
        control: Control,
        pressed: bool,
        start: Option<ParamStart>,
    ) -> Result<(), Message> {
        let fallback = start.map(|s| s.value);
        let Some(session) = session else {
            return Ok(());
        };
        let Some(track) = find(doc.project(), id).cloned() else {
            return Ok(());
        };
        let running = self.take_of(track.id, control);
        let armed = |m: &Self| {
            m.write
                || target_of(&track, control, fallback)
                    .and_then(|(target, _)| track.lane_for(target))
                    .is_some_and(|l| l.enabled && m.write_lanes.contains(&l.id))
        };
        match (pressed, running) {
            (true, None) if armed(self) && session.is_playing() => {
                let Some((target, value)) = target_of(&track, control, fallback) else {
                    return Ok(());
                };
                // Start from what is heard: the lane's value, if it has one.
                let now = session.samples_to_ticks(session.controller().position());
                let value = track
                    .lane_for(target)
                    .filter(|l| l.enabled)
                    .and_then(|l| l.value_at(now))
                    .unwrap_or(value);
                let tolerance = start.map_or(tolerance(control), |s| s.tolerance);
                let mut take = Take {
                    track: track.id,
                    control,
                    target,
                    recorder: AutomationRecorder::new(tolerance),
                    value,
                    fallback,
                    tolerance,
                    moves: 0,
                };
                record(&mut take, session);
                // Hold the take's value before the lane stops playing, so
                // the graph without it never plays the static value.
                if let Some((key, param, v)) = live_param(doc.project(), session, &take)
                    && let Err(e) = session.set_live_param(key, param, Some(v))
                {
                    // No take holds it, so don't leave it held.
                    let _ = session.set_live_param(key, param, None);
                    return Err(Message::from(e));
                }
                self.takes.push(take);
                self.suspend(doc, session)
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

    /// Ends a take without writing it.
    fn drop_take(
        &mut self,
        doc: &mut Document,
        session: &mut Session,
        take: Take,
    ) -> Result<(), Message> {
        if let Some((key, param, _)) = live_param(doc.project(), session, &take) {
            session
                .set_live_param(key, param, None)
                .map_err(Message::from)?;
        }
        self.suspend(doc, session)
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
                    recorder: AutomationRecorder::new(take.tolerance),
                    value: take.value,
                    fallback: take.fallback,
                    tolerance: take.tolerance,
                    // The gesture goes on: letting go writes it.
                    moves: take.moves.max(2),
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
            .filter_map(|t| project.track(t.track)?.lane_for(t.target).map(|l| l.id))
            .collect();
        if &lanes == session.suspended_lanes() {
            return Ok(());
        }
        session.set_suspended_lanes(lanes);
        session.sync(&doc.snapshot()).map_err(Message::from)
    }
}

/// The automation target of a control and its static value. `fallback` is
/// a device parameter's value when neither the document nor the built-in
/// device's defaults hold one (a plugin's own value).
fn target_of(
    track: &Track,
    control: Control,
    fallback: Option<f64>,
) -> Option<(AutomationTarget, f64)> {
    match control {
        Control::Volume => Some((AutomationTarget::Volume, track.volume_db)),
        Control::Pan if track.kind == TrackKind::Master => None,
        Control::Pan => Some((AutomationTarget::Pan, track.pan)),
        Control::Send(target) => {
            let send = track.sends.iter().find(|s| strip_id(s.target) == target)?;
            Some((AutomationTarget::SendLevel { send: send.id }, send.level_db))
        }
        Control::Param { device, param } => {
            let d = track.devices.iter().find(|d| d.id == device)?;
            let value = d
                .params
                .get(&param)
                .copied()
                .or_else(|| BuiltinParams.default_value(d, param));
            let value = value.or(fallback)?;
            Some((AutomationTarget::DeviceParam { device, param }, value))
        }
    }
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
        AutomationTarget::DeviceParam { device, param } => {
            project.device_location(device)?;
            Some((Session::device_key(device), param, take.value))
        }
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
    let Some((_, static_value)) = target_of(track, take.control, take.fallback) else {
        return Ok(());
    };
    let existing = track.lane_for(take.target);
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
