//! `Project`: the QML-facing document. File handling, undo, tracks and the
//! transport, over [`crate::state`].

use std::pin::Pin;

use cxx_qt::CxxQtType;
use cxx_qt_lib::{
    QColor, QList, QMap, QMapPair_QString_QVariant, QString, QStringList, QUrl, QVariant,
};
use daw_core::time::{PPQ, TickTime};
use daw_model::commands::{MoveTrack, RemoveTrack, SetTrack, TrackField};
use daw_model::{FadeShape, InputChannels, Monitoring, Track, TrackKind};

use crate::instrument;
use crate::message::{Context, Fields, Message, Reason};
use crate::recovery::Candidate;
use crate::state::{self, AppState};

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qcolor.h");
        type QColor = cxx_qt_lib::QColor;
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
        EditRefused,
        NoteOutsideClip,
        EngineRefused,
        DeviceStateRejected,
        UnknownDevice,
        PluginNotLoaded,
        PluginMissing,
        PluginLoadFailed,
        PluginCrashed,
        PluginStateNotSaved,
        PluginEditorFailed,
        CannotStream,
        CannotRead,
        NotesDropped,
        ClipsDropped,
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
        AlreadyOpen,
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        /// Changes whenever anything shown about the project changed.
        #[qproperty(i32, revision)]
        #[qproperty(i32, track_count)]
        #[qproperty(QString, name)]
        #[qproperty(bool, modified)]
        /// Whether the project has a bundle on disk (so Save needs no dialog).
        #[qproperty(bool, saved_before)]
        #[qproperty(bool, can_undo)]
        #[qproperty(bool, can_redo)]
        #[qproperty(QString, undo_text)]
        #[qproperty(QString, redo_text)]
        #[qproperty(bool, playing)]
        /// Takes are being recorded.
        #[qproperty(bool, recording)]
        #[qproperty(bool, looping)]
        /// Bar.beat of the playhead, like "12.3".
        #[qproperty(QString, position_text)]
        /// Minutes:seconds of the playhead.
        #[qproperty(QString, time_text)]
        #[qproperty(bool, has_selection)]
        /// The selection holds an audio clip (for clip gain and fades).
        #[qproperty(bool, audio_selected)]
        #[qproperty(i32, selected_track)]
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

        /// Called by a timer: picks up engine position and background work.
        #[qinvokable]
        fn tick(self: Pin<&mut Project>);

        #[qinvokable]
        fn new_project(self: Pin<&mut Project>);
        /// Returns an empty map, or a message map saying why it failed.
        #[qinvokable]
        fn open_project(self: Pin<&mut Project>, url: &QUrl) -> QMap_QString_QVariant;
        /// Returns an empty map, or a message map saying why it failed.
        #[qinvokable]
        fn save(self: Pin<&mut Project>) -> QMap_QString_QVariant;
        /// Saves into a new bundle directory at `url`. Returns an empty map,
        /// or a message map saying why it failed.
        #[qinvokable]
        fn save_as(self: Pin<&mut Project>, url: &QUrl) -> QMap_QString_QVariant;

        #[qinvokable]
        fn undo(self: Pin<&mut Project>);
        #[qinvokable]
        fn redo(self: Pin<&mut Project>);

        /// Adds a track of `kind` (0 audio, 1 instrument, 2 bus) called
        /// `name`; QML words the default name with `next_track_number`.
        #[qinvokable]
        fn add_track(self: Pin<&mut Project>, kind: i32, name: &QString);
        /// The number for the next default track name of `kind`, as in
        /// "Audio 3".
        #[qinvokable]
        fn next_track_number(self: &Project, kind: i32) -> i32;
        #[qinvokable]
        fn remove_track(self: Pin<&mut Project>, index: i32);
        #[qinvokable]
        fn track_name(self: &Project, index: i32) -> QString;
        /// 0 audio, 1 instrument, 2 bus, -1 none.
        #[qinvokable]
        fn track_kind(self: &Project, index: i32) -> i32;
        #[qinvokable]
        fn track_color(self: &Project, index: i32) -> QColor;
        #[qinvokable]
        fn track_muted(self: &Project, index: i32) -> bool;
        #[qinvokable]
        fn track_soloed(self: &Project, index: i32) -> bool;
        /// Armed for recording.
        #[qinvokable]
        fn track_armed(self: &Project, index: i32) -> bool;
        #[qinvokable]
        fn set_track_armed(self: Pin<&mut Project>, index: i32, armed: bool);
        /// The first hardware input an audio track records, from 0.
        #[qinvokable]
        fn track_input(self: &Project, index: i32) -> i32;
        /// Whether an audio track records a stereo pair from its input on.
        #[qinvokable]
        fn track_input_stereo(self: &Project, index: i32) -> bool;
        #[qinvokable]
        fn set_track_input(self: Pin<&mut Project>, index: i32, first: i32, stereo: bool);
        /// Whether an audio track monitors its input while armed.
        #[qinvokable]
        fn track_monitor_auto(self: &Project, index: i32) -> bool;
        #[qinvokable]
        fn set_track_monitor_auto(self: Pin<&mut Project>, index: i32, on: bool);
        /// Hardware inputs the running audio engine has.
        #[qinvokable]
        fn input_count(self: &Project) -> i32;
        #[qinvokable]
        fn set_track_muted(self: Pin<&mut Project>, index: i32, muted: bool);
        #[qinvokable]
        fn set_track_soloed(self: Pin<&mut Project>, index: i32, soloed: bool);
        #[qinvokable]
        fn rename_track(self: Pin<&mut Project>, index: i32, name: &QString);
        /// The name of the track's instrument; empty when it has none.
        #[qinvokable]
        fn track_instrument(self: &Project, index: i32) -> QString;
        /// The row in `Instruments` of the track's instrument, or -1.
        #[qinvokable]
        fn track_instrument_choice(self: &Project, index: i32) -> i32;
        /// Swaps the track's instrument for row `choice` of `Instruments`
        /// at its `revision`.
        #[qinvokable]
        fn set_track_instrument(self: Pin<&mut Project>, index: i32, choice: i32, revision: i32);
        #[qinvokable]
        fn select_track(self: Pin<&mut Project>, index: i32);
        /// Moves the track at `from` to index `to`, as one undo step.
        #[qinvokable]
        fn move_track(self: Pin<&mut Project>, from: i32, to: i32);
        /// Imports an audio file at the playhead: the first (`index` 0) on
        /// the selected audio track, the rest on new tracks. Returns false
        /// if it could not; `status` says why.
        #[qinvokable]
        fn import_audio(self: Pin<&mut Project>, url: &QUrl, index: i32) -> bool;

        #[qinvokable]
        fn toggle_play(self: Pin<&mut Project>);
        #[qinvokable]
        fn stop(self: Pin<&mut Project>);
        /// Starts recording every armed track (with none armed, MIDI on the
        /// open clip's track, or the selected or first instrument track),
        /// or stops and keeps the takes.
        #[qinvokable]
        fn toggle_record(self: Pin<&mut Project>);
        #[qinvokable]
        fn go_to_start(self: Pin<&mut Project>);
        /// Turns looping on (over the selection, or the four bars from the
        /// playhead) or off.
        #[qinvokable]
        fn toggle_loop(self: Pin<&mut Project>);

        #[qinvokable]
        fn delete_selection(self: Pin<&mut Project>);
        #[qinvokable]
        fn duplicate_selection(self: Pin<&mut Project>);
        #[qinvokable]
        fn split_at_playhead(self: Pin<&mut Project>);
        #[qinvokable]
        fn select_all(self: Pin<&mut Project>);
        #[qinvokable]
        fn nudge(self: Pin<&mut Project>, direction: i32);
        #[qinvokable]
        fn zoom(self: Pin<&mut Project>, factor: f64);
        #[qinvokable]
        fn zoom_to_fit(self: Pin<&mut Project>);
        /// Sets both fades of the selected audio clips to a shape:
        /// 0 linear, 1 equal power, 2 S-curve.
        #[qinvokable]
        fn set_fade_shape(self: Pin<&mut Project>, shape: i32);
        #[qinvokable]
        fn remove_fades(self: Pin<&mut Project>);
        #[qinvokable]
        fn reset_clip_gain(self: Pin<&mut Project>);
        #[qinvokable]
        fn clear_status(self: Pin<&mut Project>);
    }

    impl cxx_qt::Initialize for Project {}
}

#[derive(Default)]
pub struct ProjectRust {
    revision: i32,
    track_count: i32,
    name: QString,
    modified: bool,
    saved_before: bool,
    can_undo: bool,
    can_redo: bool,
    undo_text: QString,
    redo_text: QString,
    playing: bool,
    recording: bool,
    looping: bool,
    position_text: QString,
    time_text: QString,
    has_selection: bool,
    audio_selected: bool,
    selected_track: i32,
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
    seen_position: i64,
}

fn kind_index(kind: TrackKind) -> i32 {
    match kind {
        TrackKind::Audio => 0,
        TrackKind::Instrument => 1,
        TrackKind::Bus => 2,
        TrackKind::Master => 3,
    }
}

/// The `kind` argument of `add_track`.
fn track_kind(kind: i32) -> TrackKind {
    match kind {
        1 => TrackKind::Instrument,
        2 => TrackKind::Bus,
        _ => TrackKind::Audio,
    }
}

/// The name the window title shows: the project's own, else the bundle's.
/// Empty for a new project that was never saved; QML words that one.
fn shown_name(p: &daw_model::Project, bundle: Option<&daw_model::Bundle>) -> String {
    if !p.name.is_empty() {
        return p.name.clone();
    }
    bundle.map(|b| b.name().to_owned()).unwrap_or_default()
}

fn track_at(s: &AppState, index: i32) -> Option<&std::sync::Arc<Track>> {
    usize::try_from(index)
        .ok()
        .and_then(|i| s.doc.project().tracks.get(i))
}

/// A message as QML sees it: `context` and `reason` (the qenums above),
/// `fileName` (of `Fields::file`), `path`, `name`, `detail`, `number` and
/// `number2`.
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
        Reason::EditRefused => QR::EditRefused,
        Reason::NoteOutsideClip => QR::NoteOutsideClip,
        Reason::EngineRefused => QR::EngineRefused,
        Reason::DeviceStateRejected => QR::DeviceStateRejected,
        Reason::UnknownDevice => QR::UnknownDevice,
        Reason::PluginNotLoaded => QR::PluginNotLoaded,
        Reason::PluginMissing => QR::PluginMissing,
        Reason::PluginLoadFailed => QR::PluginLoadFailed,
        Reason::PluginCrashed => QR::PluginCrashed,
        Reason::PluginStateNotSaved => QR::PluginStateNotSaved,
        Reason::PluginEditorFailed => QR::PluginEditorFailed,
        Reason::CannotStream => QR::CannotStream,
        Reason::CannotRead => QR::CannotRead,
        Reason::NotesDropped => QR::NotesDropped,
        Reason::ClipsDropped => QR::ClipsDropped,
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
    /// Copies state into the properties: everything when `all` or when the
    /// view revision moved, the playhead always.
    fn refresh(mut self: Pin<&mut Self>, all: bool) {
        let seen = self.seen;
        let seen_position = self.seen_position;
        let snapshot = state::with(|s| {
            s.sync();
            let playhead = s.playhead();
            let changed = all || s.view_revision != seen || playhead.0 != seen_position;
            if !changed {
                return None;
            }
            let p = s.doc.project();
            let h = s.doc.history();
            let meter = s.arrange.meter(p);
            let bb = meter.bar_beat(playhead);
            let secs = s.engine.session().map_or(0.0, |x| {
                x.ticks_to_samples(playhead).0 as f64 / x.sample_rate()
            });
            let selected = s
                .arrange
                .selected_track
                .and_then(|id| p.track_index(id))
                .map_or(-1, |i| i as i32);
            Some((
                s.view_revision,
                playhead.0,
                p.tracks.len() as i32,
                shown_name(p, s.doc.bundle()),
                s.doc.is_modified() || s.has_pending_plugin_changes(),
                s.doc.bundle().is_some(),
                h.can_undo(),
                h.can_redo(),
                h.undo_label().unwrap_or_default(),
                h.redo_label().unwrap_or_default(),
                s.engine.is_playing(),
                s.loop_range().is_some(),
                format!("{}.{}", bb.bar + 1, bb.beat + 1),
                format!("{}:{:04.1}", (secs / 60.0).floor() as i64, secs % 60.0),
                !s.arrange.selection.is_empty(),
                s.arrange.audio_selected(p),
                selected,
                s.status.clone(),
            ))
        });
        let Some((
            rev,
            pos,
            tracks,
            name,
            modified,
            saved,
            can_undo,
            can_redo,
            undo_text,
            redo_text,
            playing,
            looping,
            position,
            time,
            has_selection,
            audio_selected,
            selected,
            status,
        )) = snapshot
        else {
            return;
        };
        {
            let mut r = self.as_mut().rust_mut();
            r.seen = rev;
            r.seen_position = pos;
        }
        let mut this = self.as_mut();
        this.as_mut().set_track_count(tracks);
        this.as_mut().set_name(QString::from(name.as_str()));
        this.as_mut().set_modified(modified);
        this.as_mut().set_saved_before(saved);
        this.as_mut().set_can_undo(can_undo);
        this.as_mut().set_can_redo(can_redo);
        this.as_mut()
            .set_undo_text(QString::from(undo_text.as_str()));
        this.as_mut()
            .set_redo_text(QString::from(redo_text.as_str()));
        this.as_mut().set_playing(playing);
        this.as_mut()
            .set_recording(state::with(|s| s.is_recording()));
        this.as_mut().set_looping(looping);
        this.as_mut()
            .set_position_text(QString::from(position.as_str()));
        this.as_mut().set_time_text(QString::from(time.as_str()));
        this.as_mut().set_has_selection(has_selection);
        this.as_mut().set_audio_selected(audio_selected);
        this.as_mut().set_selected_track(selected);
        this.as_mut().set_status(message_or_empty(status.as_ref()));
        this.as_mut().refresh_files();
        // Last, so QML that reacts to it sees the other values already.
        this.set_revision((rev % i32::MAX as u64) as i32);
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
        let mut list = QList::<QString>::default();
        for p in &recent {
            list.append(QString::from(p.as_str()));
        }
        let list = QStringList::from(&list);
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
            let mut names = QList::<QString>::default();
            for f in &self.missing {
                names.append(QString::from(f.source.display().to_string().as_str()));
            }
            self.as_mut().set_missing_files(QStringList::from(&names));
            self.as_mut().set_missing_count(count);
            self.as_mut()
                .set_external_count(i32::try_from(external).unwrap_or(i32::MAX));
        }
    }

    /// Runs `f` on the state, then refreshes every property.
    fn edit(self: Pin<&mut Self>, f: impl FnOnce(&mut AppState)) {
        state::with(|s| {
            // A button may still be held on the timeline.
            s.end_drag();
            // A change made in a plugin's editor comes before this edit.
            s.flush_plugin_changes();
            f(s);
            s.sync();
            s.touch();
        });
        self.refresh(true);
    }

    fn tick(self: Pin<&mut Self>) {
        state::with(|s| {
            s.tick_engine();
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
        url.to_local_file()
            .map(|p| std::path::PathBuf::from(p.to_string()))
            .ok_or_else(|| Message::NotLocalFile(context).fields())
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
                Box::new(crate::settings::KConfigStore),
                crate::recovery::default_base().as_deref(),
            );
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

    /// Takes candidate `index` off the list.
    fn take_candidate(self: Pin<&mut Self>, index: i32) -> Option<Candidate> {
        let i = usize::try_from(index).ok()?;
        let mut c = None;
        self.edit(|s| c = s.take_recovery(i));
        c
    }

    fn recover_session(mut self: Pin<&mut Self>, index: i32) -> QMap<QMapPair_QString_QVariant> {
        let Some(c) = self.as_mut().take_candidate(index) else {
            return QMap::default();
        };
        match &c.bundle {
            Some(root) => self.open_with(root, AppState::recover),
            None => {
                let mut result = None;
                self.as_mut().edit(|s| {
                    if let Err(e) = s.recover_untitled(&c) {
                        result = Some(e.fields());
                    }
                });
                message_or_empty(result.as_ref())
            }
        }
    }

    fn discard_session(mut self: Pin<&mut Self>, index: i32) {
        let Some(c) = self.as_mut().take_candidate(index) else {
            return;
        };
        // Another Kadence may have recovered it since it was offered.
        if let Some(root) = c.bundle.as_ref().filter(|r| crate::recovery::is_locked(r)) {
            let root = root.clone();
            self.edit(|s| s.report(Message::AlreadyOpen(root)));
            return;
        }
        if let Err(e) = c.discard() {
            let path = c.autosave.clone();
            self.edit(|s| {
                s.report(Message::File(
                    Context::Recover,
                    daw_model::FormatError::Io { path, source: e },
                ));
            });
        }
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
            s.store_plugin_states();
            if let Err(e) = s.doc.save() {
                result = Some(Message::File(Context::Save, e).fields());
            }
        });
        message_or_empty(result.as_ref())
    }

    fn save_as(mut self: Pin<&mut Self>, url: &QUrl) -> QMap<QMapPair_QString_QVariant> {
        let Some(path) = url.to_local_file() else {
            return message_map(&Message::NotLocalFile(Context::Save).fields());
        };
        let mut dir = std::path::PathBuf::from(path.to_string());
        // A file dialog may hand back "Name.kadence"; the bundle is the
        // directory "Name" around it.
        if dir.extension().is_some_and(|e| e == "kadence") {
            dir.set_extension("");
        }
        let mut result = None;
        self.as_mut().edit(|s| {
            s.store_plugin_states();
            if let Err(e) = s.save_as(&dir) {
                result = Some(Message::File(Context::Save, e).fields());
            }
            s.resync();
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

    fn next_track_number(&self, kind: i32) -> i32 {
        let kind = track_kind(kind);
        state::with(|s| {
            let n = s
                .doc
                .project()
                .tracks
                .iter()
                .filter(|t| t.kind == kind)
                .count();
            i32::try_from(n + 1).unwrap_or(i32::MAX)
        })
    }

    fn add_track(self: Pin<&mut Self>, kind: i32, name: &QString) {
        let name = name.to_string();
        self.edit(|s| {
            let kind = track_kind(kind);
            let p = s.doc.project();
            // After the selected track, or at the end.
            let at = s
                .arrange
                .selected_track
                .and_then(|id| p.track_index(id))
                .map_or(p.tracks.len(), |i| i + 1);
            let track = Track::new(kind, name);
            let id = track.id;
            let command = instrument::add_track(at, track, s.instruments.default_choice());
            match s.doc.execute_boxed(command) {
                Ok(_) => s.arrange.selected_track = Some(id),
                Err(e) => s.report(e.into()),
            }
            s.doc.seal();
        });
    }

    fn remove_track(self: Pin<&mut Self>, index: i32) {
        self.edit(|s| {
            let Some(id) = track_at(s, index).map(|t| t.id) else {
                return;
            };
            if let Err(e) = s.doc.execute(RemoveTrack::new(id)) {
                s.report(e.into());
            }
            s.doc.seal();
            if s.arrange.selected_track == Some(id) {
                s.arrange.selected_track = None;
            }
        });
    }

    fn track_name(&self, index: i32) -> QString {
        state::with(|s| track_at(s, index).map(|t| QString::from(t.name.as_str())))
            .unwrap_or_default()
    }

    fn track_kind(&self, index: i32) -> i32 {
        state::with(|s| track_at(s, index).map_or(-1, |t| kind_index(t.kind)))
    }

    fn track_color(&self, index: i32) -> QColor {
        state::with(|s| {
            let Some(t) = track_at(s, index) else {
                return QColor::default();
            };
            let c = daw_arrange::track_color(t, index as usize);
            QColor::from_rgba(i32::from(c.r()), i32::from(c.g()), i32::from(c.b()), 255)
        })
    }

    fn track_muted(&self, index: i32) -> bool {
        state::with(|s| track_at(s, index).is_some_and(|t| t.mute))
    }

    fn track_soloed(&self, index: i32) -> bool {
        state::with(|s| track_at(s, index).is_some_and(|t| t.solo))
    }

    fn set_track_field(self: Pin<&mut Self>, index: i32, field: TrackField) {
        self.edit(|s| {
            let Some(id) = track_at(s, index).map(|t| t.id) else {
                return;
            };
            if let Err(e) = s.doc.execute(SetTrack::new(id, field)) {
                s.report(e.into());
            }
            s.doc.seal();
        });
    }

    fn track_armed(&self, index: i32) -> bool {
        state::with(|s| track_at(s, index).is_some_and(|t| t.armed))
    }

    fn set_track_armed(self: Pin<&mut Self>, index: i32, armed: bool) {
        self.set_track_field(index, TrackField::Armed(armed));
    }

    fn track_input(&self, index: i32) -> i32 {
        state::with(|s| track_at(s, index).map_or(0, |t| i32::from(t.audio_input.channels.first())))
    }

    fn track_input_stereo(&self, index: i32) -> bool {
        state::with(|s| {
            track_at(s, index)
                .is_some_and(|t| matches!(t.audio_input.channels, InputChannels::Stereo(_)))
        })
    }

    fn set_track_input(self: Pin<&mut Self>, index: i32, first: i32, stereo: bool) {
        let Ok(first) = u16::try_from(first) else {
            return;
        };
        let Some(mut input) = state::with(|s| track_at(s, index).map(|t| t.audio_input)) else {
            return;
        };
        let channels = if stereo {
            InputChannels::Stereo(first)
        } else {
            InputChannels::Mono(first)
        };
        if input.channels == channels {
            return;
        }
        input.channels = channels;
        self.set_track_field(index, TrackField::AudioInput(input));
    }

    fn track_monitor_auto(&self, index: i32) -> bool {
        state::with(|s| {
            track_at(s, index).is_some_and(|t| t.audio_input.monitor == Monitoring::Auto)
        })
    }

    fn set_track_monitor_auto(self: Pin<&mut Self>, index: i32, on: bool) {
        let Some(mut input) = state::with(|s| track_at(s, index).map(|t| t.audio_input)) else {
            return;
        };
        input.monitor = if on {
            Monitoring::Auto
        } else {
            Monitoring::Off
        };
        self.set_track_field(index, TrackField::AudioInput(input));
    }

    fn input_count(&self) -> i32 {
        state::with(|s| {
            s.engine
                .session()
                .map_or(0, |x| i32::from(x.config().inputs))
        })
    }

    fn set_track_muted(self: Pin<&mut Self>, index: i32, muted: bool) {
        self.set_track_field(index, TrackField::Mute(muted));
    }

    fn set_track_soloed(self: Pin<&mut Self>, index: i32, soloed: bool) {
        self.set_track_field(index, TrackField::Solo(soloed));
    }

    fn track_instrument(&self, index: i32) -> QString {
        state::with(|s| {
            track_at(s, index)
                .and_then(|t| instrument::instrument_of(t))
                .map(|d| QString::from(d.name.as_str()))
        })
        .unwrap_or_default()
    }

    fn track_instrument_choice(&self, index: i32) -> i32 {
        state::with(|s| {
            track_at(s, index)
                .and_then(|t| instrument::instrument_of(t))
                .and_then(|d| s.instruments.position(&d.source))
                .and_then(|i| i32::try_from(i).ok())
                .unwrap_or(-1)
        })
    }

    fn set_track_instrument(self: Pin<&mut Self>, index: i32, choice: i32, revision: i32) {
        // Read before `edit`, which holds the state.
        let Some(choice) = crate::instruments_bridge::choice_at(choice, revision) else {
            return;
        };
        self.edit(|s| {
            let choice = &choice;
            let Some(track) = track_at(s, index) else {
                return;
            };
            let Some(command) = instrument::set_instrument(s.doc.project(), track.id, choice)
            else {
                return;
            };
            if let Err(e) = s.doc.execute_boxed(command) {
                s.report(e.into());
            }
            s.doc.seal();
        });
    }

    fn rename_track(self: Pin<&mut Self>, index: i32, name: &QString) {
        let name = name.to_string();
        if name.trim().is_empty() {
            return;
        }
        self.set_track_field(index, TrackField::Name(name));
    }

    fn select_track(self: Pin<&mut Self>, index: i32) {
        self.edit(|s| {
            s.arrange.selected_track = track_at(s, index).map(|t| t.id);
        });
    }

    fn toggle_play(self: Pin<&mut Self>) {
        self.edit(|s| {
            if s.is_recording() {
                s.stop_recording();
            } else if s.engine.is_playing() {
                s.engine.stop();
            } else {
                s.engine.play();
            }
        });
    }

    fn stop(self: Pin<&mut Self>) {
        self.edit(|s| {
            s.stop_recording();
            s.engine.stop();
        });
    }

    fn toggle_record(self: Pin<&mut Self>) {
        self.edit(|s| {
            if s.is_recording() {
                s.stop_recording();
            } else if let Err(e) = s.start_recording() {
                s.report(e);
            }
        });
    }

    fn go_to_start(self: Pin<&mut Self>) {
        self.edit(|s| {
            if let Some(session) = s.engine.session_mut() {
                let _ = session.locate(TickTime(0));
            }
            s.arrange.view.scroll_x = 0.0;
        });
    }

    fn toggle_loop(self: Pin<&mut Self>) {
        self.edit(|s| {
            let playhead = s.playhead();
            let p = s.doc.project();
            let range = if s.loop_range().is_some() {
                None
            } else {
                let sel = p
                    .tracks
                    .iter()
                    .flat_map(|t| t.clips.iter())
                    .filter(|c| s.arrange.selection.contains(&c.id));
                let (lo, hi) = sel.fold((i64::MAX, i64::MIN), |(lo, hi), c| {
                    (lo.min(c.start.0), hi.max(c.start.0 + c.length.0))
                });
                if lo < hi {
                    Some((TickTime(lo), TickTime(hi)))
                } else {
                    let bar = s.arrange.meter(p).bar_at(playhead);
                    Some((bar.start, TickTime(bar.start.0 + 4 * bar.len.max(PPQ))))
                }
            };
            if let Some(session) = s.engine.session_mut() {
                let _ = session.set_loop(range);
            }
        });
    }

    fn arrange_command(
        self: Pin<&mut Self>,
        f: impl FnOnce(
            &mut daw_arrange::Arrange,
            &mut daw_model::Document,
            &daw_arrange::Env,
        ) -> daw_arrange::Outcome,
    ) {
        self.edit(|s| {
            let out = s.with_arrange(f);
            s.apply(out);
        });
    }

    fn delete_selection(self: Pin<&mut Self>) {
        self.arrange_command(|a, doc, _| a.delete_selection(doc));
    }

    fn duplicate_selection(self: Pin<&mut Self>) {
        self.arrange_command(|a, doc, _| a.duplicate_selection(doc));
    }

    fn split_at_playhead(self: Pin<&mut Self>) {
        self.arrange_command(|a, doc, env| a.split_at(doc, env, env.playhead));
    }

    fn select_all(self: Pin<&mut Self>) {
        self.arrange_command(|a, doc, _| a.select_all(doc.project()));
    }

    fn nudge(self: Pin<&mut Self>, direction: i32) {
        self.arrange_command(|a, doc, _| a.nudge(doc, i64::from(direction.signum())));
    }

    fn zoom(self: Pin<&mut Self>, factor: f64) {
        self.edit(|s| {
            let w = s.arrange.width;
            let x = s.arrange.view.tick_to_x(s.playhead());
            let anchor = if (0.0..=w).contains(&x) { x } else { w / 2.0 };
            s.arrange.view.zoom_at(factor, anchor);
        });
    }

    fn zoom_to_fit(self: Pin<&mut Self>) {
        self.edit(|s| {
            let p = s.doc.project().clone();
            s.arrange.zoom_to_fit(&p);
        });
    }

    fn move_track(self: Pin<&mut Self>, from: i32, to: i32) {
        self.edit(|s| {
            let Some(id) = track_at(s, from).map(|t| t.id) else {
                return;
            };
            let count = s.doc.project().tracks.len();
            let Ok(to) = usize::try_from(to) else {
                return;
            };
            if to >= count || Some(to) == s.doc.project().track_index(id) {
                return;
            }
            if let Err(e) = s.doc.execute(MoveTrack::new(id, to)) {
                s.report(e.into());
            }
            s.doc.seal();
        });
    }

    fn import_audio(self: Pin<&mut Self>, url: &QUrl, index: i32) -> bool {
        let mut ok = false;
        self.edit(|s| {
            let Some(path) = url.to_local_file() else {
                s.report(Message::NotLocalFile(Context::Import));
                return;
            };
            let path = std::path::PathBuf::from(path.to_string());
            let track = if index == 0 {
                s.arrange.selected_track
            } else {
                None
            };
            let at = s.playhead();
            match s.import_audio(&path, track, at) {
                Ok(()) => ok = true,
                Err(m) => s.report(m),
            }
        });
        ok
    }

    fn set_fade_shape(self: Pin<&mut Self>, shape: i32) {
        let shape = match shape {
            1 => FadeShape::EqualPower,
            2 => FadeShape::SCurve,
            _ => FadeShape::Linear,
        };
        self.arrange_command(move |a, doc, _| a.set_fade_shape(doc, shape));
    }

    fn remove_fades(self: Pin<&mut Self>) {
        self.arrange_command(|a, doc, _| a.remove_fades(doc));
    }

    fn reset_clip_gain(self: Pin<&mut Self>) {
        self.arrange_command(|a, doc, _| a.reset_clip_gain(doc));
    }

    fn clear_status(self: Pin<&mut Self>) {
        self.edit(|s| s.status = None);
    }
}
