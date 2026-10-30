//! Tempo, time signature, metronome and count-in, over [`AppState`]. Kept
//! free of Qt so it can be tested without a window.

use std::time::{Duration, Instant};

use daw_model::ModelError;
use daw_model::commands::{SetTempo, SetTimeSignature};

use crate::settings::ClickSettings;
use crate::state::AppState;

/// Tempo changes closer together than this, with nothing in between, are
/// one gesture (a drag, a scroll, typing) and one undo step.
const TEMPO_GESTURE: Duration = Duration::from_secs(2);

impl AppState {
    /// The tempo and time signature in effect at the playhead, as
    /// (bpm, numerator, denominator).
    pub fn tempo_at_playhead(&self) -> (f64, u8, u8) {
        let at = self.playhead();
        let tempo = &self.doc.project().tempo;
        let (numerator, denominator) = tempo.meter_at(at);
        (tempo.bpm_at(at), numerator, denominator)
    }

    /// Sets the tempo in effect at the playhead. Changes in a row (see
    /// [`TEMPO_GESTURE`]) are one undo step.
    pub fn set_bpm(&mut self, bpm: f64) -> Result<(), ModelError> {
        let tempo = self.doc.project().tempo.with_bpm_at(self.playhead(), bpm);
        if tempo == self.doc.project().tempo {
            return Ok(());
        }
        // Merge into the last tempo change only while the document is where
        // it left it (the history top is that change) and it was moments ago.
        let in_a_row = self
            .tempo_edit
            .is_some_and(|(rev, at)| rev == self.doc.revision() && at.elapsed() < TEMPO_GESTURE);
        if !in_a_row {
            self.doc.seal();
        }
        self.doc.execute(SetTempo { tempo })?;
        self.tempo_edit = Some((self.doc.revision(), Instant::now()));
        Ok(())
    }

    /// Sets the time signature in effect at the playhead, as its own undo
    /// step.
    pub fn set_meter(&mut self, numerator: u8, denominator: u8) -> Result<(), ModelError> {
        let tempo = self
            .doc
            .project()
            .tempo
            .with_meter_at(self.playhead(), numerator, denominator);
        if tempo == self.doc.project().tempo {
            return Ok(());
        }
        self.doc.seal();
        let result = self.doc.execute(SetTimeSignature { tempo }).map(drop);
        self.doc.seal();
        self.tempo_edit = None;
        result
    }

    pub fn click(&self) -> ClickSettings {
        self.click
    }

    /// Changes the click; the engine hears it at once.
    pub fn set_click(&mut self, click: ClickSettings) {
        self.click = click.clamped();
        self.apply_click();
    }

    /// Gives the session the click settings, if it doesn't have them (a
    /// restarted engine starts with the defaults).
    pub fn apply_click(&mut self) {
        let metronome = self.click.metronome;
        if let Some(session) = self.engine.session_mut()
            && session.metronome() != metronome
            && let Err(e) = session.set_metronome(metronome)
        {
            self.report(crate::message::Message::Playback(e));
        }
    }

    /// Whether the transport is counting in before recording.
    pub fn is_counting_in(&self) -> bool {
        self.engine
            .session()
            .is_some_and(daw_session::Session::is_counting_in)
    }

    /// Starts playback for a take: after the count-in, when one is set.
    pub(crate) fn play_for_recording(&mut self) {
        let bars = self.click.count_in_bars;
        match self.engine.session_mut() {
            Some(session) if bars > 0 => {
                let _ = session.play_with_count_in(bars);
            }
            _ => self.engine.play(),
        }
    }
}

#[cfg(test)]
mod tests {
    use daw_core::time::{PPQ, TickTime};
    use daw_model::{Document, Project, Track, TrackKind};
    use daw_session::MetronomeSettings;

    use super::*;
    use crate::engine::StubEngine;

    fn state() -> AppState {
        AppState::with_engine(Box::new(StubEngine::default()))
    }

    #[test]
    fn tempo_edits_in_a_row_are_one_undo_step() {
        let mut s = state();
        for bpm in [121.0, 122.0, 140.0] {
            s.set_bpm(bpm).unwrap();
        }
        assert_eq!(s.tempo_at_playhead().0, 140.0);
        s.set_meter(6, 8).unwrap();
        assert_eq!(s.tempo_at_playhead(), (140.0, 6, 8));
        // Another drag after the meter edit is a step of its own.
        s.set_bpm(100.0).unwrap();

        s.doc.undo().unwrap();
        assert_eq!(s.tempo_at_playhead(), (140.0, 6, 8));
        s.doc.undo().unwrap();
        assert_eq!(s.tempo_at_playhead(), (140.0, 4, 4));
        s.doc.undo().unwrap();
        assert_eq!(s.tempo_at_playhead(), (120.0, 4, 4));
        assert!(!s.doc.history().can_undo());
    }

    #[test]
    fn a_pause_starts_a_new_undo_step() {
        let mut s = state();
        s.set_bpm(130.0).unwrap();
        // As if the last change were long ago.
        s.tempo_edit = s
            .tempo_edit
            .map(|(rev, at)| (rev, at - TEMPO_GESTURE - Duration::from_millis(1)));
        s.set_bpm(90.0).unwrap();
        s.doc.undo().unwrap();
        assert_eq!(s.tempo_at_playhead().0, 130.0);
    }

    #[test]
    fn a_tempo_edit_after_undo_does_not_merge_into_an_older_one() {
        let mut s = state();
        s.set_bpm(130.0).unwrap();
        s.set_bpm(131.0).unwrap();
        s.doc.undo().unwrap();
        // The history top is no longer a tempo change made just now.
        s.doc.redo().unwrap();
        s.set_bpm(90.0).unwrap();
        s.doc.undo().unwrap();
        assert_eq!(s.tempo_at_playhead().0, 131.0);
    }

    #[test]
    fn edits_reach_the_engine_and_keep_clips_in_ticks() {
        let mut s = state();
        let mut doc = Document::new(Project::new("t"));
        let mut track = Track::new(TrackKind::Instrument, "keys");
        track.clips.push(std::sync::Arc::new(daw_model::Clip::new(
            "c",
            TickTime(4 * PPQ),
            TickTime(PPQ),
            daw_model::ClipContent::Midi(Default::default()),
        )));
        doc.execute(daw_model::commands::AddTrack::new(0, track))
            .unwrap();
        s.replace_document(doc);
        s.set_bpm(60.0).unwrap();
        s.set_meter(3, 4).unwrap();
        s.sync();
        let clip = &s.doc.project().tracks[0].clips[0];
        assert_eq!(clip.start, TickTime(4 * PPQ));
        let session = s.engine.session().unwrap();
        assert_eq!(
            daw_core::time::TempoMap::bpm_at(&session.controller().timeline().tempo, TickTime(0)),
            60.0
        );
        // Beat 4 is four seconds in at 60 bpm.
        assert_eq!(session.ticks_to_samples(clip.start).0, 4 * 48_000);
        assert_eq!(
            session.controller().timeline().meter.points()[0].numerator,
            3
        );
    }

    #[test]
    fn click_settings_reach_the_session_and_survive_a_restart() {
        let mut s = state();
        let click = ClickSettings {
            metronome: MetronomeSettings {
                enabled: true,
                volume_db: -100.0,
            },
            count_in_bars: 7,
        };
        s.set_click(click);
        // Clamped.
        assert_eq!(s.click().count_in_bars, 2);
        assert_eq!(s.click().metronome.volume_db, -40.0);
        assert_eq!(s.engine.session().unwrap().metronome(), s.click().metronome);
        // A restarted engine brings a session with the defaults; the next
        // full sync hands it the settings again.
        s.engine
            .session_mut()
            .unwrap()
            .set_metronome(MetronomeSettings::default())
            .unwrap();
        s.resync();
        assert_eq!(s.engine.session().unwrap().metronome(), s.click().metronome);
    }

    #[test]
    fn recording_counts_in_first() {
        let mut s = state();
        let mut doc = Document::new(Project::new("t"));
        doc.execute(daw_model::commands::AddTrack::new(
            0,
            Track::new(TrackKind::Instrument, "keys"),
        ))
        .unwrap();
        s.replace_document(doc);
        s.set_click(ClickSettings {
            count_in_bars: 1,
            ..ClickSettings::default()
        });
        s.start_recording().unwrap();
        assert!(s.is_recording() && s.engine.is_playing());
        // The silent backend runs in real time: the count-in (two seconds at
        // 120 bpm) holds the playhead at the start for a while.
        let start = Instant::now();
        while !s.is_counting_in() && start.elapsed() < Duration::from_secs(1) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(s.is_counting_in());
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(s.playhead(), TickTime(0));
        s.poll_recording();
        assert!(s.is_recording(), "still recording during the count-in");
        s.stop_recording();
        assert!(!s.is_counting_in() || !s.engine.is_playing());
    }
}
