//! MIDI recording: arms the target track's clip player, collects what it
//! captures while playing, and turns the take into one undoable edit.
//!
//! The take is overdubbed into the clip open in the piano roll when
//! recording starts inside it and every note fits; otherwise it becomes a
//! new clip spanning the bars that were played into, so nothing is lost.

use daw_core::time::{SegmentTempoMap, TempoMap, TickTime};
use daw_midi::model::{meter, midi_clip, new_note, take_command, tempo_map};
use daw_midi::record::{RecordedNote, Recorder, TakeMode, merge_take, to_clip_notes};
use daw_model::commands::AddClip;
use daw_model::{Clip, ClipContent, ClipId, Command, MidiClip, Project, TrackId, TrackKind};

use crate::message::{Message, RecordProblem};
use crate::state::AppState;

/// A take in progress.
pub struct Recording {
    track: TrackId,
    /// The clip the take goes into, if one was open on the track.
    clip: Option<ClipId>,
    /// The name of a new clip for the take, in the user's language.
    name: String,
    /// Whether dropped input was reported for this take.
    dropped: bool,
    recorder: Recorder,
    /// Where playback was when recording started.
    from: TickTime,
    /// The tempo and rate the take's sample times were recorded under.
    tempo: SegmentTempoMap,
    sample_rate: f64,
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

    /// Arms the target track and starts playback, after a count-in when one
    /// is set. Fails when there is no instrument track or no running
    /// session.
    pub fn start_recording(&mut self) -> Result<(), Message> {
        if self.recording.is_some() {
            return Ok(());
        }
        let Some((track, clip)) = target(self) else {
            return Err(Message::Record(RecordProblem::NoInstrumentTrack));
        };
        let from = self.playhead();
        let sample_rate = self.sample_rate();
        let tempo = tempo_map(&self.doc.project().tempo);
        let Some(session) = self.engine.session_mut() else {
            return Err(Message::Record(RecordProblem::EngineNotRunning));
        };
        let Some(player) = session.midi_player(track) else {
            return Err(Message::Record(RecordProblem::NoMidiPlayer));
        };
        // Anything captured before arming belongs to no take.
        player.drain_captured().for_each(drop);
        player.set_armed(true);
        self.recording = Some(Recording {
            track,
            clip,
            name: self.arrange.names.recording.clone(),
            dropped: false,
            recorder: Recorder::new(),
            from,
            tempo,
            sample_rate,
        });
        // Capture waits for the count-in: the player only records while
        // the transport moves.
        self.play_for_recording();
        Ok(())
    }

    /// Feeds what the player captured into the take. Stops recording when
    /// playback stopped.
    pub fn poll_recording(&mut self) {
        let Some(rec) = self.recording.as_mut() else {
            return;
        };
        let mut dropped = false;
        if let Some(player) = self
            .engine
            .session_mut()
            .and_then(|s| s.midi_player(rec.track))
        {
            // A rebuild (a new bundle root, say) makes fresh, unarmed players.
            if !player.is_armed() {
                player.set_armed(true);
            }
            for c in player.drain_captured() {
                rec.recorder.feed(c);
            }
            if player.take_capture_overflows() > 0 {
                dropped = true;
            }
        }
        // Once per take, so a flood of input can't keep the message up.
        let first_drop = dropped && !rec.dropped;
        rec.dropped |= dropped;
        if first_drop {
            self.report(Message::Record(RecordProblem::InputDropped));
        }
        if !self.engine.is_playing() {
            self.stop_recording();
        }
    }

    /// Disarms, stops playback and adds the take as one undo step.
    pub fn stop_recording(&mut self) {
        let Some(mut rec) = self.recording.take() else {
            return;
        };
        let stop_at = self
            .engine
            .session()
            .map(|s| s.ticks_to_samples(self.playhead()));
        if let Some(player) = self
            .engine
            .session_mut()
            .and_then(|s| s.midi_player(rec.track))
        {
            player.set_armed(false);
            for c in player.drain_captured() {
                rec.recorder.feed(c);
            }
        }
        self.engine.stop();
        let take = rec.recorder.finish(stop_at);
        if take.is_empty() {
            return;
        }
        match take_into(self.doc.project(), &rec, &take) {
            Some(cmd) => {
                if let Err(e) = self.doc.execute_boxed(cmd) {
                    self.report(Message::Record(RecordProblem::Refused(e)));
                }
            }
            None => self.report(Message::Record(RecordProblem::NothingRecorded)),
        }
        self.doc.seal();
    }

    /// Ends a take without keeping it, for when the document or the engine
    /// is about to be replaced.
    pub fn discard_recording(&mut self) {
        let Some(rec) = self.recording.take() else {
            return;
        };
        if let Some(player) = self
            .engine
            .session_mut()
            .and_then(|s| s.midi_player(rec.track))
        {
            player.set_armed(false);
            player.drain_captured().for_each(drop);
        }
    }
}

/// The command that adds `take` to the project: overdubbed into the
/// recording's clip when it has one, else as a new clip on its track.
fn take_into(
    project: &Project,
    rec: &Recording,
    take: &[RecordedNote],
) -> Option<Box<dyn Command>> {
    let (tempo, sample_rate) = (&rec.tempo, rec.sample_rate);
    let in_clip = rec.clip.and_then(|c| project.clip(c)).filter(|clip| {
        let end = clip.start.0 + clip.length.0;
        take.iter().all(|n| {
            tempo.samples_to_ticks(n.start, sample_rate) >= clip.start
                && tempo.samples_to_ticks(n.end, sample_rate).0 <= end
        })
    });
    match in_clip {
        Some(clip) => {
            let range = clip.start..TickTime(clip.start.0 + clip.length.0);
            let notes = to_clip_notes(take, tempo, sample_rate, range);
            let existing = midi_clip(project, clip.id).map_or(&[][..], |m| &m.notes);
            let span = TickTime(rec.from.0 - clip.start.0)..TickTime(i64::MAX);
            let plan = merge_take(existing, notes, TakeMode::Overdub, span);
            take_command(clip.id, existing, plan)
        }
        None => {
            let (start, end) = take_span(project, take, tempo, sample_rate);
            let mut notes: Vec<_> = to_clip_notes(take, tempo, sample_rate, start..end)
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
            Some(Box::new(AddClip::new(rec.track, clip)))
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
        let rec = Recording {
            track,
            clip: None,
            name: "Recording".into(),
            dropped: false,
            recorder: Recorder::new(),
            from: TickTime(0),
            tempo: SegmentTempoMap::default(),
            sample_rate: SR,
        };
        // Beat 5 to 6.5: all inside bar 2 in 4/4.
        let take = [note(5.0, 6.5, 60), note(5.0, 5.5, 64)];
        let cmd = take_into(doc.project(), &rec, &take).unwrap();
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
        let rec = Recording {
            track,
            clip: Some(clip_id),
            name: "Recording".into(),
            dropped: false,
            recorder: Recorder::new(),
            from: TickTime(0),
            tempo: SegmentTempoMap::default(),
            sample_rate: SR,
        };
        let take = [note(1.0, 2.0, 60)];
        let cmd = take_into(doc.project(), &rec, &take).unwrap();
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
        let cmd = take_into(doc.project(), &rec, &take).unwrap();
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
