//! The application state shared by the QML-facing objects: the open
//! document, the audio engine and the arrange view.
//!
//! It lives in a thread-local on the GUI thread. Every bridge object
//! (`Engine`, `Project`, `ArrangeView`) runs its invokables there and reaches
//! the state through [`with`]. Nothing here is a Qt type.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use daw_arrange::{Arrange, Env};
use daw_core::time::{SegmentTempoMap, TickTime};
use daw_media::Decoder;
use daw_model::{Document, FormatError, Project};
use daw_pianoroll::PianoRoll;

use crate::engine::{EngineHandle, default_engine};
use crate::message::{Context, Fields, Message};

pub struct AppState {
    pub doc: Document,
    pub engine: Box<dyn EngineHandle>,
    pub arrange: Arrange,
    /// The piano roll on the Edit page.
    pub piano: PianoRoll,
    /// The instruments the instrument menus offer.
    pub instruments: crate::instrument::InstrumentChoices,
    /// The MIDI take in progress.
    pub recording: Option<crate::recording::Recording>,
    /// The export to a file in progress (see `crate::export`).
    pub export: Option<daw_session::export::ExportJob>,
    /// The audio takes in progress.
    pub audio_recording: Option<crate::recording::AudioRecording>,
    /// The document revision the engine last played.
    synced: Option<u64>,
    /// Bumped whenever anything a view shows changed: the document, the
    /// selection, the zoom. QML watches it.
    pub view_revision: u64,
    /// The last error or warning, for the status line.
    pub status: Option<Fields>,
    /// The document revision whose autosave failed and when it was last
    /// tried, so the timer reports it once and retries only now and then.
    autosave_failed: Option<(u64, Instant)>,
    /// Scroll to keep the playhead in view while playing.
    pub follow_playhead: bool,
    /// Tempo map for the view when no session runs.
    fallback_tempo: SegmentTempoMap,
    /// Counts documents opened, so views can tell a new document from an
    /// edit (document revisions start over).
    pub doc_serial: u64,
}

thread_local! {
    static STATE: RefCell<Option<AppState>> = const { RefCell::new(None) };
}

/// Runs `f` with the application state, creating it on first use. Must not
/// be called again from inside `f`.
pub fn with<R>(f: impl FnOnce(&mut AppState) -> R) -> R {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let state = s.get_or_insert_with(AppState::new);
        f(state)
    })
}

impl AppState {
    fn new() -> Self {
        let mut s = Self {
            doc: Document::new(Project::default()),
            engine: default_engine(),
            arrange: Arrange::new(),
            piano: PianoRoll::new(),
            instruments: crate::instrument::InstrumentChoices::builtin(),
            recording: None,
            export: None,
            audio_recording: None,
            synced: None,
            view_revision: 0,
            status: None,
            autosave_failed: None,
            follow_playhead: true,
            fallback_tempo: SegmentTempoMap::default(),
            doc_serial: 0,
        };
        s.sync();
        s
    }

    /// Hands the document to the engine if it changed since last time.
    pub fn sync(&mut self) {
        let revision = self.doc.revision();
        if self.synced == Some(revision) {
            return;
        }
        let root = self.doc.bundle().map(|b| b.root().to_path_buf());
        let snapshot = self.doc.snapshot();
        if let Some(session) = self.engine.session_mut() {
            session.set_root(root);
            if let Err(e) = session.sync(&snapshot) {
                self.status = Some(Message::Playback(e).fields());
            } else if let Some(w) = session.warnings().first() {
                self.status = Some(Message::Warning(w.clone()).fields());
            }
        }
        self.synced = Some(revision);
        self.view_revision += 1;
    }

    /// Writes an autosave if the document changed. A failure is reported
    /// once per revision and retried quietly every few seconds, in case it
    /// was passing (a full disk that was cleared).
    pub fn autosave(&mut self) {
        const RETRY: Duration = Duration::from_secs(10);
        let revision = self.doc.revision();
        let failed_here = matches!(self.autosave_failed, Some((r, _)) if r == revision);
        if let Some((r, at)) = self.autosave_failed
            && r == revision
            && at.elapsed() < RETRY
        {
            return;
        }
        match self.doc.autosave() {
            Ok(_) => self.autosave_failed = None,
            Err(e) => {
                self.autosave_failed = Some((revision, Instant::now()));
                if !failed_here {
                    self.report(Message::File(Context::Autosave, e));
                }
            }
        }
    }

    /// Forces a full resync, after the engine was restarted.
    pub fn resync(&mut self) {
        self.synced = None;
        self.sync();
    }

    /// Shows `message` in the status line.
    pub fn report(&mut self, message: Message) {
        self.status = Some(message.fields());
        self.touch();
    }

    /// Something visible changed.
    pub fn touch(&mut self) {
        self.view_revision += 1;
    }

    pub fn root(&self) -> Option<PathBuf> {
        self.doc.bundle().map(|b| b.root().to_path_buf())
    }

    pub fn playhead(&self) -> TickTime {
        self.engine.session().map_or(TickTime(0), |s| s.position())
    }

    pub fn loop_range(&self) -> Option<(TickTime, TickTime)> {
        self.engine.session().and_then(|s| s.loop_range())
    }

    pub fn sample_rate(&self) -> f64 {
        self.engine.session().map_or(48_000.0, |s| s.sample_rate())
    }

    /// Calls `f` with the arrange view, the document and an [`Env`].
    /// Ends an arrange drag in progress before an edit from elsewhere (undo,
    /// a menu), so the drag doesn't merge with it or touch what it removed.
    pub fn end_drag(&mut self) {
        self.arrange.end_drag(&mut self.doc);
    }

    pub fn with_arrange<R>(&mut self, f: impl FnOnce(&mut Arrange, &mut Document, &Env) -> R) -> R {
        let root = self.root();
        let playhead = self.playhead();
        let loop_range = self.loop_range();
        let sample_rate = self.sample_rate();
        let tempo = self
            .engine
            .session()
            .map_or(&self.fallback_tempo, |s| s.tempo());
        let env = Env {
            tempo,
            sample_rate,
            root: root.as_deref(),
            playhead,
            loop_range,
        };
        f(&mut self.arrange, &mut self.doc, &env)
    }

    /// Calls `f` with the piano roll, the document and an [`Env`].
    pub fn with_piano<R>(&mut self, f: impl FnOnce(&mut PianoRoll, &mut Document, &Env) -> R) -> R {
        let root = self.root();
        let playhead = self.playhead();
        let loop_range = self.loop_range();
        let sample_rate = self.sample_rate();
        let tempo = self
            .engine
            .session()
            .map_or(&self.fallback_tempo, |s| s.tempo());
        let env = Env {
            tempo,
            sample_rate,
            root: root.as_deref(),
            playhead,
            loop_range,
        };
        f(&mut self.piano, &mut self.doc, &env)
    }

    /// Applies what an arrange view gesture asked for.
    pub fn apply(&mut self, out: daw_arrange::Outcome) {
        if let Some(clip) = out.open_clip {
            self.piano.open(self.doc.project(), clip);
        }
        if let Some(t) = out.locate
            && let Some(s) = self.engine.session_mut()
        {
            let _ = s.locate(t);
        }
        if let Some(range) = out.set_loop
            && let Some(s) = self.engine.session_mut()
        {
            let _ = s.set_loop(range);
        }
        if let Some(e) = out.error {
            self.report(Message::Edit(e));
        }
        if out.document {
            self.sync();
        }
        if out.view || out.locate.is_some() || out.set_loop.is_some() {
            self.touch();
        }
    }

    // -----------------------------------------------------------------------
    // Files

    pub fn new_document(&mut self) {
        self.replace_document(Document::new(Project::default()));
    }

    pub fn open(&mut self, path: &Path) -> Result<(), FormatError> {
        let doc = Document::open(path)?;
        self.replace_document(doc);
        Ok(())
    }

    pub fn replace_document(&mut self, doc: Document) {
        // A take belongs to the document it was recorded for.
        self.discard_recording();
        if let Some(s) = self.engine.session_mut() {
            let _ = s.stop();
            let _ = s.locate(TickTime(0));
            let _ = s.set_loop(None);
        }
        self.doc = doc;
        self.autosave_failed = None;
        self.doc_serial += 1;
        self.arrange.selection.clear();
        self.arrange.selected_track = None;
        self.arrange.view.scroll_x = 0.0;
        self.arrange.scroll_y = 0.0;
        self.resync();
    }

    /// Adds `file` as a clip, copying it into the project bundle first when
    /// there is one.
    pub fn import_audio(
        &mut self,
        file: &Path,
        track: Option<daw_model::TrackId>,
        at: TickTime,
    ) -> Result<(), Message> {
        let owned = || file.to_path_buf();
        let info = Decoder::open(file)
            .map_err(|e| Message::ImportDecode(owned(), e))?
            .info()
            .clone();
        let frames = info.frames.ok_or_else(|| Message::ImportLength(owned()))?;
        let stored = match self.doc.bundle() {
            Some(b) => {
                let rel = b
                    .import_audio(file)
                    .map_err(|e| Message::ImportCopy(owned(), e))?;
                b.resolve(&rel)
            }
            None => file.to_path_buf(),
        };
        let r = self.with_arrange(|a, doc, env| {
            a.import_audio(doc, env, &stored, frames, info.sample_rate, track, at)
        });
        r.map_err(|e| Message::ImportEdit(owned(), e))?;
        self.sync();
        Ok(())
    }
}
