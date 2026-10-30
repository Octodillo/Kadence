//! Exporting the song to an audio file, for the export dialog.
//!
//! The render itself is `daw_session::export`; this turns the dialog's
//! choices into its settings and keeps the running job. Nothing here may
//! mention Qt.

use std::path::PathBuf;

use daw_core::time::TickTime;
use daw_media::{FileFormat, FlacBits, WavFormat};
use daw_session::export::{ExportJob, ExportSettings, RenderSettings, song_end};

use crate::message::{ExportProblem, Message};
use crate::state::AppState;

/// The file formats the dialog offers, in its order.
pub const FORMATS: [FileFormat; 5] = [
    FileFormat::Wav(WavFormat::Pcm16),
    FileFormat::Wav(WavFormat::Pcm24),
    FileFormat::Wav(WavFormat::Float32),
    FileFormat::Flac(FlacBits::B16),
    FileFormat::Flac(FlacBits::B24),
];

/// The file name extension for `format`.
pub fn extension(format: FileFormat) -> &'static str {
    match format {
        FileFormat::Wav(_) => "wav",
        FileFormat::Flac(_) => "flac",
    }
}

/// Which part of the song to export.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Range {
    /// From the start to the end of the last clip.
    Song,
    /// The loop range.
    Loop,
    /// Bars `first` to `last`, counted from 1, both included.
    Bars { first: i64, last: i64 },
}

/// The dialog's choices.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub range: Range,
    pub tail_seconds: f64,
    pub format: FileFormat,
    pub sample_rate: u32,
    pub dither: bool,
}

impl AppState {
    /// The span `range` covers in this project.
    pub fn export_span(&self, range: Range) -> Result<(TickTime, TickTime), ExportProblem> {
        let project = self.doc.project();
        let (start, end) = match range {
            Range::Song => {
                let end = song_end(project);
                if end <= TickTime(0) {
                    return Err(ExportProblem::EmptySong);
                }
                (TickTime(0), end)
            }
            Range::Loop => self.loop_range().ok_or(ExportProblem::NoLoop)?,
            Range::Bars { first, last } => {
                if first < 1 || last < first {
                    return Err(ExportProblem::EmptyRange);
                }
                let meter = self.arrange.meter(project);
                (meter.bar_start(first - 1), meter.bar_start(last))
            }
        };
        if end <= start {
            return Err(ExportProblem::EmptyRange);
        }
        Ok((start, end))
    }

    /// How many bars the song spans, counting a bar the last clip ends in
    /// part way.
    pub fn song_bars(&self) -> i64 {
        let project = self.doc.project();
        let end = song_end(project);
        if end <= TickTime(0) {
            return 0;
        }
        let meter = self.arrange.meter(project);
        let bar = meter.bar_at(TickTime(end.0 - 1));
        bar.index + 1
    }

    /// Starts exporting the song to `path` on a worker thread; playback and
    /// editing go on meanwhile. `path` gets the format's extension if it has
    /// none. Watch it with [`poll_export`](Self::poll_export).
    pub fn start_export(&mut self, mut path: PathBuf, request: &Request) -> Result<(), Message> {
        if self.export.is_some() {
            return Err(Message::Export(ExportProblem::Busy));
        }
        let (start, end) = self.export_span(request.range).map_err(Message::Export)?;
        let wanted = extension(request.format);
        match path.extension().map(|e| e.to_string_lossy().to_lowercase()) {
            Some(e) if e == wanted => {}
            // Writing FLAC into "mix.wav" (or the other way round) would make a
            // file other programs misread.
            Some(e) if FORMATS.iter().any(|&f| extension(f) == e) => {
                return Err(Message::Export(ExportProblem::WrongExtension(e)));
            }
            // "Song v1.2" has no audio extension yet.
            _ => {
                let mut name = path.file_name().unwrap_or_default().to_os_string();
                name.push(".");
                name.push(wanted);
                path.set_file_name(name);
            }
        }
        let settings = ExportSettings {
            render: RenderSettings {
                start,
                end,
                tail_seconds: request.tail_seconds,
                sample_rate: request.sample_rate,
            },
            format: request.format,
            dither: request.dither,
        };
        let job = ExportJob::start(self.doc.snapshot(), self.root(), path, settings)
            .map_err(|e| Message::Export(ExportProblem::Thread(e)))?;
        self.export = Some(job);
        Ok(())
    }

    /// How far the running export is, from 0 to 1; `None` when none runs.
    pub fn export_progress(&self) -> Option<f64> {
        self.export.as_ref().map(ExportJob::progress)
    }

    /// Asks the running export to stop. [`poll_export`](Self::poll_export)
    /// then reports it cancelled.
    pub fn cancel_export(&mut self) {
        if let Some(job) = &self.export {
            job.cancel();
        }
    }

    /// Once the running export has ended, how it went: the file written, or
    /// why there is none. Warnings from building the project (a clip whose
    /// file is missing) go to the status line.
    pub fn poll_export(&mut self) -> Option<Message> {
        if !self.export.as_ref()?.is_finished() {
            return None;
        }
        let job = self.export.take().expect("checked above");
        let path = job.path().to_path_buf();
        Some(match job.wait() {
            Ok(report) => {
                if let Some(w) = report.warnings.first() {
                    self.report(Message::Warning(w.clone()));
                }
                let rate = u64::from(report.sample_rate.max(1));
                let seconds = (report.frames + rate / 2) / rate;
                Message::Exported { path, seconds }
            }
            Err(e) => Message::Export(ExportProblem::Failed(e)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::EngineHandle;
    use crate::message::{Context, Reason};
    use crate::state;

    fn request(range: Range) -> Request {
        Request {
            range,
            tail_seconds: 0.5,
            format: FileFormat::Wav(WavFormat::Pcm16),
            sample_rate: 48_000,
            dither: true,
        }
    }

    /// Waits for the export and returns its message.
    fn finish(s: &mut AppState) -> Message {
        loop {
            if let Some(m) = s.poll_export() {
                return m;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn ranges() {
        state::with(|s| {
            s.replace_document(crate::demo::build().unwrap());
            // The demo's last clip ends at bar 12.
            assert_eq!(s.song_bars(), 12);
            let bar = 4 * daw_core::time::PPQ;
            assert_eq!(
                s.export_span(Range::Song).unwrap(),
                (TickTime(0), TickTime(12 * bar))
            );
            assert_eq!(
                s.export_span(Range::Bars { first: 2, last: 3 }).unwrap(),
                (TickTime(bar), TickTime(3 * bar))
            );
            assert!(matches!(
                s.export_span(Range::Bars { first: 3, last: 2 }),
                Err(ExportProblem::EmptyRange)
            ));
            assert!(matches!(
                s.export_span(Range::Loop),
                Err(ExportProblem::NoLoop)
            ));
            s.engine
                .session_mut()
                .unwrap()
                .set_loop(Some((TickTime(bar), TickTime(2 * bar))))
                .unwrap();
            assert_eq!(
                s.export_span(Range::Loop).unwrap(),
                (TickTime(bar), TickTime(2 * bar))
            );
            s.new_document();
            assert!(matches!(
                s.export_span(Range::Song),
                Err(ExportProblem::EmptySong)
            ));
            assert_eq!(s.song_bars(), 0);
        });
    }

    #[test]
    fn exports_the_demo_and_reports_the_file() {
        let dir = tempfile::tempdir().unwrap();
        state::with(|s| {
            s.replace_document(crate::demo::build().unwrap());
            // No extension: the format's is added.
            s.start_export(dir.path().join("demo"), &request(Range::Song))
                .unwrap();
            assert!(matches!(
                s.start_export(dir.path().join("again"), &request(Range::Song)),
                Err(Message::Export(ExportProblem::Busy))
            ));
            let f = finish(s).fields();
            assert_eq!((f.context, f.reason), (Context::Export, Reason::Exported));
            let path = dir.path().join("demo.wav");
            assert_eq!(f.path.as_deref(), Some(path.as_path()));
            // Twelve bars at 120 bpm and the half-second tail, rounded.
            assert_eq!(f.number, 25);
            let data = daw_media::decode_file(&path).unwrap();
            assert_eq!(data.frames(), 24 * 48_000 + 24_000);
            assert!(s.export_progress().is_none());
        });
    }

    /// The demo song exported, against what the stub engine's session
    /// plays: the same session code, driven in 256-frame periods the way the
    /// backend's callback drives it.
    #[test]
    fn the_demo_export_matches_playback() {
        use daw_core::event::EventList;
        use daw_session::export::{Progress, render};
        let mut doc = crate::demo::build().unwrap();
        let root = doc.bundle().unwrap().root().to_path_buf();
        let snapshot = doc.snapshot();
        let (start, end) = (TickTime(0), song_end(&snapshot));
        let settings = RenderSettings {
            start,
            end,
            tail_seconds: 0.0,
            sample_rate: 48_000,
        };
        let mut exported = Vec::new();
        render(
            &snapshot,
            Some(root.clone()),
            &settings,
            &Progress::default(),
            |planes| {
                for (l, r) in planes[0].iter().zip(planes[1]) {
                    exported.extend([*l, *r]);
                }
                Ok(())
            },
        )
        .unwrap();

        let mut engine = crate::engine::StubEngine::default();
        let config = engine.session().unwrap().config().clone();
        let (mut session, mut proc) = daw_session::Session::new(config);
        session.set_root(Some(root));
        session.sync(&snapshot).unwrap();
        session.play().unwrap();
        let events = EventList::with_capacity(0);
        let (mut l, mut r) = (vec![0.0; 256], vec![0.0; 256]);
        let mut played = Vec::with_capacity(exported.len());
        while played.len() < exported.len() {
            let n = 256.min((exported.len() - played.len()) / 2);
            let mut outs: [&mut [f32]; 2] = [&mut l[..n], &mut r[..n]];
            proc.process(n, &[], &mut outs, &events);
            for (l, r) in l[..n].iter().zip(&r[..n]) {
                played.extend([*l, *r]);
            }
            session.collect_garbage();
        }
        engine.stop();

        let peak = played.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        assert!(peak > 0.1, "the demo plays");
        let diff = exported
            .iter()
            .zip(&played)
            .fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(diff < 1e-5, "export differs from playback by {diff}");
    }

    #[test]
    fn file_names_get_the_formats_extension() {
        let dir = tempfile::tempdir().unwrap();
        state::with(|s| {
            s.replace_document(crate::demo::build().unwrap());
            let mut r = request(Range::Bars { first: 1, last: 1 });
            r.format = FileFormat::Flac(FlacBits::B24);
            assert!(matches!(
                s.start_export(dir.path().join("mix.WAV"), &r),
                Err(Message::Export(ExportProblem::WrongExtension(e))) if e == "wav"
            ));
            s.start_export(dir.path().join("Song v1.2"), &r).unwrap();
            let f = finish(s).fields();
            assert_eq!(
                f.path.as_deref(),
                Some(dir.path().join("Song v1.2.flac").as_path())
            );
            s.start_export(dir.path().join("mix.Flac"), &r).unwrap();
            assert_eq!(finish(s).fields().reason, Reason::Exported);
            assert!(dir.path().join("mix.Flac").exists());
        });
    }

    #[test]
    fn a_cancelled_export_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        state::with(|s| {
            s.replace_document(crate::demo::build().unwrap());
            let mut r = request(Range::Song);
            r.tail_seconds = 30.0;
            s.start_export(dir.path().join("demo.wav"), &r).unwrap();
            s.cancel_export();
            let f = finish(s).fields();
            assert_eq!(f.reason, Reason::ExportCancelled);
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        });
    }
}
