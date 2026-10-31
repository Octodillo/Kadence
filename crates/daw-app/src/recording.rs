//! Recording: the Record action records every armed track, audio and
//! MIDI, and on stop adds the audio takes as one undoable edit and the
//! MIDI take as another.
//!
//! Audio takes are recorded by `daw-session` ([`AudioTakes`]) into the
//! project bundle's audio folder, or a recordings folder in the user's data
//! directory while the project is unsaved.
//!
//! MIDI arms the target track's clip player and collects what it captures
//! while playing. The take is overdubbed into the clip open in the piano
//! roll when recording starts inside it and every note fits; otherwise it
//! becomes a new clip spanning the bars that were played into, so nothing
//! is lost. With nothing armed, Record records MIDI on the open clip's
//! track, the selected track or the first instrument track.

use daw_core::time::{SegmentTempoMap, TempoMap, TickTime};
use daw_midi::model::{meter, midi_clip, new_note, take_command, tempo_map};
use daw_midi::record::{RecordedNote, Recorder, TakeMode, merge_take, to_clip_notes};
use std::path::PathBuf;

use daw_model::commands::AddClip;
use daw_model::{Clip, ClipContent, ClipId, Command, MidiClip, Project, TrackId, TrackKind};
use daw_session::record::{AudioTakes, armed_audio_tracks};

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

/// Audio takes in progress.
pub struct AudioRecording {
    takes: AudioTakes,
    /// The audio system's xrun count when recording started.
    xruns: u64,
    /// Whether xruns were reported for this recording.
    xruns_reported: bool,
}

/// Where takes of an unsaved project go: `kadence/recordings` in the
/// user's data directory.
fn unsaved_recordings_dir() -> PathBuf {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(std::env::temp_dir);
    data.join("kadence").join("recordings")
}

/// The instrument track that hears live MIDI while none is armed: the one
/// recording goes to, else the selected track, else the open clip's track,
/// else the first instrument track. Unlike [`target`] it ignores the
/// playhead, so live input doesn't jump between tracks (cutting held
/// notes) as playback crosses the open clip.
pub fn input_focus(state: &AppState) -> Option<TrackId> {
    if let Some(rec) = &state.recording {
        return Some(rec.track);
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

/// Whether any audio or instrument track is armed.
fn anything_armed(project: &Project) -> bool {
    project
        .tracks
        .iter()
        .any(|t| t.armed && matches!(t.kind, TrackKind::Audio | TrackKind::Instrument))
}

/// The track to record MIDI on. When anything is armed, the first armed
/// instrument track (the open clip goes with it when the playhead is in
/// it), or none. Otherwise the open clip's track when the playhead is in
/// that clip, else the selected track, else the open clip's track, else
/// the first instrument track. Only instrument tracks qualify.
fn target(state: &AppState) -> Option<(TrackId, Option<ClipId>)> {
    let project = state.doc.project();
    let armed = anything_armed(project);
    let is_instrument = |id: TrackId| {
        project
            .track(id)
            .is_some_and(|t| t.kind == TrackKind::Instrument && (t.armed || !armed))
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
        .find(|t| is_instrument(t.id))
        .map(|t| (t.id, None))
}

impl AppState {
    pub fn is_recording(&self) -> bool {
        self.recording.is_some() || self.audio_recording.is_some()
    }

    /// Starts recording every armed audio track and MIDI on the target
    /// track, then starts playback. Fails when there is nothing to record
    /// on or no running session; problems with single tracks are reported
    /// and the rest record.
    pub fn start_recording(&mut self) -> Result<(), Message> {
        if self.is_recording() {
            return Ok(());
        }
        let audio_armed = armed_audio_tracks(self.doc.project()).next().is_some();
        let audio = self.start_audio()?;
        let midi = match target(self) {
            Some((track, clip)) => self.start_midi(track, clip),
            None if audio => Ok(()),
            // Every armed audio track failed, and each was reported.
            None if audio_armed => return Ok(()),
            None => Err(Message::Record(RecordProblem::NoInstrumentTrack)),
        };
        match midi {
            Ok(()) => {}
            // Audio is recording; say why MIDI isn't and go on.
            Err(e) if audio => self.report(e),
            Err(e) => return Err(e),
        }
        // Capture waits for the count-in: takes only record while the
        // transport moves.
        self.play_for_recording();
        Ok(())
    }

    /// Starts the audio takes. Returns whether any track records.
    fn start_audio(&mut self) -> Result<bool, Message> {
        let project = self.doc.project();
        if armed_audio_tracks(project).next().is_none() {
            return Ok(false);
        }
        let dir = self
            .doc
            .bundle()
            .map_or_else(unsaved_recordings_dir, |b| b.audio_dir());
        let latency = self.engine.round_trip_latency();
        let xruns = self.engine.xruns();
        let Some(session) = self.engine.session_mut() else {
            return Err(Message::Record(RecordProblem::EngineNotRunning));
        };
        let (takes, problems) = AudioTakes::start(session, project, &dir, latency);
        for p in problems {
            self.report(Message::Record(RecordProblem::Take(p)));
        }
        if takes.is_empty() {
            return Ok(false);
        }
        self.audio_recording = Some(AudioRecording {
            takes,
            xruns,
            xruns_reported: false,
        });
        Ok(true)
    }

    /// Arms the MIDI player of `track` for a take into `clip`, if given.
    fn start_midi(&mut self, track: TrackId, clip: Option<ClipId>) -> Result<(), Message> {
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
        Ok(())
    }

    /// Feeds what the player captured into the take and reports problems
    /// with the audio takes. Stops recording when playback stopped.
    pub fn poll_recording(&mut self) {
        self.poll_audio();
        self.poll_midi();
        if self.is_recording() && !self.engine.is_playing() {
            self.stop_recording();
        }
    }

    fn poll_audio(&mut self) {
        let xruns = self.engine.xruns();
        let Some(rec) = self.audio_recording.as_mut() else {
            return;
        };
        let mut messages: Vec<Message> = rec
            .takes
            .poll()
            .into_iter()
            .map(|p| Message::Record(RecordProblem::Take(p)))
            .collect();
        let missed = xruns.saturating_sub(rec.xruns);
        if missed > 0 && !rec.xruns_reported {
            rec.xruns_reported = true;
            messages.push(Message::Record(RecordProblem::Xruns(missed)));
        }
        for m in messages {
            self.report(m);
        }
    }

    fn poll_midi(&mut self) {
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
        self.show_live_take();
    }

    /// Shows the take so far in the piano roll when it has a clip of the
    /// recorded track open, held keys growing up to the playhead.
    fn show_live_take(&mut self) {
        let Some(rec) = self.recording.as_ref() else {
            return;
        };
        let project = self.doc.project();
        let Some(clip) = self
            .piano
            .clip
            .filter(|&c| {
                project
                    .clip_location(c)
                    .is_some_and(|(t, _)| t == rec.track)
            })
            .and_then(|c| project.clip(c))
        else {
            if !self.piano.live.is_empty() {
                self.piano.live.clear();
                self.touch();
            }
            return;
        };
        let now = rec.tempo.ticks_to_samples(self.playhead(), rec.sample_rate);
        let mut notes = rec.recorder.notes().to_vec();
        notes.extend(
            rec.recorder
                .held()
                .map(|(channel, key, start, velocity)| RecordedNote {
                    start,
                    end: now.max(start),
                    key,
                    channel,
                    velocity,
                }),
        );
        let range = clip.start..TickTime(clip.start.0 + clip.length.0);
        let live = to_clip_notes(&notes, &rec.tempo, rec.sample_rate, range);
        if live != self.piano.live {
            self.piano.live = live;
            self.touch();
        }
    }

    /// Stops recording and playback and adds the takes: all audio takes as
    /// one undo step, the MIDI take as another.
    pub fn stop_recording(&mut self) {
        if !self.is_recording() {
            return;
        }
        let mut commands = Vec::new();
        if let Some(cmd) = self.stop_midi() {
            commands.push(cmd);
        }
        self.engine.stop();
        if let Some(mut rec) = self.audio_recording.take() {
            let root = self.root();
            // Port latencies settle a while after connections change; the
            // value now is more trustworthy than the one at the start.
            rec.takes.set_io_latency(self.engine.round_trip_latency());
            if let Some(session) = self.engine.session_mut() {
                let done = rec
                    .takes
                    .finish(session, self.doc.project(), root.as_deref());
                commands.extend(done.command);
                for p in done.problems {
                    self.report(Message::Record(RecordProblem::Take(p)));
                }
            }
        }
        // Each kind of take is its own undo step, so a take the model
        // refuses can't take the other kind down with it.
        for cmd in commands {
            if let Err(e) = self.doc.execute_boxed(cmd) {
                self.report(Message::Record(RecordProblem::Refused(e)));
            }
            self.doc.seal();
        }
        self.doc.seal();
    }

    /// Disarms the MIDI player and returns the command that adds its take.
    fn stop_midi(&mut self) -> Option<Box<dyn Command>> {
        let mut rec = self.recording.take()?;
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
        self.piano.live.clear();
        let take = rec.recorder.finish(stop_at);
        if take.is_empty() {
            return None;
        }
        let cmd = take_into(self.doc.project(), &rec, &take);
        if cmd.is_none() {
            self.report(Message::Record(RecordProblem::NothingRecorded));
        }
        cmd
    }

    /// Ends a take without keeping it, for when the document or the engine
    /// is about to be replaced.
    pub fn discard_recording(&mut self) {
        if let Some(rec) = self.audio_recording.take()
            && let Some(session) = self.engine.session_mut()
        {
            rec.takes.discard(session);
        }
        let Some(rec) = self.recording.take() else {
            return;
        };
        self.piano.live.clear();
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
