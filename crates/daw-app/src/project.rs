//! `Project`: the QML-facing document. File handling, undo, tracks and the
//! transport, over [`crate::state`].

use std::pin::Pin;

use cxx_qt::CxxQtType;
use cxx_qt_lib::{QColor, QMap, QMapPair_QString_QVariant, QString, QUrl, QVariant};
use daw_core::time::{PPQ, TickTime};
use daw_model::commands::{AddTrack, RemoveTrack, SetTrack, TrackField};
use daw_model::{Track, TrackKind};

use crate::message::{Context, Fields, Message, Reason};
use crate::settings::{self, ClickSettings, KConfigStore};
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
        CannotStream,
        CannotRead,
        NotesDropped,
        ClipsDropped,
        NoInstrumentTrack,
        EngineNotRunning,
        NoMidiPlayer,
        MidiInputDropped,
        NothingRecorded,
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
        /// A MIDI take is being recorded.
        #[qproperty(bool, recording)]
        #[qproperty(bool, looping)]
        /// Bar.beat of the playhead, like "12.3".
        #[qproperty(QString, position_text)]
        /// Minutes:seconds of the playhead.
        #[qproperty(QString, time_text)]
        /// The tempo in effect at the playhead, in beats per minute.
        #[qproperty(f64, bpm)]
        /// The time signature in effect at the playhead.
        #[qproperty(i32, meter_numerator)]
        #[qproperty(i32, meter_denominator)]
        /// The click sounds while playing.
        #[qproperty(bool, metronome)]
        /// The click's level in dB.
        #[qproperty(f64, metronome_volume)]
        /// Bars counted in before recording, 0 for none.
        #[qproperty(i32, count_in_bars)]
        /// A count-in is under way.
        #[qproperty(bool, counting_in)]
        #[qproperty(bool, has_selection)]
        #[qproperty(i32, selected_track)]
        /// The last error or warning, as a message map (see
        /// `message_map`); empty when there is none. QML words it.
        #[qproperty(QMap_QString_QVariant, status)]
        type Project = super::ProjectRust;

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
        #[qinvokable]
        fn set_track_muted(self: Pin<&mut Project>, index: i32, muted: bool);
        #[qinvokable]
        fn set_track_soloed(self: Pin<&mut Project>, index: i32, soloed: bool);
        #[qinvokable]
        fn rename_track(self: Pin<&mut Project>, index: i32, name: &QString);
        #[qinvokable]
        fn select_track(self: Pin<&mut Project>, index: i32);

        #[qinvokable]
        fn toggle_play(self: Pin<&mut Project>);
        #[qinvokable]
        fn stop(self: Pin<&mut Project>);
        /// Starts recording MIDI on the open clip's track (or the selected
        /// or first instrument track), or stops and keeps the take.
        #[qinvokable]
        fn toggle_record(self: Pin<&mut Project>);
        #[qinvokable]
        fn go_to_start(self: Pin<&mut Project>);
        /// Sets the tempo at the playhead. Calls in a row are one undo step.
        #[qinvokable]
        fn set_tempo(self: Pin<&mut Project>, bpm: f64);
        /// Sets the time signature at the playhead; the denominator is
        /// rounded down to a power of two.
        #[qinvokable]
        fn set_time_signature(self: Pin<&mut Project>, numerator: i32, denominator: i32);
        #[qinvokable]
        fn set_metronome_enabled(self: Pin<&mut Project>, enabled: bool);
        #[qinvokable]
        fn set_metronome_level(self: Pin<&mut Project>, volume_db: f64);
        #[qinvokable]
        fn set_count_in(self: Pin<&mut Project>, bars: i32);
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
    bpm: f64,
    meter_numerator: i32,
    meter_denominator: i32,
    metronome: bool,
    metronome_volume: f64,
    count_in_bars: i32,
    counting_in: bool,
    has_selection: bool,
    selected_track: i32,
    status: QMap<QMapPair_QString_QVariant>,
    /// Last view revision copied into the properties.
    seen: u64,
    seen_position: i64,
    /// Whether a count-in was under way at the last refresh; it starts and
    /// ends with the playhead standing still.
    seen_counting_in: bool,
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
        Reason::CannotStream => QR::CannotStream,
        Reason::CannotRead => QR::CannotRead,
        Reason::NotesDropped => QR::NotesDropped,
        Reason::ClipsDropped => QR::ClipsDropped,
        Reason::NoInstrumentTrack => QR::NoInstrumentTrack,
        Reason::EngineNotRunning => QR::EngineNotRunning,
        Reason::NoMidiPlayer => QR::NoMidiPlayer,
        Reason::MidiInputDropped => QR::MidiInputDropped,
        Reason::NothingRecorded => QR::NothingRecorded,
    };
    let text = |s: &str| QVariant::from(&QString::from(s));
    // Numbers are schema versions; they fit an i32.
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
        let click = settings::load_click_settings(&KConfigStore);
        state::with(|s| s.set_click(click));
        self.refresh(true);
    }
}

impl qobject::Project {
    /// Copies state into the properties: everything when `all` or when the
    /// view revision moved, the playhead always.
    fn refresh(mut self: Pin<&mut Self>, all: bool) {
        let seen = self.seen;
        let seen_position = self.seen_position;
        let seen_counting_in = self.seen_counting_in;
        let snapshot = state::with(|s| {
            s.sync();
            let playhead = s.playhead();
            let counting_in = s.is_counting_in();
            let changed = all
                || s.view_revision != seen
                || playhead.0 != seen_position
                || counting_in != seen_counting_in;
            if !changed {
                return None;
            }
            let p = s.doc.project();
            let h = s.doc.history();
            // Bars and beats as the engine counts them (what the metronome
            // clicks), else from the document.
            let (bar, beat) = match s.engine.session() {
                Some(x) => {
                    let m = x.music();
                    (m.bar, m.bar_beat().floor() as i64)
                }
                None => {
                    let bb = s.arrange.meter(p).bar_beat(playhead);
                    (bb.bar, bb.beat)
                }
            };
            let tempo = s.tempo_at_playhead();
            let click = s.click();
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
                s.doc.is_modified(),
                s.doc.bundle().is_some(),
                h.can_undo(),
                h.can_redo(),
                h.undo_label().unwrap_or_default(),
                h.redo_label().unwrap_or_default(),
                s.engine.is_playing(),
                s.loop_range().is_some(),
                format!("{}.{}", bar + 1, beat + 1),
                format!("{}:{:04.1}", (secs / 60.0).floor() as i64, secs % 60.0),
                !s.arrange.selection.is_empty(),
                selected,
                s.status.clone(),
                tempo,
                click,
                counting_in,
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
            selected,
            status,
            (bpm, numerator, denominator),
            click,
            counting_in,
        )) = snapshot
        else {
            return;
        };
        {
            let mut r = self.as_mut().rust_mut();
            r.seen = rev;
            r.seen_position = pos;
            r.seen_counting_in = counting_in;
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
        this.as_mut().set_bpm(bpm);
        this.as_mut().set_meter_numerator(i32::from(numerator));
        this.as_mut().set_meter_denominator(i32::from(denominator));
        this.as_mut().set_metronome(click.metronome.enabled);
        this.as_mut()
            .set_metronome_volume(click.metronome.volume_db);
        this.as_mut()
            .set_count_in_bars(i32::try_from(click.count_in_bars).unwrap_or(0));
        this.as_mut().set_counting_in(counting_in);
        this.as_mut().set_has_selection(has_selection);
        this.as_mut().set_selected_track(selected);
        this.as_mut().set_status(message_or_empty(status.as_ref()));
        // Last, so QML that reacts to it sees the other values already.
        this.set_revision((rev % i32::MAX as u64) as i32);
    }

    /// Runs `f` on the state, then refreshes every property.
    fn edit(self: Pin<&mut Self>, f: impl FnOnce(&mut AppState)) {
        state::with(|s| {
            // A button may still be held on the timeline.
            s.end_drag();
            f(s);
            s.sync();
            s.touch();
        });
        self.refresh(true);
    }

    fn tick(self: Pin<&mut Self>) {
        state::with(|s| {
            if let Some(session) = s.engine.session_mut() {
                session.collect_garbage();
            }
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
        let mut result = None;
        self.edit(|s| {
            if let Err(e) = s.open(&path) {
                result = Some(Message::File(Context::Open, e).fields());
            }
        });
        message_or_empty(result.as_ref())
    }

    fn save(self: Pin<&mut Self>) -> QMap<QMapPair_QString_QVariant> {
        let mut result = None;
        self.edit(|s| {
            if let Err(e) = s.doc.save() {
                result = Some(Message::File(Context::Save, e).fields());
            }
        });
        message_or_empty(result.as_ref())
    }

    fn save_as(self: Pin<&mut Self>, url: &QUrl) -> QMap<QMapPair_QString_QVariant> {
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
        self.edit(|s| {
            if let Err(e) = s.doc.save_as(&dir) {
                result = Some(Message::File(Context::Save, e).fields());
            }
            s.resync();
        });
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
            match s.doc.execute(AddTrack::new(at, track)) {
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

    fn set_track_muted(self: Pin<&mut Self>, index: i32, muted: bool) {
        self.set_track_field(index, TrackField::Mute(muted));
    }

    fn set_track_soloed(self: Pin<&mut Self>, index: i32, soloed: bool) {
        self.set_track_field(index, TrackField::Solo(soloed));
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

    fn set_tempo(self: Pin<&mut Self>, bpm: f64) {
        self.edit(|s| {
            if let Err(e) = s.set_bpm(bpm) {
                s.report(e.into());
            }
        });
    }

    fn set_time_signature(self: Pin<&mut Self>, numerator: i32, denominator: i32) {
        let to_u8 = |n: i32| u8::try_from(n.clamp(0, 255)).unwrap_or(0);
        self.edit(|s| {
            if let Err(e) = s.set_meter(to_u8(numerator), to_u8(denominator)) {
                s.report(e.into());
            }
        });
    }

    /// Changes the click settings with `f`, applies and saves them.
    fn change_click(self: Pin<&mut Self>, f: impl FnOnce(&mut ClickSettings)) {
        self.edit(|s| {
            let mut click = s.click();
            f(&mut click);
            s.set_click(click);
            settings::save_click_settings(&mut KConfigStore, &s.click());
        });
    }

    fn set_metronome_enabled(self: Pin<&mut Self>, enabled: bool) {
        self.change_click(|c| c.metronome.enabled = enabled);
    }

    fn set_metronome_level(self: Pin<&mut Self>, volume_db: f64) {
        self.change_click(|c| c.metronome.volume_db = volume_db);
    }

    fn set_count_in(self: Pin<&mut Self>, bars: i32) {
        self.change_click(|c| c.count_in_bars = u32::try_from(bars).unwrap_or(0));
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

    fn clear_status(self: Pin<&mut Self>) {
        self.edit(|s| s.status = None);
    }
}
