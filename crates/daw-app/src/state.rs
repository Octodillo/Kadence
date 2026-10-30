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
use crate::recovery::{self, Candidate, DirLock, Scratch};
use crate::settings::{self, MemoryStore, SettingsStore};

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
    /// Where the recent list is kept: in memory until [`AppState::start_session`]
    /// hands over the real config, so tests never touch it.
    settings: Box<dyn SettingsStore>,
    /// Recently used bundles, newest first.
    pub recent: Vec<PathBuf>,
    /// Where a never-saved project autosaves; `None` until the session
    /// starts, or if the folder could not be made.
    scratch: Option<Scratch>,
    /// The (`doc_serial`, revision) last written to `scratch`.
    scratch_saved: Option<(u64, u64)>,
    /// Held on the open bundle so other Kadence windows know it is in use.
    bundle_lock: Option<DirLock>,
    /// Unsaved work earlier sessions left, newest first, to offer.
    pub recovery: Vec<Candidate>,
    /// Set by [`AppState::shutdown`]: autosave no more, so a last timer
    /// tick can't bring back what the user chose to discard.
    shut_down: bool,
}

/// `path` with symlinks resolved, so two spellings of one folder compare
/// equal; as it is if that fails.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
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
            synced: None,
            view_revision: 0,
            status: None,
            autosave_failed: None,
            follow_playhead: true,
            fallback_tempo: SegmentTempoMap::default(),
            doc_serial: 0,
            settings: Box::new(MemoryStore::default()),
            recent: Vec::new(),
            scratch: None,
            scratch_saved: None,
            bundle_lock: None,
            recovery: Vec::new(),
            shut_down: false,
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
        if self.shut_down {
            return;
        }
        if self.doc.bundle().is_none() {
            self.autosave_scratch();
            return;
        }
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

    /// [`AppState::autosave`] for a project with no bundle: into the scratch
    /// folder, removed again when the project is back to unmodified.
    fn autosave_scratch(&mut self) {
        const RETRY: Duration = Duration::from_secs(10);
        let Some(scratch) = &self.scratch else {
            return;
        };
        let at = (self.doc_serial, self.doc.revision());
        if self.scratch_saved == Some(at) {
            return;
        }
        let revision = self.doc.revision();
        let failed_here = matches!(self.autosave_failed, Some((r, _)) if r == revision);
        if let Some((r, when)) = self.autosave_failed
            && r == revision
            && when.elapsed() < RETRY
        {
            return;
        }
        let result = if self.doc.is_modified() {
            scratch.write(self.doc.project())
        } else {
            scratch.discard().map_err(|e| daw_model::FormatError::Io {
                path: scratch.file(),
                source: e,
            })
        };
        match result {
            Ok(()) => {
                self.scratch_saved = Some(at);
                self.autosave_failed = None;
            }
            Err(e) => {
                self.autosave_failed = Some((revision, Instant::now()));
                if !failed_here {
                    self.report(Message::File(Context::Autosave, e));
                }
            }
        }
    }

    /// Hands over the user's config and the scratch folder, and lists the
    /// unsaved work earlier sessions left behind in `recovery`. Until this
    /// is called (as in tests and the self test) nothing is remembered or
    /// autosaved outside a bundle.
    pub fn start_session(&mut self, store: Box<dyn SettingsStore>, recovery_base: Option<&Path>) {
        self.settings = store;
        self.recent = settings::load_recent(self.settings.as_ref());
        // Look before making our own folder, so it is not offered.
        self.recovery = recovery::find(recovery_base, &self.recent);
        self.scratch = recovery_base.and_then(|b| Scratch::create(b).ok());
        self.scratch_saved = None;
        self.touch();
    }

    /// Takes unsaved work `index` off the offer.
    pub fn take_recovery(&mut self, index: usize) -> Option<Candidate> {
        (index < self.recovery.len()).then(|| self.recovery.remove(index))
    }

    /// At a clean exit, after the user saved or chose to discard: drop the
    /// autosave so the next start doesn't offer it, and the scratch folder.
    pub fn shutdown(&mut self) {
        self.shut_down = true;
        self.discard_autosave();
        self.bundle_lock = None;
        if let Some(s) = self.scratch.take() {
            s.remove();
        }
    }

    /// Deletes the open document's autosave if it has unsaved changes: the
    /// user chose to drop them.
    fn discard_autosave(&mut self) {
        if !self.doc.is_modified() {
            return;
        }
        match self.doc.bundle() {
            Some(b) => {
                let _ = b.discard_autosave();
            }
            None => {
                if let Some(s) = &self.scratch {
                    let _ = s.discard();
                }
            }
        }
        self.scratch_saved = None;
    }

    /// Puts the open bundle first in the recent list.
    fn remember(&mut self) {
        if let Some(root) = self.root() {
            self.recent = settings::add_recent(self.settings.as_mut(), &root);
        }
    }

    pub fn forget_recent(&mut self, bundle: &Path) {
        self.recent = settings::remove_recent(self.settings.as_mut(), bundle);
        self.touch();
    }

    /// Locks the open bundle, letting go of the one before.
    fn relock(&mut self) {
        let root = self.root().map(|r| canonical(&r));
        if self.bundle_lock.as_ref().map(|l| l.dir()) == root.as_deref() {
            return;
        }
        self.bundle_lock = None;
        self.bundle_lock = root.and_then(|r| DirLock::try_lock(&r).ok().flatten());
    }

    /// The bundle at `path` (its folder or project file), if another Kadence
    /// has it open.
    fn open_elsewhere(&self, path: &Path) -> Option<PathBuf> {
        let root = daw_model::Bundle::open(path).ok()?.root().to_path_buf();
        let ours = self
            .bundle_lock
            .as_ref()
            .is_some_and(|l| l.dir() == canonical(&root));
        (!ours && recovery::is_locked(&root)).then_some(root)
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

    /// Opens a bundle. Fails with [`FormatError::AutosavePending`] when it
    /// holds unsaved changes from an earlier session: ask, then call
    /// [`AppState::recover`] or [`AppState::open_discarding_autosave`].
    pub fn open(&mut self, path: &Path) -> Result<(), Message> {
        self.open_with(path, true, |p| Document::open(p))
    }

    pub fn open_discarding_autosave(&mut self, path: &Path) -> Result<(), Message> {
        self.open_with(path, true, |p| Document::open_discarding_autosave(p))
    }

    /// Opens a bundle from its autosave.
    pub fn recover(&mut self, path: &Path) -> Result<(), Message> {
        self.open_with(path, false, |p| Document::recover(p))
    }

    /// Opens a bundle with `open`. With `reopen_drops_changes`, reopening
    /// the open project first drops its autosave: the user chose to discard
    /// its changes, so they are not unsaved work to offer back.
    fn open_with(
        &mut self,
        path: &Path,
        reopen_drops_changes: bool,
        open: fn(&Path) -> Result<Document, FormatError>,
    ) -> Result<(), Message> {
        if let Some(root) = self.open_elsewhere(path) {
            return Err(Message::AlreadyOpen(root));
        }
        let same = daw_model::Bundle::open(path).is_ok_and(|b| {
            self.doc
                .bundle()
                .is_some_and(|o| canonical(o.root()) == canonical(b.root()))
        });
        // Loads the saved version first, so a failed open keeps the autosave.
        let open: fn(&Path) -> Result<Document, FormatError> =
            if same && reopen_drops_changes && self.doc.is_modified() {
                |p: &Path| Document::open_discarding_autosave(p)
            } else {
                open
            };
        let doc = open(path).map_err(|e| Message::File(Context::Open, e))?;
        self.replace_document(doc);
        self.remember();
        Ok(())
    }

    /// Recovers a project that was never saved from the autosave a crashed
    /// Kadence left in its scratch folder, and removes that folder.
    pub fn recover_untitled(&mut self, candidate: &Candidate) -> Result<(), Message> {
        let project = daw_model::bundle::read_project(&candidate.autosave)
            .map_err(|e| Message::File(Context::Recover, e))?;
        self.replace_document(Document::recovered(project));
        // Take over its folder, so the work stays in one place and goes
        // away when it is saved or discarded.
        if let Some(adopted) = candidate
            .autosave
            .parent()
            .and_then(|d| Scratch::adopt(d).ok().flatten())
            && let Some(own) = self.scratch.replace(adopted)
        {
            own.remove();
        }
        self.scratch_saved = None;
        self.autosave_scratch();
        Ok(())
    }

    /// Saves into a new bundle at `dir`.
    pub fn save_as(&mut self, dir: &Path) -> Result<(), FormatError> {
        let was_untitled = self.doc.bundle().is_none();
        self.doc.save_as(dir)?;
        if was_untitled && let Some(s) = &self.scratch {
            let _ = s.discard();
        }
        self.scratch_saved = None;
        self.relock();
        self.remember();
        Ok(())
    }

    /// Replaces the open document. The old one's unsaved changes are
    /// dropped, with their autosave: callers ask the user first.
    pub fn replace_document(&mut self, doc: Document) {
        // A take belongs to the document it was recorded for.
        self.discard_recording();
        self.discard_autosave();
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
        self.scratch_saved = None;
        self.relock();
        self.resync();
    }

    /// The audio files clips play that are not on disk.
    pub fn missing_files(&self) -> Vec<daw_model::files::AudioFile> {
        daw_model::files::missing_audio(self.doc.project(), self.doc.bundle())
    }

    /// How many files clips play from outside the bundle, which Collect
    /// Files would copy in. 0 without a bundle.
    pub fn external_file_count(&self) -> usize {
        self.doc.bundle().map_or(0, |b| {
            daw_model::files::external_audio(self.doc.project(), b).len()
        })
    }

    /// Points the clips playing the missing file `source` at `file`, and
    /// any other missing file found next to it by name. Returns how many
    /// files were relinked.
    pub fn relink(&mut self, source: &Path, file: &Path) -> Result<usize, Message> {
        use daw_model::files;
        let bundle = self.doc.bundle();
        let missing = self.missing_files();
        let mut changes = vec![(source.to_path_buf(), files::source_for(file, bundle))];
        if let Some(folder) = file.parent() {
            let others: Vec<_> = missing.into_iter().filter(|f| f.source != source).collect();
            changes.extend(
                files::find_in_folder(&others, folder)
                    .into_iter()
                    .map(|(from, to)| (from, files::source_for(&to, bundle))),
            );
        }
        let Some(cmd) = files::relink(self.doc.project(), &changes, "Relink Audio") else {
            return Ok(0);
        };
        self.doc.execute(cmd)?;
        self.doc.seal();
        // The sync reports what is still missing, not what was.
        self.status = None;
        self.sync();
        Ok(changes.len())
    }

    /// Copies every file clips play from outside the bundle into it.
    /// Returns how many files were copied.
    pub fn collect_files(&mut self) -> Result<usize, Message> {
        let bundle = self
            .doc
            .bundle()
            .ok_or(Message::File(Context::Collect, FormatError::NoBundle))?;
        let Some(collected) = daw_model::files::collect_files(self.doc.project(), bundle)
            .map_err(|e| Message::File(Context::Collect, e))?
        else {
            return Ok(0);
        };
        let n = collected.copies.len();
        if let Err(e) = self.doc.execute(collected.command) {
            daw_model::files::remove_copies(&collected.copies);
            return Err(e.into());
        }
        self.doc.seal();
        self.sync();
        Ok(n)
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
