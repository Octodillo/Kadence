//! `Project`: the QML-facing document. File handling, undo, tracks and the
//! transport, over [`crate::state`].

use std::pin::Pin;

use cxx_qt::CxxQtType;
use cxx_qt_lib::{QColor, QString, QUrl};
use daw_core::time::{PPQ, TickTime};
use daw_model::commands::{AddTrack, RemoveTrack, SetTrack, TrackField};
use daw_model::{Track, TrackKind};

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
        #[qproperty(bool, has_selection)]
        #[qproperty(i32, selected_track)]
        /// The last error or warning, for an inline message.
        #[qproperty(QString, status_text)]
        type Project = super::ProjectRust;

        /// Called by a timer: picks up engine position and background work.
        #[qinvokable]
        fn tick(self: Pin<&mut Project>);

        #[qinvokable]
        fn new_project(self: Pin<&mut Project>);
        /// Returns "" or an error message.
        #[qinvokable]
        fn open_project(self: Pin<&mut Project>, url: &QUrl) -> QString;
        #[qinvokable]
        fn save(self: Pin<&mut Project>) -> QString;
        /// Saves into a new bundle directory at `url`.
        #[qinvokable]
        fn save_as(self: Pin<&mut Project>, url: &QUrl) -> QString;

        #[qinvokable]
        fn undo(self: Pin<&mut Project>);
        #[qinvokable]
        fn redo(self: Pin<&mut Project>);

        /// 0 audio, 1 instrument, 2 bus.
        #[qinvokable]
        fn add_track(self: Pin<&mut Project>, kind: i32);
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
    has_selection: bool,
    selected_track: i32,
    status_text: QString,
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

fn track_at(s: &AppState, index: i32) -> Option<&std::sync::Arc<Track>> {
    usize::try_from(index)
        .ok()
        .and_then(|i| s.doc.project().tracks.get(i))
}

fn error_text(e: impl std::fmt::Display) -> QString {
    QString::from(e.to_string().as_str())
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
                p.name.clone(),
                s.doc.is_modified(),
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
        this.as_mut().set_selected_track(selected);
        this.as_mut()
            .set_status_text(QString::from(status.as_str()));
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
            if let Err(e) = s.doc.autosave() {
                s.status = e.to_string();
            }
        });
        self.refresh(false);
    }

    fn new_project(self: Pin<&mut Self>) {
        self.edit(AppState::new_document);
    }

    fn open_project(self: Pin<&mut Self>, url: &QUrl) -> QString {
        let Some(path) = url.to_local_file() else {
            return QString::from("not a local file");
        };
        let path = std::path::PathBuf::from(path.to_string());
        let mut result = QString::default();
        self.edit(|s| {
            if let Err(e) = s.open(&path) {
                result = error_text(e);
            }
        });
        result
    }

    fn save(self: Pin<&mut Self>) -> QString {
        let mut result = QString::default();
        self.edit(|s| {
            if let Err(e) = s.doc.save() {
                result = error_text(e);
            }
        });
        result
    }

    fn save_as(self: Pin<&mut Self>, url: &QUrl) -> QString {
        let Some(path) = url.to_local_file() else {
            return QString::from("not a local file");
        };
        let mut dir = std::path::PathBuf::from(path.to_string());
        // A file dialog may hand back "Name.kadence"; the bundle is the
        // directory "Name" around it.
        if dir.extension().is_some_and(|e| e == "kadence") {
            dir.set_extension("");
        }
        let mut result = QString::default();
        self.edit(|s| {
            if let Err(e) = s.doc.save_as(&dir) {
                result = error_text(e);
            }
            s.resync();
        });
        result
    }

    fn undo(self: Pin<&mut Self>) {
        self.edit(|s| {
            if let Err(e) = s.doc.undo() {
                s.status = e.to_string();
            }
        });
    }

    fn redo(self: Pin<&mut Self>) {
        self.edit(|s| {
            if let Err(e) = s.doc.redo() {
                s.status = e.to_string();
            }
        });
    }

    fn add_track(self: Pin<&mut Self>, kind: i32) {
        self.edit(|s| {
            let (kind, base) = match kind {
                1 => (TrackKind::Instrument, "Instrument"),
                2 => (TrackKind::Bus, "Bus"),
                _ => (TrackKind::Audio, "Audio"),
            };
            let p = s.doc.project();
            let n = p.tracks.iter().filter(|t| t.kind == kind).count() + 1;
            // After the selected track, or at the end.
            let at = s
                .arrange
                .selected_track
                .and_then(|id| p.track_index(id))
                .map_or(p.tracks.len(), |i| i + 1);
            let track = Track::new(kind, format!("{base} {n}"));
            let id = track.id;
            match s.doc.execute(AddTrack::new(at, track)) {
                Ok(_) => s.arrange.selected_track = Some(id),
                Err(e) => s.status = e.to_string(),
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
                s.status = e.to_string();
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
                s.status = e.to_string();
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
                s.status = e;
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

    fn clear_status(self: Pin<&mut Self>) {
        self.edit(|s| s.status.clear());
    }
}
