//! `Project`: the QML-facing document's lifecycle: opening, saving,
//! recovering and relinking it, undo, and the status line, over
//! [`crate::state`]. Its `revision` is the one every view follows.
//!
//! The rest of what QML sees of the project has bridges of its own: the
//! track list is [`Tracks`](crate::tracks_bridge), playback and tempo
//! [`Transport`](crate::transport_bridge), and the timeline's edit commands
//! [`Arrangement`](crate::arrangement_bridge).

use std::pin::Pin;

use cxx_qt::CxxQtType;
use cxx_qt_lib::{QMap, QMapPair_QString_QVariant, QString, QStringList, QUrl, QVariant};

use crate::message::{Context, Fields, Message, Reason};
use crate::qt_lists::string_list;
use crate::recovery::Candidate;
use crate::settings::{self, KConfigStore};
use crate::state::{self, AppState};

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qurl.h");
        type QUrl = cxx_qt_lib::QUrl;
        include!("cxx-qt-lib/qmap.h");
        type QMap_QString_QVariant = cxx_qt_lib::QMap<cxx_qt_lib::QMapPair_QString_QVariant>;
        include!("cxx-qt-lib/qstringlist.h");
        type QStringList = cxx_qt_lib::QStringList;
    }

    /// The `context` of a message map: what the user was doing. See
    /// `crate::message::Context`.
    #[qenum(Project)]
    enum MessageContext {
        OpenContext,
        SaveContext,
        AutosaveContext,
        ImportContext,
        EditContext,
        PlaybackContext,
        RecordContext,
        CollectContext,
        RecoverContext,
        ExportContext,
        EditorContext,
    }

    /// The `reason` of a message map. See `crate::message::Reason` for
    /// which other keys each one fills.
    #[qenum(Project)]
    enum MessageReason {
        NotLocalFile,
        FileAccess,
        Malformed,
        NotAProject,
        TooNew,
        NoMigration,
        Inconsistent,
        NoLocation,
        AutosavePending,
        CannotDecode,
        UnknownLength,
        NameNotText,
        EditRefused,
        NoteOutsideClip,
        LaneNotDescribed,
        EngineRefused,
        DeviceStateRejected,
        UnknownDevice,
        PluginNotLoaded,
        PluginMissing,
        PluginLoadFailed,
        PluginCrashed,
        PluginsNotRealtime,
        PluginStateNotSaved,
        PluginEditorFailed,
        NoPluginDisplay,
        CannotStream,
        CannotRead,
        NotesDropped,
        ClipsDropped,
        RoutingLoop,
        NotesUnplayed,
        LaneSkipped,
        NoInstrumentTrack,
        EngineNotRunning,
        NoMidiPlayer,
        MidiInputDropped,
        NothingRecorded,
        Exported,
        ExportCancelled,
        EmptySong,
        NoLoop,
        EmptyRange,
        ExportBusy,
        ExportPluginScan,
        EncodeFailed,
        DiskTooSlow,
        WrongExtension,
        PluginTooSlow,
        ExportPluginCrashed,
        LatencyChanged,
        CannotRecordTo,
        NoSuchInput,
        TrackNotReady,
        RecordingStopped,
        AudioDropped,
        PositionJumped,
        TrackGoneWhileRecording,
        RecordXruns,
        RecordAudioRestarted,
        AlreadyOpen,
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        /// Changes whenever anything shown about the project changed.
        #[qproperty(i32, revision)]
        #[qproperty(QString, name)]
        #[qproperty(bool, modified)]
        /// Whether the project has a bundle on disk (so Save needs no dialog).
        #[qproperty(bool, saved_before)]
        #[qproperty(bool, can_undo)]
        #[qproperty(bool, can_redo)]
        #[qproperty(QString, undo_text)]
        #[qproperty(QString, redo_text)]
        /// The last error or warning, as a message map (see
        /// `message_map`); empty when there is none. QML words it.
        #[qproperty(QMap_QString_QVariant, status)]
        /// Folders of recently used projects, newest first.
        #[qproperty(QStringList, recent_projects)]
        /// How many audio files clips play that are not on disk; see
        /// `missing_files`, `missing_clip_count` and `relink`.
        #[qproperty(i32, missing_count)]
        /// The missing files, as the clips name them; index them for
        /// `missing_clip_count` and `relink`.
        #[qproperty(QStringList, missing_files)]
        /// How many audio files clips play from outside the bundle, which
        /// `collect_files` copies in.
        #[qproperty(i32, external_count)]
        /// How many earlier sessions left unsaved work; see `recovery_name`.
        #[qproperty(i32, recovery_count)]
        type Project = super::ProjectRust;

        /// Starts remembering recent projects and autosaving projects that
        /// were never saved, and looks for unsaved work earlier sessions
        /// left (`recovery_count`). The real app calls this once at
        /// startup; the self test doesn't, so it never touches the user's
        /// files.
        #[qinvokable]
        fn start_session(self: Pin<&mut Project>);
        /// At a clean exit, after the user saved or chose to discard their
        /// changes.
        #[qinvokable]
        fn shutdown(self: Pin<&mut Project>);

        /// The bundle name of unsaved work `index`; empty for a project
        /// that was never saved.
        #[qinvokable]
        fn recovery_name(self: &Project, index: i32) -> QString;
        /// Where unsaved work `index` is kept.
        #[qinvokable]
        fn recovery_path(self: &Project, index: i32) -> QString;
        /// When unsaved work `index` was last autosaved, in milliseconds
        /// since 1970, or 0 if unknown.
        #[qinvokable]
        fn recovery_time(self: &Project, index: i32) -> f64;
        /// Opens unsaved work `index` in place of the current project.
        /// Returns an empty map, or a message map saying why it failed.
        #[qinvokable]
        fn recover_session(self: Pin<&mut Project>, index: i32) -> QMap_QString_QVariant;
        /// Deletes unsaved work `index`.
        #[qinvokable]
        fn discard_session(self: Pin<&mut Project>, index: i32);

        /// Opens the bundle at `url` from its autosave, after open_project
        /// reported `AutosavePending`.
        #[qinvokable]
        fn recover_project(self: Pin<&mut Project>, url: &QUrl) -> QMap_QString_QVariant;
        /// Opens the bundle at `url`, deleting its autosave.
        #[qinvokable]
        fn open_discarding_autosave(self: Pin<&mut Project>, url: &QUrl) -> QMap_QString_QVariant;
        /// Opens the recent project at `url` (from `recent_url`). A project
        /// that is gone is dropped from the list. By URL, not index, since
        /// a save in between can reorder the list.
        #[qinvokable]
        fn open_recent(self: Pin<&mut Project>, url: &QUrl) -> QMap_QString_QVariant;
        /// The URL of recent project `index`, for open_discarding_autosave
        /// and recover_project.
        #[qinvokable]
        fn recent_url(self: &Project, index: i32) -> QUrl;
        #[qinvokable]
        fn forget_recent(self: Pin<&mut Project>, index: i32);

        /// How many clips play missing file `index`.
        #[qinvokable]
        fn missing_clip_count(self: &Project, index: i32) -> i32;
        /// Points the clips playing missing file `index` at the file at
        /// `url`, and other missing files found next to it by name, as one
        /// undoable edit. Returns how many files were relinked.
        #[qinvokable]
        fn relink(self: Pin<&mut Project>, index: i32, url: &QUrl) -> i32;
        /// Copies every audio file clips play from outside the bundle into
        /// it, as one undoable edit. Returns how many files were copied, or
        /// -1 after reporting why it failed in `status`.
        #[qinvokable]
        fn collect_files(self: Pin<&mut Project>) -> i32;

        /// Called by the UI's clock (`UiClock.qml`): picks up engine position
        /// and background work.
        #[qinvokable]
        fn tick(self: Pin<&mut Project>);
        /// Shows an edit made through another bridge (`Tracks`,
        /// `Transport`, `Arrangement`): moves `revision` if the state
        /// changed. Called by the UI's clock.
        #[qinvokable]
        fn sync(self: Pin<&mut Project>);

        #[qinvokable]
        fn new_project(self: Pin<&mut Project>);
        /// Returns an empty map, or a message map saying why it failed.
        #[qinvokable]
        fn open_project(self: Pin<&mut Project>, url: &QUrl) -> QMap_QString_QVariant;
        /// Returns an empty map, or a message map saying why it failed.
        #[qinvokable]
        fn save(self: Pin<&mut Project>) -> QMap_QString_QVariant;
        /// Saves into a new bundle at `url`, its directory or the project
        /// file to go in it. Returns an empty map, or a message map saying
        /// why it failed.
        #[qinvokable]
        fn save_as(self: Pin<&mut Project>, url: &QUrl) -> QMap_QString_QVariant;

        #[qinvokable]
        fn undo(self: Pin<&mut Project>);
        #[qinvokable]
        fn redo(self: Pin<&mut Project>);

        #[qinvokable]
        fn clear_status(self: Pin<&mut Project>);
    }

    impl cxx_qt::Initialize for Project {}
}

#[derive(Default)]
pub struct ProjectRust {
    revision: i32,
    name: QString,
    modified: bool,
    saved_before: bool,
    can_undo: bool,
    can_redo: bool,
    undo_text: QString,
    redo_text: QString,
    status: QMap<QMapPair_QString_QVariant>,
    recent_projects: QStringList,
    missing_count: i32,
    missing_files: QStringList,
    external_count: i32,
    recovery_count: i32,
    /// Missing files, as of the document state in `files_seen`.
    missing: Vec<daw_model::files::AudioFile>,
    /// (`doc_serial`, revision) the file lists were made for.
    files_seen: Option<(u64, u64)>,
    /// Last view revision copied into the properties.
    seen: u64,
}

/// The name the window title shows: the project's own, else the bundle's.
/// Empty for a new project that was never saved; QML words that one.
fn shown_name(p: &daw_model::Project, bundle: Option<&daw_model::Bundle>) -> String {
    if !p.name.is_empty() {
        return p.name.clone();
    }
    bundle.map(|b| b.name().to_owned()).unwrap_or_default()
}

/// A message as QML sees it: `context` and `reason` (the qenums above),
/// `fileName` (of `Fields::file`), `path`, `name`, `detail`, `number` and
/// `number2`.
/// The file on this machine a file dialog's `url` names, for the edit
/// `context` names.
pub fn local_file(url: &QUrl, context: Context) -> Result<std::path::PathBuf, Message> {
    url.to_local_file()
        .map(|p| std::path::PathBuf::from(p.to_string()))
        .ok_or(Message::NotLocalFile(context))
}

pub fn message_map(f: &Fields) -> QMap<QMapPair_QString_QVariant> {
    use qobject::{MessageContext as QC, MessageReason as QR};
    let context = match f.context {
        Context::Open => QC::OpenContext,
        Context::Save => QC::SaveContext,
        Context::Autosave => QC::AutosaveContext,
        Context::Import => QC::ImportContext,
        Context::Edit => QC::EditContext,
        Context::Playback => QC::PlaybackContext,
        Context::Record => QC::RecordContext,
        Context::Collect => QC::CollectContext,
        Context::Recover => QC::RecoverContext,
        Context::Export => QC::ExportContext,
        Context::Editor => QC::EditorContext,
    };
    let reason = match f.reason {
        Reason::NotLocalFile => QR::NotLocalFile,
        Reason::FileAccess => QR::FileAccess,
        Reason::Malformed => QR::Malformed,
        Reason::NotAProject => QR::NotAProject,
        Reason::TooNew => QR::TooNew,
        Reason::NoMigration => QR::NoMigration,
        Reason::Inconsistent => QR::Inconsistent,
        Reason::NoLocation => QR::NoLocation,
        Reason::AutosavePending => QR::AutosavePending,
        Reason::CannotDecode => QR::CannotDecode,
        Reason::UnknownLength => QR::UnknownLength,
        Reason::NameNotText => QR::NameNotText,
        Reason::EditRefused => QR::EditRefused,
        Reason::NoteOutsideClip => QR::NoteOutsideClip,
        Reason::LaneNotDescribed => QR::LaneNotDescribed,
        Reason::EngineRefused => QR::EngineRefused,
        Reason::DeviceStateRejected => QR::DeviceStateRejected,
        Reason::UnknownDevice => QR::UnknownDevice,
        Reason::PluginNotLoaded => QR::PluginNotLoaded,
        Reason::PluginMissing => QR::PluginMissing,
        Reason::PluginLoadFailed => QR::PluginLoadFailed,
        Reason::PluginCrashed => QR::PluginCrashed,
        Reason::PluginsNotRealtime => QR::PluginsNotRealtime,
        Reason::PluginStateNotSaved => QR::PluginStateNotSaved,
        Reason::PluginEditorFailed => QR::PluginEditorFailed,
        Reason::NoPluginDisplay => QR::NoPluginDisplay,
        Reason::CannotStream => QR::CannotStream,
        Reason::CannotRead => QR::CannotRead,
        Reason::NotesDropped => QR::NotesDropped,
        Reason::ClipsDropped => QR::ClipsDropped,
        Reason::RoutingLoop => QR::RoutingLoop,
        Reason::NotesUnplayed => QR::NotesUnplayed,
        Reason::LaneSkipped => QR::LaneSkipped,
        Reason::NoInstrumentTrack => QR::NoInstrumentTrack,
        Reason::EngineNotRunning => QR::EngineNotRunning,
        Reason::NoMidiPlayer => QR::NoMidiPlayer,
        Reason::MidiInputDropped => QR::MidiInputDropped,
        Reason::NothingRecorded => QR::NothingRecorded,
        Reason::Exported => QR::Exported,
        Reason::ExportCancelled => QR::ExportCancelled,
        Reason::EmptySong => QR::EmptySong,
        Reason::NoLoop => QR::NoLoop,
        Reason::EmptyRange => QR::EmptyRange,
        Reason::ExportBusy => QR::ExportBusy,
        Reason::ExportPluginScan => QR::ExportPluginScan,
        Reason::EncodeFailed => QR::EncodeFailed,
        Reason::DiskTooSlow => QR::DiskTooSlow,
        Reason::WrongExtension => QR::WrongExtension,
        Reason::PluginTooSlow => QR::PluginTooSlow,
        Reason::ExportPluginCrashed => QR::ExportPluginCrashed,
        Reason::LatencyChanged => QR::LatencyChanged,
        Reason::CannotRecordTo => QR::CannotRecordTo,
        Reason::NoSuchInput => QR::NoSuchInput,
        Reason::TrackNotReady => QR::TrackNotReady,
        Reason::RecordingStopped => QR::RecordingStopped,
        Reason::AudioDropped => QR::AudioDropped,
        Reason::PositionJumped => QR::PositionJumped,
        Reason::TrackGoneWhileRecording => QR::TrackGoneWhileRecording,
        Reason::RecordXruns => QR::RecordXruns,
        Reason::RecordAudioRestarted => QR::RecordAudioRestarted,
        Reason::AlreadyOpen => QR::AlreadyOpen,
    };
    let text = |s: &str| QVariant::from(&QString::from(s));
    // Numbers are schema versions or lengths in seconds; they fit an i32.
    let number = |n: u64| QVariant::from(&i32::try_from(n).unwrap_or(i32::MAX));
    let path = f
        .path
        .as_deref()
        .map_or_else(String::new, |p| p.display().to_string());
    let file_name = f.file.as_deref().map_or_else(String::new, |p| {
        p.file_name().map_or_else(
            || p.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        )
    });
    let mut map = QMap::default();
    map.insert(QString::from("context"), QVariant::from(&context.repr));
    map.insert(QString::from("reason"), QVariant::from(&reason.repr));
    map.insert(QString::from("path"), text(&path));
    map.insert(QString::from("fileName"), text(&file_name));
    map.insert(QString::from("name"), text(&f.name));
    map.insert(QString::from("detail"), text(&f.detail));
    map.insert(QString::from("number"), number(f.number));
    map.insert(QString::from("number2"), number(f.number2));
    map
}

fn message_or_empty(fields: Option<&Fields>) -> QMap<QMapPair_QString_QVariant> {
    fields.map_or_else(QMap::default, message_map)
}

impl cxx_qt::Initialize for qobject::Project {
    fn initialize(self: Pin<&mut Self>) {
        self.refresh(true);
    }
}

impl qobject::Project {
    /// Copies state into the properties when `all` or when the view
    /// revision moved.
    fn refresh(mut self: Pin<&mut Self>, all: bool) {
        // What the properties show, read while the state is borrowed.
        struct Shown {
            revision: u64,
            name: String,
            modified: bool,
            saved_before: bool,
            can_undo: bool,
            can_redo: bool,
            undo_text: String,
            redo_text: String,
            status: Option<Fields>,
        }
        let seen = self.seen;
        let shown = state::with(|s| {
            s.sync();
            if !all && s.view_revision == seen {
                return None;
            }
            let h = s.doc.history();
            Some(Shown {
                revision: s.view_revision,
                name: shown_name(s.doc.project(), s.doc.bundle()),
                modified: s.doc.is_modified() || s.has_pending_plugin_changes(),
                saved_before: s.doc.bundle().is_some(),
                can_undo: h.can_undo(),
                can_redo: h.can_redo(),
                undo_text: h.undo_label().unwrap_or_default(),
                redo_text: h.redo_label().unwrap_or_default(),
                status: s.status.clone(),
            })
        });
        let Some(shown) = shown else {
            return;
        };
        self.as_mut().rust_mut().seen = shown.revision;
        let mut this = self.as_mut();
        this.as_mut().set_name(QString::from(shown.name.as_str()));
        this.as_mut().set_modified(shown.modified);
        this.as_mut().set_saved_before(shown.saved_before);
        this.as_mut().set_can_undo(shown.can_undo);
        this.as_mut().set_can_redo(shown.can_redo);
        this.as_mut()
            .set_undo_text(QString::from(shown.undo_text.as_str()));
        this.as_mut()
            .set_redo_text(QString::from(shown.redo_text.as_str()));
        this.as_mut()
            .set_status(message_or_empty(shown.status.as_ref()));
        this.as_mut().refresh_files();
        // Last, so QML that reacts to it sees the other values already.
        this.set_revision((shown.revision % i32::MAX as u64) as i32);
    }

    /// The recent list, and the missing and external files when the
    /// document changed.
    fn refresh_files(mut self: Pin<&mut Self>) {
        let seen = self.files_seen;
        let (recent, files, recovery) = state::with(|s| {
            let recent: Vec<String> = s.recent.iter().map(|p| p.display().to_string()).collect();
            let at = (s.doc_serial, s.doc.revision());
            let files =
                (seen != Some(at)).then(|| (at, s.missing_files(), s.external_file_count()));
            (recent, files, s.recovery.len())
        });
        self.as_mut()
            .set_recovery_count(i32::try_from(recovery).unwrap_or(i32::MAX));
        let list = string_list(&recent);
        if self.recent_projects != list {
            self.as_mut().set_recent_projects(list);
        }
        if let Some((at, missing, external)) = files {
            let count = i32::try_from(missing.len()).unwrap_or(i32::MAX);
            {
                let mut r = self.as_mut().rust_mut();
                r.files_seen = Some(at);
                r.missing = missing;
            }
            let names = string_list(self.missing.iter().map(|f| f.source.display().to_string()));
            self.as_mut().set_missing_files(names);
            self.as_mut().set_missing_count(count);
            self.as_mut()
                .set_external_count(i32::try_from(external).unwrap_or(i32::MAX));
        }
    }

    /// Runs `f` on the state, then refreshes every property.
    fn edit(self: Pin<&mut Self>, f: impl FnOnce(&mut AppState)) {
        state::with(|s| s.edit(f));
        self.refresh(true);
    }

    fn tick(self: Pin<&mut Self>) {
        state::with(|s| {
            s.tick_engine();
            s.tick_mixer();
            s.tick_plugins();
            // An update a full player queue refused; the queue has drained.
            if s.engine
                .session()
                .is_some_and(|session| session.wants_sync())
            {
                s.resync();
            }
            if s.arrange.peaks.poll() {
                s.touch();
            }
            // Live MIDI goes where recording would, while nothing is armed.
            let focus = crate::recording::input_focus(s);
            if let Some(session) = s.engine.session_mut() {
                session.set_input_focus(focus);
            }
            if s.engine.is_playing() && s.follow_playhead && !s.arrange.dragging() {
                let at = s.playhead();
                if s.arrange.follow(at) {
                    s.touch();
                }
            }
            if s.is_recording() {
                s.poll_recording();
                if !s.is_recording() {
                    s.sync();
                    s.touch();
                }
            }
            s.autosave();
        });
        self.refresh(false);
    }

    fn sync(self: Pin<&mut Self>) {
        self.refresh(false);
    }

    fn new_project(self: Pin<&mut Self>) {
        self.edit(AppState::new_document);
    }

    fn open_project(self: Pin<&mut Self>, url: &QUrl) -> QMap<QMapPair_QString_QVariant> {
        let Some(path) = url.to_local_file() else {
            return message_map(&Message::NotLocalFile(Context::Open).fields());
        };
        let path = std::path::PathBuf::from(path.to_string());
        self.open_with(&path, AppState::open)
    }

    fn open_with(
        mut self: Pin<&mut Self>,
        path: &std::path::Path,
        open: fn(&mut AppState, &std::path::Path) -> Result<(), Message>,
    ) -> QMap<QMapPair_QString_QVariant> {
        let mut result = None;
        self.as_mut().edit(|s| {
            if let Err(e) = open(s, path) {
                result = Some(e.fields());
            }
        });
        // Recheck the files on disk even for the same document.
        self.as_mut().rust_mut().files_seen = None;
        self.refresh_files();
        message_or_empty(result.as_ref())
    }

    fn local_path(url: &QUrl, context: Context) -> Result<std::path::PathBuf, Fields> {
        local_file(url, context).map_err(|e| e.fields())
    }

    fn recover_project(self: Pin<&mut Self>, url: &QUrl) -> QMap<QMapPair_QString_QVariant> {
        match Self::local_path(url, Context::Recover) {
            Ok(path) => self.open_with(&path, AppState::recover),
            Err(f) => message_map(&f),
        }
    }

    fn open_discarding_autosave(
        self: Pin<&mut Self>,
        url: &QUrl,
    ) -> QMap<QMapPair_QString_QVariant> {
        match Self::local_path(url, Context::Open) {
            Ok(path) => self.open_with(&path, AppState::open_discarding_autosave),
            Err(f) => message_map(&f),
        }
    }

    fn recent_path(index: i32) -> Option<std::path::PathBuf> {
        state::with(|s| {
            usize::try_from(index)
                .ok()
                .and_then(|i| s.recent.get(i).cloned())
        })
    }

    fn open_recent(mut self: Pin<&mut Self>, url: &QUrl) -> QMap<QMapPair_QString_QVariant> {
        let path = match Self::local_path(url, Context::Open) {
            Ok(p) => p,
            Err(f) => return message_map(&f),
        };
        let result = self.as_mut().open_with(&path, AppState::open);
        if !path.exists() {
            self.edit(|s| s.forget_recent(&path));
        }
        result
    }

    fn recent_url(&self, index: i32) -> QUrl {
        Self::recent_path(index).map_or_else(QUrl::default, |p| {
            QUrl::from_local_file(&QString::from(p.display().to_string().as_str()))
        })
    }

    fn forget_recent(self: Pin<&mut Self>, index: i32) {
        let Some(path) = Self::recent_path(index) else {
            return;
        };
        self.edit(|s| s.forget_recent(&path));
    }

    fn start_session(self: Pin<&mut Self>) {
        self.edit(|s| {
            s.start_session(
                Box::new(KConfigStore),
                crate::recovery::default_base().as_deref(),
            );
            s.start_plugin_scan(settings::load_plugin_folders(&KConfigStore));
        });
    }

    fn shutdown(self: Pin<&mut Self>) {
        state::with(AppState::shutdown);
    }

    fn candidate<R>(index: i32, f: impl FnOnce(&Candidate) -> R) -> Option<R> {
        state::with(|s| {
            usize::try_from(index)
                .ok()
                .and_then(|i| s.recovery.get(i))
                .map(f)
        })
    }

    fn recovery_name(&self, index: i32) -> QString {
        Self::candidate(index, |c| QString::from(c.name().as_str())).unwrap_or_default()
    }

    fn recovery_path(&self, index: i32) -> QString {
        Self::candidate(index, |c| {
            QString::from(c.autosave.display().to_string().as_str())
        })
        .unwrap_or_default()
    }

    fn recovery_time(&self, index: i32) -> f64 {
        Self::candidate(index, |c| c.modified)
            .flatten()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0.0, |d| d.as_millis() as f64)
    }

    fn recover_session(mut self: Pin<&mut Self>, index: i32) -> QMap<QMapPair_QString_QVariant> {
        let Ok(index) = usize::try_from(index) else {
            return QMap::default();
        };
        let mut result = None;
        self.as_mut().edit(|s| {
            if let Err(e) = s.recover_offered(index) {
                result = Some(e.fields());
            }
        });
        // Recheck the files on disk even for the same document.
        self.as_mut().rust_mut().files_seen = None;
        self.refresh_files();
        message_or_empty(result.as_ref())
    }

    fn discard_session(self: Pin<&mut Self>, index: i32) {
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        self.edit(|s| {
            if let Err(e) = s.discard_offered(index) {
                s.report(e);
            }
        });
    }

    fn missing_entry(&self, index: i32) -> Option<&daw_model::files::AudioFile> {
        usize::try_from(index)
            .ok()
            .and_then(|i| self.missing.get(i))
    }

    fn missing_clip_count(&self, index: i32) -> i32 {
        self.missing_entry(index)
            .map_or(0, |f| i32::try_from(f.clips.len()).unwrap_or(i32::MAX))
    }

    fn relink(mut self: Pin<&mut Self>, index: i32, url: &QUrl) -> i32 {
        let Some(source) = self.missing_entry(index).map(|f| f.source.clone()) else {
            return 0;
        };
        let file = match Self::local_path(url, Context::Edit) {
            Ok(f) => f,
            Err(f) => {
                self.edit(|s| s.status = Some(f));
                return 0;
            }
        };
        let mut n = 0;
        self.as_mut().edit(|s| match s.relink(&source, &file) {
            Ok(count) => n = count,
            Err(e) => s.report(e),
        });
        i32::try_from(n).unwrap_or(i32::MAX)
    }

    fn collect_files(mut self: Pin<&mut Self>) -> i32 {
        let mut n = -1;
        self.as_mut().edit(|s| match s.collect_files() {
            Ok(count) => n = i32::try_from(count).unwrap_or(i32::MAX),
            Err(e) => s.report(e),
        });
        n
    }

    fn save(self: Pin<&mut Self>) -> QMap<QMapPair_QString_QVariant> {
        let mut result = None;
        self.edit(|s| {
            if let Err(e) = s.save() {
                result = Some(Message::File(Context::Save, e).fields());
            }
        });
        message_or_empty(result.as_ref())
    }

    fn save_as(mut self: Pin<&mut Self>, url: &QUrl) -> QMap<QMapPair_QString_QVariant> {
        let Some(path) = url.to_local_file() else {
            return message_map(&Message::NotLocalFile(Context::Save).fields());
        };
        // A file dialog may hand back "Name.kadence": the document works
        // out which bundle that means.
        let path = std::path::PathBuf::from(path.to_string());
        let mut result = None;
        self.as_mut().edit(|s| {
            if let Err(e) = s.save_as(&path) {
                result = Some(Message::File(Context::Save, e).fields());
            }
        });
        // Files now resolve against the bundle, and may be outside it.
        self.as_mut().rust_mut().files_seen = None;
        self.as_mut().refresh_files();
        message_or_empty(result.as_ref())
    }

    fn undo(self: Pin<&mut Self>) {
        self.edit(|s| {
            if let Err(e) = s.doc.undo() {
                s.report(e.into());
            }
        });
    }

    fn redo(self: Pin<&mut Self>) {
        self.edit(|s| {
            if let Err(e) = s.doc.redo() {
                s.report(e.into());
            }
        });
    }

    fn clear_status(self: Pin<&mut Self>) {
        self.edit(|s| s.status = None);
    }
}
