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
    /// Plugin devices that reported a state change, and when they last did.
    /// The change is recorded once the plugin has been quiet a moment, so a
    /// knob turned in its editor is one undo step.
    plugin_changes: std::collections::HashMap<daw_model::DeviceId, Instant>,
    /// How plugin editor windows are titled: `%1` is the device, `%2` the
    /// track. Set from QML in the user's language.
    pub editor_title_format: String,
    /// The master track's name, for editor titles, in the user's language.
    pub master_name: String,
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
            audio_recording: None,
            synced: None,
            view_revision: 0,
            status: None,
            autosave_failed: None,
            follow_playhead: true,
            fallback_tempo: SegmentTempoMap::default(),
            doc_serial: 0,
            plugin_changes: std::collections::HashMap::new(),
            editor_title_format: "%1 — %2".into(),
            master_name: "Master".into(),
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
        self.retitle_editors();
    }

    /// The title of a plugin device's editor window: the device and its
    /// track.
    pub fn editor_title(&self, device: daw_model::DeviceId) -> Option<String> {
        let project = self.doc.project();
        let master = project.master.id;
        let (track, d) = project
            .all_tracks()
            .find_map(|t| t.devices.iter().find(|d| d.id == device).map(|d| (t, d)))?;
        let device_name = match (&d.source, d.name.is_empty()) {
            (_, false) => d.name.clone(),
            (daw_model::DeviceSource::Plugin { id, .. }, true) => id.clone(),
            (daw_model::DeviceSource::Builtin { kind }, true) => kind.clone(),
        };
        let track_name = if track.id == master && track.name.is_empty() {
            self.master_name.clone()
        } else {
            track.name.clone()
        };
        Some(
            self.editor_title_format
                .replacen("%1", &device_name, 1)
                .replacen("%2", &track_name, 1),
        )
    }

    /// Opens a plugin device's own editor window. Returns false, and says
    /// why in the status line, if it would not open.
    pub fn open_plugin_editor(&mut self, device: daw_model::DeviceId) -> bool {
        self.sync();
        let Some(title) = self.editor_title(device) else {
            return false;
        };
        let opened = match self.engine.session_mut() {
            Some(session) => {
                // What the editor starts from, so undoing its first change
                // goes back here.
                let baseline = session.plugin_state(device);
                session
                    .open_plugin_editor(device, &title)
                    .map(|()| baseline)
            }
            None => Err("audio is not running".into()),
        };
        match opened {
            Ok(baseline) => {
                if let Some(Ok(state)) = baseline
                    && self.doc.store_plugin_state(device, Some(state)).is_ok()
                {
                    self.sync();
                }
                self.touch();
                true
            }
            Err(detail) => {
                let device = self.device_name(device);
                self.report(Message::Warning(daw_session::Warning::PluginEditorFailed {
                    device,
                    detail,
                }));
                false
            }
        }
    }

    fn device_name(&self, device: daw_model::DeviceId) -> String {
        self.doc
            .project()
            .all_tracks()
            .flat_map(|t| t.devices.iter())
            .find(|d| d.id == device)
            .map(|d| d.name.clone())
            .unwrap_or_default()
    }

    /// Closes a plugin device's editor window; its last changes are
    /// recorded on the next ticks.
    pub fn close_plugin_editor(&mut self, device: daw_model::DeviceId) {
        if let Some(session) = self.engine.session_mut() {
            session.close_plugin_editor(device);
        }
        self.touch();
    }

    /// Whether a plugin device's editor window is open.
    pub fn plugin_editor_open(&self, device: daw_model::DeviceId) -> bool {
        self.engine
            .session()
            .is_some_and(|s| s.plugin_editor_open(device))
    }

    /// Whether a plugin device has an editor window of its own.
    pub fn plugin_has_editor(&self, device: daw_model::DeviceId) -> bool {
        self.engine
            .session()
            .is_some_and(|s| s.plugin_has_editor(device))
    }

    /// Keeps open editor windows titled after their device and track, which
    /// may have been renamed.
    pub fn retitle_editors(&mut self) {
        let Some(session) = self.engine.session() else {
            return;
        };
        let open: Vec<daw_model::DeviceId> = self
            .doc
            .project()
            .all_tracks()
            .flat_map(|t| t.devices.iter())
            .filter(|d| matches!(d.source, daw_model::DeviceSource::Plugin { .. }))
            .map(|d| d.id)
            .filter(|&id| session.plugin_editor_open(id))
            .collect();
        for id in open {
            if let Some(title) = self.editor_title(id)
                && let Some(session) = self.engine.session_mut()
            {
                session.set_plugin_editor_title(id, &title);
            }
        }
    }

    /// Records the state of plugins that reported a change and have been
    /// quiet since, one "Change Plugin State" step each.
    fn record_plugin_changes(&mut self, reported: Vec<daw_model::DeviceId>) {
        const QUIET: Duration = Duration::from_millis(300);
        let now = Instant::now();
        for device in reported {
            self.plugin_changes.insert(device, now);
        }
        if self.plugin_changes.is_empty() {
            return;
        }
        let mut due: Vec<daw_model::DeviceId> = self
            .plugin_changes
            .iter()
            .filter(|&(_, &at)| now.duration_since(at) >= QUIET)
            .map(|(&d, _)| d)
            .collect();
        due.sort_by_key(|d| d.0);
        for device in due {
            self.plugin_changes.remove(&device);
            self.record_plugin_state(device);
        }
    }

    /// Records a plugin device's state, if it changed, as an undo step.
    fn record_plugin_state(&mut self, device: daw_model::DeviceId) {
        // The plugin's state is compared with the document's as the engine
        // last had it.
        self.sync();
        let Some(session) = self.engine.session_mut() else {
            return;
        };
        match session.plugin_state(device) {
            None => {}
            Some(Ok(state)) => {
                // Its own step, not merged into an earlier change of the
                // same plugin.
                self.doc.seal();
                let result = self.doc.execute(daw_model::commands::SetDevice {
                    id: device,
                    field: daw_model::commands::DeviceField::State(Some(state)),
                });
                self.doc.seal();
                match result {
                    // So the next report compares against this state.
                    Ok(_) => self.sync(),
                    // Removed since it reported: nothing to keep the state for.
                    Err(daw_model::ModelError::DeviceNotFound(_)) => {}
                    Err(e) => self.report(e.into()),
                }
            }
            Some(Err(detail)) => {
                let device = self.device_name(device);
                self.report(Message::Warning(
                    daw_session::Warning::PluginStateNotSaved { device, detail },
                ));
            }
        }
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

    /// Runs the plugins' main-thread work and frees what the audio thread
    /// is done with. Called by the UI's refresh timer, about 60 times a
    /// second.
    pub fn tick_engine(&mut self) {
        let Some(session) = self.engine.session_mut() else {
            return;
        };
        let report = session.tick();
        self.record_plugin_changes(report.state_changed);
        if let Some(w) = report.warnings.into_iter().next() {
            self.report(Message::Warning(w));
        }
        if let Some(e) = report.error {
            // Retried on later ticks; shown once.
            let fields = Message::Playback(e).fields();
            if self.status.as_ref() != Some(&fields) {
                self.status = Some(fields);
                self.touch();
            }
        }
    }

    /// Stores each plugin's current state in the document, so saving keeps
    /// it. Not an undo step: the plugin changed, not the project, so undo
    /// must not throw a plugin's own changes away.
    pub fn store_plugin_states(&mut self) {
        let Some(session) = self.engine.session_mut() else {
            return;
        };
        let mut stored = false;
        for s in session.plugin_states() {
            match s.state {
                // The device was synced moments ago; if it is gone anyway, there is nothing
                // to keep its state for.
                Ok(state) => stored |= self.doc.store_plugin_state(s.device, Some(state)).is_ok(),
                Err(detail) => {
                    let device = self
                        .doc
                        .project()
                        .all_tracks()
                        .flat_map(|t| t.devices.iter())
                        .find(|d| d.id == s.device)
                        .map(|d| d.name.clone())
                        .unwrap_or_default();
                    self.report(Message::Warning(
                        daw_session::Warning::PluginStateNotSaved { device, detail },
                    ));
                }
            }
        }
        if stored {
            self.sync();
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
            // The new document's plugins start from its own saved state, even where it
            // has the same devices (reopening a project).
            s.clear_plugins();
        }
        self.doc = doc;
        self.autosave_failed = None;
        // Changes of the old document's plugins, whose editors closed with them.
        self.plugin_changes.clear();
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
