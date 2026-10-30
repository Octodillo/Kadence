//! MIDI recording: arms the clip players of the armed instrument tracks (or,
//! with none armed, of the target track), collects what they capture while
//! playing, and turns the takes into one undoable edit.
//!
//! A take is overdubbed into the clip open in the piano roll when recording
//! starts inside it and every note fits; otherwise it becomes a new clip
//! spanning the bars that were played into, so nothing is lost.

use daw_core::time::{SegmentTempoMap, TempoMap, TickTime};
use daw_midi::model::{meter, midi_clip, new_note, take_command, tempo_map};
use daw_midi::record::{RecordedNote, Recorder, TakeMode, merge_take, to_clip_notes};
use daw_model::commands::{AddClip, Batch};
use daw_model::{Clip, ClipContent, ClipId, Command, MidiClip, Project, TrackId, TrackKind};

use crate::message::{Message, RecordProblem};
use crate::state::AppState;

/// One track's take in progress.
struct Take {
    track: TrackId,
    /// The clip the take goes into, if one was open on the track.
    clip: Option<ClipId>,
    recorder: Recorder,
}

/// A recording in progress: a take per track.
pub struct Recording {
    takes: Vec<Take>,
    /// The name of a new clip for a take, in the user's language.
    name: String,
    /// Whether dropped input was reported for this recording.
    dropped: bool,
    /// Where playback was when recording started.
    from: TickTime,
    /// The tempo and rate the takes' sample times were recorded under.
    tempo: SegmentTempoMap,
    sample_rate: f64,
}

/// The tracks to record on: every armed instrument track, else the
/// [`target`]. Each goes into the open clip when the playhead is in it.
fn targets(state: &AppState) -> Vec<(TrackId, Option<ClipId>)> {
    let project = state.doc.project();
    let armed: Vec<TrackId> = project
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Instrument && t.armed)
        .map(|t| t.id)
        .collect();
    if armed.is_empty() {
        return target(state).into_iter().collect();
    }
    let open = target(state).filter(|(_, clip)| clip.is_some());
    armed
        .into_iter()
        .map(|track| match open {
            Some((t, clip)) if t == track => (track, clip),
            _ => (track, None),
        })
        .collect()
}

/// The instrument track that hears live MIDI while none is armed: the one
/// recording goes to, else the selected track, else the open clip's track,
/// else the first instrument track. Unlike [`target`] it ignores the
/// playhead, so live input doesn't jump between tracks (cutting held
/// notes) as playback crosses the open clip.
pub fn input_focus(state: &AppState) -> Option<TrackId> {
    if let Some(rec) = &state.recording {
        return rec.takes.first().map(|t| t.track);
    }
    let project = state.doc.project();
    let is_instrument = |id: TrackId| {
        project
            .track(id)
            .is_some_and(|t| t.kind == TrackKind::Instrument)
    };
    let open = state
        .piano
        .clip
        .and_then(|c| project.clip_location(c))
        .map(|(track, _)| track);
    state
        .arrange
        .selected_track
        .filter(|&t| is_instrument(t))
        .or(open.filter(|&t| is_instrument(t)))
        .or_else(|| {
            project
                .tracks
                .iter()
                .find(|t| t.kind == TrackKind::Instrument)
                .map(|t| t.id)
        })
}

/// The track to record on: the open clip's track when the playhead is in
/// that clip, else the selected track, else the open clip's track, else the
/// first instrument track. Only instrument tracks qualify.
fn target(state: &AppState) -> Option<(TrackId, Option<ClipId>)> {
    let project = state.doc.project();
    let is_instrument = |id: TrackId| {
        project
            .track(id)
            .is_some_and(|t| t.kind == TrackKind::Instrument)
    };
    let playhead = state.playhead();
    let open = state
        .piano
        .clip
        .filter(|&c| midi_clip(project, c).is_some())
        .and_then(|c| Some((project.clip_location(c)?.0, project.clip(c)?)))
        .filter(|&(track, _)| is_instrument(track));
    if let Some((track, clip)) = open
        && (clip.start..TickTime(clip.start.0 + clip.length.0)).contains(&playhead)
    {
        return Some((track, Some(clip.id)));
    }
    if let Some(track) = state.arrange.selected_track
        && is_instrument(track)
    {
        return Some((track, None));
    }
    if let Some((track, _)) = open {
        return Some((track, None));
    }
    project
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Instrument)
        .map(|t| (t.id, None))
}

impl AppState {
    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    /// Arms the tracks to record on and starts playback. Fails when there
    /// is no instrument track or no running session.
    pub fn start_recording(&mut self) -> Result<(), Message> {
        if self.recording.is_some() {
            return Ok(());
        }
        let targets = targets(self);
        if targets.is_empty() {
            return Err(Message::Record(RecordProblem::NoInstrumentTrack));
        }
        let from = self.playhead();
        let sample_rate = self.sample_rate();
        let tempo = tempo_map(&self.doc.project().tempo);
        let Some(session) = self.engine.session_mut() else {
            return Err(Message::Record(RecordProblem::EngineNotRunning));
        };
        if targets
            .iter()
            .any(|&(track, _)| session.midi_player(track).is_none())
        {
            return Err(Message::Record(RecordProblem::NoMidiPlayer));
        }
        let mut takes = Vec::new();
        for (track, clip) in targets {
            let player = session.midi_player(track).expect("checked above");
            // Anything captured before arming belongs to no take.
            player.drain_captured().for_each(drop);
            player.set_armed(true);
            takes.push(Take {
                track,
                clip,
                recorder: Recorder::new(),
            });
        }
        self.recording = Some(Recording {
            takes,
            name: self.arrange.names.recording.clone(),
            dropped: false,
            from,
            tempo,
            sample_rate,
        });
        if let Some(focus) = input_focus(self)
            && let Some(session) = self.engine.session_mut()
        {
            session.set_input_focus(Some(focus));
        }
        self.engine.play();
        Ok(())
    }

    /// Feeds what the players captured into the takes. Stops recording
    /// when playback stopped.
    pub fn poll_recording(&mut self) {
        let Some(rec) = self.recording.as_mut() else {
            return;
        };
        let mut dropped = false;
        if let Some(session) = self.engine.session_mut() {
            for take in &mut rec.takes {
                let Some(player) = session.midi_player(take.track) else {
                    continue;
                };
                // A rebuild (a new bundle root, say) makes fresh, unarmed
                // players.
                if !player.is_armed() {
                    player.set_armed(true);
                }
                for c in player.drain_captured() {
                    take.recorder.feed(c);
                }
                if player.take_capture_overflows() > 0 {
                    dropped = true;
                }
            }
        }
        // Once per recording, so a flood of input can't keep the message up.
        let first_drop = dropped && !rec.dropped;
        rec.dropped |= dropped;
        if first_drop {
            self.report(Message::Record(RecordProblem::InputDropped));
        }
        if !self.engine.is_playing() {
            self.stop_recording();
        }
    }

    /// Disarms, stops playback and adds the takes as one undo step.
    pub fn stop_recording(&mut self) {
        let Some(mut rec) = self.recording.take() else {
            return;
        };
        let stop_at = self
            .engine
            .session()
            .map(|s| s.ticks_to_samples(self.playhead()));
        if let Some(session) = self.engine.session_mut() {
            for take in &mut rec.takes {
                if let Some(player) = session.midi_player(take.track) {
                    player.set_armed(false);
                    for c in player.drain_captured() {
                        take.recorder.feed(c);
                    }
                }
            }
        }
        self.engine.stop();
        let played: Vec<_> = rec
            .takes
            .iter_mut()
            .map(|take| take.recorder.finish(stop_at))
            .collect();
        if played.iter().all(Vec::is_empty) {
            return;
        }
        let mut commands: Vec<Box<dyn Command>> = rec
            .takes
            .iter()
            .zip(&played)
            .filter(|(_, notes)| !notes.is_empty())
            .filter_map(|(take, notes)| take_into(self.doc.project(), &rec, take, notes))
            .collect();
        let cmd = match commands.len() {
            0 => {
                self.report(Message::Record(RecordProblem::NothingRecorded));
                return;
            }
            1 => commands.pop().expect("one command"),
            _ => Box::new(Batch::new("Record Takes", commands)),
        };
        if let Err(e) = self.doc.execute_boxed(cmd) {
            self.report(Message::Record(RecordProblem::Refused(e)));
        }
        self.doc.seal();
    }

    /// Ends a recording without keeping it, for when the document or the
    /// engine is about to be replaced.
    pub fn discard_recording(&mut self) {
        let Some(rec) = self.recording.take() else {
            return;
        };
        if let Some(session) = self.engine.session_mut() {
            for take in &rec.takes {
                if let Some(player) = session.midi_player(take.track) {
                    player.set_armed(false);
                    player.drain_captured().for_each(drop);
                }
            }
        }
    }
}

/// The command that adds the notes of `take` to the project: overdubbed
/// into its clip when it has one, else as a new clip on its track.
fn take_into(
    project: &Project,
    rec: &Recording,
    take: &Take,
    take_notes: &[RecordedNote],
) -> Option<Box<dyn Command>> {
    let (tempo, sample_rate) = (&rec.tempo, rec.sample_rate);
    let in_clip = take.clip.and_then(|c| project.clip(c)).filter(|clip| {
        let end = clip.start.0 + clip.length.0;
        take_notes.iter().all(|n| {
            tempo.samples_to_ticks(n.start, sample_rate) >= clip.start
                && tempo.samples_to_ticks(n.end, sample_rate).0 <= end
        })
    });
    match in_clip {
        Some(clip) => {
            let range = clip.start..TickTime(clip.start.0 + clip.length.0);
            let notes = to_clip_notes(take_notes, tempo, sample_rate, range);
            let existing = midi_clip(project, clip.id).map_or(&[][..], |m| &m.notes);
            let span = TickTime(rec.from.0 - clip.start.0)..TickTime(i64::MAX);
            let plan = merge_take(existing, notes, TakeMode::Overdub, span);
            take_command(clip.id, existing, plan)
        }
        None => {
            let (start, end) = take_span(project, take_notes, tempo, sample_rate);
            let mut notes: Vec<_> = to_clip_notes(take_notes, tempo, sample_rate, start..end)
                .into_iter()
                .map(new_note)
                .collect();
            if notes.is_empty() {
                return None;
            }
            notes.sort_by_key(|n| (n.start, n.key, n.id));
            let clip = Clip::new(
                rec.name.clone(),
                start,
                TickTime(end.0 - start.0),
                ClipContent::Midi(MidiClip { notes }),
            );
            Some(Box::new(AddClip::new(take.track, clip)))
        }
    }
}

/// Whole bars covering the take.
fn take_span(
    project: &Project,
    take: &[RecordedNote],
    tempo: &dyn TempoMap,
    sample_rate: f64,
) -> (TickTime, TickTime) {
    let first = take.iter().map(|n| n.start).min().unwrap_or_default();
    let last = take.iter().map(|n| n.end).max().unwrap_or_default();
    let first = tempo.samples_to_ticks(first, sample_rate).max(TickTime(0));
    let last = tempo.samples_to_ticks(last, sample_rate);
    let m = meter(&project.tempo);
    let start = m.bar_at(first).start;
    let end_bar = m.bar_at(last);
    let end = if end_bar.start == last {
        last
    } else {
        end_bar.end()
    };
    (start, end.max(TickTime(start.0 + 1)))
}

#[cfg(test)]
mod tests {
    use daw_core::time::{PPQ, SampleTime};
    use daw_model::commands::AddTrack;
    use daw_model::{Document, Track};

    use super::*;

    const SR: f64 = 48_000.0;

    /// At the default 120 bpm a beat is half a second.
    fn beat(b: f64) -> SampleTime {
        SampleTime((b * SR / 2.0) as i64)
    }

    fn note(from: f64, to: f64, key: u8) -> RecordedNote {
        RecordedNote {
            start: beat(from),
            end: beat(to),
            key,
            channel: 0,
            velocity: 0.5,
        }
    }

    fn recording(track: TrackId, clip: Option<ClipId>) -> (Recording, Take) {
        let rec = Recording {
            takes: Vec::new(),
            name: "Recording".into(),
            dropped: false,
            from: TickTime(0),
            tempo: SegmentTempoMap::default(),
            sample_rate: SR,
        };
        let take = Take {
            track,
            clip,
            recorder: Recorder::new(),
        };
        (rec, take)
    }

    fn doc() -> (Document, TrackId) {
        let mut doc = Document::new(Project::new("t"));
        let track = Track::new(TrackKind::Instrument, "keys");
        let id = track.id;
        doc.execute(AddTrack::new(0, track)).unwrap();
        (doc, id)
    }

    #[test]
    fn a_take_without_a_clip_becomes_a_clip_of_whole_bars() {
        let (mut doc, track) = doc();
        let (rec, one) = recording(track, None);
        // Beat 5 to 6.5: all inside bar 2 in 4/4.
        let take = [note(5.0, 6.5, 60), note(5.0, 5.5, 64)];
        let cmd = take_into(doc.project(), &rec, &one, &take).unwrap();
        doc.execute_boxed(cmd).unwrap();
        let clip = &doc.project().track(track).unwrap().clips[0];
        assert_eq!(
            (clip.start, clip.length),
            (TickTime(4 * PPQ), TickTime(4 * PPQ))
        );
        let notes = &midi_clip(doc.project(), clip.id).unwrap().notes;
        let got: Vec<_> = notes
            .iter()
            .map(|n| (n.start.0, n.length.0, n.key))
            .collect();
        assert_eq!(got, [(PPQ, 3 * PPQ / 2, 60), (PPQ, PPQ / 2, 64)]);
        doc.undo().unwrap();
        assert!(doc.project().track(track).unwrap().clips.is_empty());
    }

    #[test]
    fn a_take_overdubs_the_open_clip() {
        let (mut doc, track) = doc();
        let existing = Clip::new(
            "part",
            TickTime(0),
            TickTime(4 * PPQ),
            ClipContent::Midi(MidiClip {
                notes: vec![new_note(daw_midi::NoteData::new(0, PPQ, 48, 0.8))],
            }),
        );
        let clip_id = existing.id;
        doc.execute(AddClip::new(track, existing)).unwrap();
        let (rec, one) = recording(track, Some(clip_id));
        let take = [note(1.0, 2.0, 60)];
        let cmd = take_into(doc.project(), &rec, &one, &take).unwrap();
        doc.execute_boxed(cmd).unwrap();
        let keys: Vec<u8> = midi_clip(doc.project(), clip_id)
            .unwrap()
            .notes
            .iter()
            .map(|n| n.key)
            .collect();
        assert_eq!(keys, [48, 60]);
        assert_eq!(doc.history().undo_label().as_deref(), Some("Record Notes"));
        doc.undo().unwrap();

        // A note past the clip's end: the whole take becomes a new clip
        // rather than losing that note.
        let take = [note(1.0, 2.0, 60), note(5.0, 6.0, 62)];
        let cmd = take_into(doc.project(), &rec, &one, &take).unwrap();
        doc.execute_boxed(cmd).unwrap();
        let clips = &doc.project().track(track).unwrap().clips;
        assert_eq!(clips.len(), 2);
        assert_eq!(midi_clip(doc.project(), clip_id).unwrap().notes.len(), 1);
        assert_eq!(
            midi_clip(doc.project(), clips[1].id).unwrap().notes.len(),
            2
        );
    }
}
