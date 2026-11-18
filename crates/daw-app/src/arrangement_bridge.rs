//! `Arrangement`: the QML-facing edit commands of the timeline (the
//! selection, clip edits, zoom, importing audio), over the state's
//! [`Arrange`](daw_arrange::Arrange). The painted timeline itself is
//! [`ArrangeView`](crate::arrange_view).
//!
//! Its properties follow the state when [`sync`](qobject::Arrangement::sync)
//! is called, which only the UI clock (`UiClock.qml`) does, on every
//! revision of the project. Its own edits emit `edited`, on which the clock
//! moves the project's revision, and so these properties too.

use std::pin::Pin;

use cxx_qt::CxxQtType;
use cxx_qt_lib::QUrl;
use daw_arrange::{Arrange, Env, Outcome};
use daw_model::{Document, FadeShape};

use crate::message::{Context, Message};
use crate::state::{self, AppState};

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qurl.h");
        type QUrl = cxx_qt_lib::QUrl;
    }

    /// The curve of a clip's fade, for `set_fade_shape`.
    #[qenum(Arrangement)]
    enum FadeShape {
        LinearFade,
        EqualPowerFade,
        SCurveFade,
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        /// Clips or automation points are selected.
        #[qproperty(bool, has_selection)]
        /// The selection holds an audio clip (for clip gain and fades).
        #[qproperty(bool, audio_selected)]
        /// The selected track's row in `Tracks`, or -1.
        #[qproperty(i32, selected_track)]
        type Arrangement = super::ArrangementRust;

        /// Copies the state into the properties if the project changed
        /// since the last time. Called by the UI's clock (`UiClock.qml`).
        #[qinvokable]
        fn sync(self: Pin<&mut Arrangement>);

        /// Selects the track at `index` of the track list (see `Tracks`).
        #[qinvokable]
        fn select_track(self: Pin<&mut Arrangement>, index: i32);
        /// Imports an audio file at the playhead: the first (`index` 0) on
        /// the selected audio track, the rest on new tracks. Returns false
        /// if it could not; `Project.status` says why.
        #[qinvokable]
        fn import_audio(self: Pin<&mut Arrangement>, url: &QUrl, index: i32) -> bool;

        #[qinvokable]
        fn delete_selection(self: Pin<&mut Arrangement>);
        #[qinvokable]
        fn duplicate_selection(self: Pin<&mut Arrangement>);
        #[qinvokable]
        fn split_at_playhead(self: Pin<&mut Arrangement>);
        #[qinvokable]
        fn select_all(self: Pin<&mut Arrangement>);
        #[qinvokable]
        fn nudge(self: Pin<&mut Arrangement>, direction: i32);
        #[qinvokable]
        fn zoom(self: Pin<&mut Arrangement>, factor: f64);
        #[qinvokable]
        fn zoom_to_fit(self: Pin<&mut Arrangement>);
        /// Sets both fades of the selected audio clips to a shape.
        #[qinvokable]
        fn set_fade_shape(self: Pin<&mut Arrangement>, shape: FadeShape);
        #[qinvokable]
        fn remove_fades(self: Pin<&mut Arrangement>);
        #[qinvokable]
        fn reset_clip_gain(self: Pin<&mut Arrangement>);

        /// One of the calls above changed the state: the project shows it
        /// now (see `UiClock.qml`).
        #[qsignal]
        fn edited(self: Pin<&mut Arrangement>);
    }

    impl cxx_qt::Initialize for Arrangement {}
}

#[derive(Default)]
pub struct ArrangementRust {
    has_selection: bool,
    audio_selected: bool,
    selected_track: i32,
    /// The view revision the properties were last copied at.
    seen: Option<u64>,
}

impl cxx_qt::Initialize for qobject::Arrangement {
    fn initialize(self: Pin<&mut Self>) {
        self.sync();
    }
}

impl qobject::Arrangement {
    fn sync(mut self: Pin<&mut Self>) {
        let seen = self.seen;
        let Some((revision, has_selection, audio_selected, selected)) = state::with(|s| {
            (seen != Some(s.view_revision)).then(|| {
                let p = s.doc.project();
                let a = &s.arrange;
                let selected = a
                    .selected_track
                    .and_then(|id| p.track_index(id))
                    .map_or(-1, |i| i32::try_from(i).unwrap_or(i32::MAX));
                (
                    s.view_revision,
                    a.clips_selected(p) || a.lanes.points_selected(p),
                    a.audio_selected(p),
                    selected,
                )
            })
        }) else {
            return;
        };
        self.as_mut().rust_mut().seen = Some(revision);
        self.as_mut().set_has_selection(has_selection);
        self.as_mut().set_audio_selected(audio_selected);
        self.set_selected_track(selected);
    }

    /// Runs `f` on the state as an edit and says so. The properties here
    /// follow with everything else's, through the project's revision (see
    /// `UiClock.qml`), so QML sees them all change together.
    fn edit(self: Pin<&mut Self>, f: impl FnOnce(&mut AppState)) {
        state::with(|s| s.edit(f));
        self.edited();
    }

    /// An edit of the arrangement that says what it changed, as a gesture
    /// on the timeline does.
    fn command(self: Pin<&mut Self>, f: impl FnOnce(&mut Arrange, &mut Document, &Env) -> Outcome) {
        self.edit(|s| {
            let out = s.with_view(f);
            s.apply(out);
        });
    }

    fn select_track(self: Pin<&mut Self>, index: i32) {
        self.edit(|s| {
            let track = usize::try_from(index)
                .ok()
                .and_then(|i| s.doc.project().tracks.get(i))
                .map(|t| t.id);
            s.arrange.selected_track = track;
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

    fn delete_selection(self: Pin<&mut Self>) {
        self.command(|a, doc, _| a.delete_selection(doc));
    }

    fn duplicate_selection(self: Pin<&mut Self>) {
        self.command(|a, doc, _| a.duplicate_selection(doc));
    }

    fn split_at_playhead(self: Pin<&mut Self>) {
        self.command(|a, doc, env| a.split_at(doc, env, env.playhead));
    }

    fn select_all(self: Pin<&mut Self>) {
        self.command(|a, doc, _| a.select_all(doc.project()));
    }

    fn nudge(self: Pin<&mut Self>, direction: i32) {
        self.command(|a, doc, _| a.nudge(doc, i64::from(direction.signum())));
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

    fn set_fade_shape(self: Pin<&mut Self>, shape: qobject::FadeShape) {
        let shape = match shape {
            qobject::FadeShape::EqualPowerFade => FadeShape::EqualPower,
            qobject::FadeShape::SCurveFade => FadeShape::SCurve,
            _ => FadeShape::Linear,
        };
        self.command(move |a, doc, _| a.set_fade_shape(doc, shape));
    }

    fn remove_fades(self: Pin<&mut Self>) {
        self.command(|a, doc, _| a.remove_fades(doc));
    }

    fn reset_clip_gain(self: Pin<&mut Self>) {
        self.command(|a, doc, _| a.reset_clip_gain(doc));
    }
}
