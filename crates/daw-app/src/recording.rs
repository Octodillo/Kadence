//! MIDI recording: arms the target track's clip player, collects what it
//! captures while playing, and turns the take into one undoable edit.
//!
//! The take goes into the clip open in the piano roll when it is on the
//! target track (overdubbed), otherwise into a new clip spanning the bars
//! that were played into.

use daw_core::time::{TempoMap, TickTime};
use daw_midi::model::{meter, midi_clip, new_note, take_command, tempo_map};
use daw_midi::record::{RecordedNote, Recorder, TakeMode, merge_take, to_clip_notes};
use daw_model::commands::AddClip;
use daw_model::{Clip, ClipContent, ClipId, Command, MidiClip, Project, TrackId, TrackKind};

use crate::state::AppState;

/// A take in progress.
pub struct Recording {
    track: TrackId,
    /// The clip the take goes into, if one was open on the track.
    clip: Option<ClipId>,
    recorder: Recorder,
    /// Where playback was when recording started.
    from: TickTime,
}

/// The track to record on: the open clip's track, else the selected track,
/// else the first instrument track. Only instrument tracks qualify.
fn target(state: &AppState) -> Option<(TrackId, Option<ClipId>)> {
    let project = state.doc.project();
    let is_instrument = |id: TrackId| {
        project
            .track(id)
            .is_some_and(|t| t.kind == TrackKind::Instrument)
    };
    if let Some(clip) = state.piano.clip
        && midi_clip(project, clip).is_some()
        && let Some((track, _)) = project.clip_location(clip)
        && is_instrument(track)
    {
        return Some((track, Some(clip)));
    }
    if let Some(track) = state.arrange.selected_track
        && is_instrument(track)
    {
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

    /// Arms the target track and starts playback. Fails when there is no
    /// instrument track or no running session.
    pub fn start_recording(&mut self) -> Result<(), String> {
        if self.recording.is_some() {
            return Ok(());
        }
        let Some((track, clip)) = target(self) else {
            return Err("Add an instrument track to record MIDI".into());
        };
        let from = self.playhead();
        let Some(session) = self.engine.session_mut() else {
            return Err("The audio engine is not running".into());
        };
        let Some(player) = session.midi_player(track) else {
            return Err("The track has no MIDI player yet".into());
        };
        // Anything captured before arming belongs to no take.
        player.drain_captured().for_each(drop);
        player.set_armed(true);
        self.recording = Some(Recording {
            track,
            clip,
            recorder: Recorder::new(),
            from,
        });
        self.engine.play();
        Ok(())
    }

    /// Feeds what the player captured into the take. Stops recording when
    /// playback stopped.
    pub fn poll_recording(&mut self) {
        let Some(rec) = self.recording.as_mut() else {
            return;
        };
        if let Some(player) = self
            .engine
            .session_mut()
            .and_then(|s| s.midi_player(rec.track))
        {
            for c in player.drain_captured() {
                rec.recorder.feed(c);
            }
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
        let sample_rate = self.sample_rate();
        let cmd = take_into(self.doc.project(), &rec, &take, sample_rate);
        let result = cmd.map(|c| self.doc.execute_boxed(c));
        if let Some(Err(e)) = result {
            self.status = e.to_string();
        }
        self.doc.seal();
    }
}

/// The command that adds `take` to the project: overdubbed into the
/// recording's clip when it has one, else as a new clip on its track.
fn take_into(
    project: &Project,
    rec: &Recording,
    take: &[RecordedNote],
    sample_rate: f64,
) -> Option<Box<dyn Command>> {
    let tempo = tempo_map(&project.tempo);
    match rec.clip.and_then(|c| project.clip(c)) {
        Some(clip) => {
            let range = clip.start..TickTime(clip.start.0 + clip.length.0);
            let notes = to_clip_notes(take, &tempo, sample_rate, range);
            let existing = midi_clip(project, clip.id).map_or(&[][..], |m| &m.notes);
            let span = TickTime(rec.from.0 - clip.start.0)..TickTime(i64::MAX);
            let plan = merge_take(existing, notes, TakeMode::Overdub, span);
            take_command(clip.id, existing, plan)
        }
        None => {
            let (start, end) = take_span(project, take, &tempo, sample_rate);
            let mut notes: Vec<_> = to_clip_notes(take, &tempo, sample_rate, start..end)
                .into_iter()
                .map(new_note)
                .collect();
            if notes.is_empty() {
                return None;
            }
            notes.sort_by_key(|n| (n.start, n.key, n.id));
            let clip = Clip::new(
                "Recording",
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
            recorder: Recorder::new(),
            from: TickTime(0),
        };
        // Beat 5 to 6.5: all inside bar 2 in 4/4.
        let take = [note(5.0, 6.5, 60), note(5.0, 5.5, 64)];
        let cmd = take_into(doc.project(), &rec, &take, SR).unwrap();
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
            recorder: Recorder::new(),
            from: TickTime(0),
        };
        // The second note starts past the clip's end and is dropped.
        let take = [note(1.0, 2.0, 60), note(5.0, 6.0, 62)];
        let cmd = take_into(doc.project(), &rec, &take, SR).unwrap();
        doc.execute_boxed(cmd).unwrap();
        let keys: Vec<u8> = midi_clip(doc.project(), clip_id)
            .unwrap()
            .notes
            .iter()
            .map(|n| n.key)
            .collect();
        assert_eq!(keys, [48, 60]);
        assert_eq!(doc.history().undo_label().as_deref(), Some("Record Notes"));
    }
}
