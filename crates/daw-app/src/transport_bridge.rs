//! `Transport`: the QML-facing playhead, playback and recording, tempo,
//! time signature and click, over [`crate::state`] (the Qt-free half is
//! [`crate::transport`]).
//!
//! Its properties follow the state when [`sync`](qobject::Transport::sync)
//! is called, which only the UI clock (`UiClock.qml`) does: on every beat
//! (the playhead moves) and on every revision of the project. Its own edits
//! emit `edited`, on which the clock moves the project's revision, and so
//! these properties too.

use std::pin::Pin;

use cxx_qt::CxxQtType;
use cxx_qt_lib::QString;
use daw_core::time::{PPQ, TickTime};

use crate::settings::{self, ClickSettings, KConfigStore};
use crate::state::{self, AppState};

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        #[qproperty(bool, playing)]
        /// Takes are being recorded.
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
        type Transport = super::TransportRust;

        /// Copies the state into the properties if the project or the
        /// playhead moved since the last time. Called by the UI's clock
        /// (`UiClock.qml`).
        #[qinvokable]
        fn sync(self: Pin<&mut Transport>);

        #[qinvokable]
        fn toggle_play(self: Pin<&mut Transport>);
        /// Stops playback; effects and instruments ring out.
        #[qinvokable]
        fn stop(self: Pin<&mut Transport>);
        /// Stops playback and recording and cuts off everything still
        /// sounding: effect tails, held voices, plugins.
        #[qinvokable]
        fn stop_all_sound(self: Pin<&mut Transport>);
        /// Starts recording every armed track (with none armed, MIDI on the
        /// open clip's track, or the selected or first instrument track),
        /// or stops and keeps the takes.
        #[qinvokable]
        fn toggle_record(self: Pin<&mut Transport>);
        #[qinvokable]
        fn go_to_start(self: Pin<&mut Transport>);
        /// Turns looping on (over the selection, or the four bars from the
        /// playhead) or off.
        #[qinvokable]
        fn toggle_loop(self: Pin<&mut Transport>);
        /// Sets the tempo at the playhead. Calls in a row are one undo step.
        #[qinvokable]
        fn set_tempo(self: Pin<&mut Transport>, bpm: f64);
        /// Sets the time signature at the playhead; the denominator is
        /// rounded down to a power of two.
        #[qinvokable]
        fn set_time_signature(self: Pin<&mut Transport>, numerator: i32, denominator: i32);
        #[qinvokable]
        fn set_metronome_enabled(self: Pin<&mut Transport>, enabled: bool);
        /// Saves the setting only when `save`, so a slider can save once it
        /// is let go.
        #[qinvokable]
        fn set_metronome_level(self: Pin<&mut Transport>, volume_db: f64, save: bool);
        #[qinvokable]
        fn set_count_in(self: Pin<&mut Transport>, bars: i32);

        /// One of the calls above changed the state: the project shows it
        /// now (see `UiClock.qml`).
        #[qsignal]
        fn edited(self: Pin<&mut Transport>);
    }

    impl cxx_qt::Initialize for Transport {}
}

#[derive(Default)]
pub struct TransportRust {
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
    /// What the properties were last copied at; see [`Seen`].
    seen: Option<Seen>,
}

/// When the properties need copying again: the view revision moved, the
/// playhead moved, or playback or a count-in started or ended (both can
/// happen with the playhead standing still).
type Seen = (u64, i64, bool, bool);

/// The transport as the properties show it.
struct Shown {
    playing: bool,
    recording: bool,
    looping: bool,
    position: String,
    time: String,
    tempo: (f64, u8, u8),
    click: ClickSettings,
    counting_in: bool,
}

impl Shown {
    fn of(s: &AppState) -> Self {
        let playhead = s.playhead();
        // Bars and beats as the engine counts them (what the metronome
        // clicks), else from the document.
        let (bar, beat) = match s.engine.session() {
            Some(x) => {
                let m = x.music();
                (m.bar, m.bar_beat().floor() as i64)
            }
            None => {
                let bb = s.arrange.meter(s.doc.project()).bar_beat(playhead);
                (bb.bar, bb.beat)
            }
        };
        let secs = s.engine.session().map_or(0.0, |x| {
            x.ticks_to_samples(playhead).0 as f64 / x.sample_rate()
        });
        Self {
            playing: s.engine.is_playing(),
            recording: s.is_recording(),
            looping: s.loop_range().is_some(),
            position: format!("{}.{}", bar + 1, beat + 1),
            time: format!("{}:{:04.1}", (secs / 60.0).floor() as i64, secs % 60.0),
            tempo: s.tempo_at_playhead(),
            click: s.click(),
            counting_in: s.is_counting_in(),
        }
    }
}

impl cxx_qt::Initialize for qobject::Transport {
    fn initialize(self: Pin<&mut Self>) {
        let click = settings::load_click_settings(&KConfigStore);
        state::with(|s| s.set_click(click));
        self.sync();
    }
}

impl qobject::Transport {
    fn sync(mut self: Pin<&mut Self>) {
        let seen = self.seen;
        let Some((at, shown)) = state::with(|s| {
            let at = (
                s.view_revision,
                s.playhead().0,
                s.engine.is_playing(),
                s.is_counting_in(),
            );
            (seen != Some(at)).then(|| (at, Shown::of(s)))
        }) else {
            return;
        };
        self.as_mut().rust_mut().seen = Some(at);
        let (bpm, numerator, denominator) = shown.tempo;
        let mut this = self.as_mut();
        this.as_mut().set_playing(shown.playing);
        this.as_mut().set_recording(shown.recording);
        this.as_mut().set_looping(shown.looping);
        this.as_mut()
            .set_position_text(QString::from(shown.position.as_str()));
        this.as_mut()
            .set_time_text(QString::from(shown.time.as_str()));
        this.as_mut().set_bpm(bpm);
        this.as_mut().set_meter_numerator(i32::from(numerator));
        this.as_mut().set_meter_denominator(i32::from(denominator));
        this.as_mut().set_metronome(shown.click.metronome.enabled);
        this.as_mut()
            .set_metronome_volume(shown.click.metronome.volume_db);
        this.as_mut()
            .set_count_in_bars(i32::try_from(shown.click.count_in_bars).unwrap_or(0));
        this.set_counting_in(shown.counting_in);
    }

    /// Runs `f` on the state as an edit and says so. The properties here
    /// follow with everything else's, through the project's revision (see
    /// `UiClock.qml`), so QML sees them all change together.
    fn edit(self: Pin<&mut Self>, f: impl FnOnce(&mut AppState)) {
        state::with(|s| s.edit(f));
        self.edited();
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

    fn stop_all_sound(self: Pin<&mut Self>) {
        self.edit(|s| {
            s.stop_recording();
            s.silence();
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
            let looping = s.loop_range().is_some();
            let sel = p
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .filter(|c| s.arrange.selection.contains(&c.id));
            let (lo, hi) = sel.fold((i64::MAX, i64::MIN), |(lo, hi), c| {
                (lo.min(c.start.0), hi.max(c.start.0 + c.length.0))
            });
            let bar = s.arrange.meter(p).bar_at(playhead);
            let bars = (bar.start, TickTime(bar.start.0 + 4 * bar.len.max(PPQ)));
            let Some(session) = s.engine.session_mut() else {
                return;
            };
            if looping {
                let _ = session.set_loop(None);
                return;
            }
            // The selected clips, or four bars from the playhead when
            // nothing is selected or the selection is too short to loop.
            if lo < hi {
                let _ = session.set_loop(Some((TickTime(lo), TickTime(hi))));
            }
            if session.loop_range().is_none() {
                let _ = session.set_loop(Some(bars));
            }
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

    /// Changes the click settings with `f` and applies them, saving them
    /// when `save`.
    fn change_click(self: Pin<&mut Self>, save: bool, f: impl FnOnce(&mut ClickSettings)) {
        self.edit(|s| {
            let mut click = s.click();
            f(&mut click);
            s.set_click(click);
            if save {
                settings::save_click_settings(&mut KConfigStore, &s.click());
            }
        });
    }

    fn set_metronome_enabled(self: Pin<&mut Self>, enabled: bool) {
        self.change_click(true, |c| c.metronome.enabled = enabled);
    }

    fn set_metronome_level(self: Pin<&mut Self>, volume_db: f64, save: bool) {
        self.change_click(save, |c| c.metronome.volume_db = volume_db);
    }

    fn set_count_in(self: Pin<&mut Self>, bars: i32) {
        self.change_click(true, |c| {
            c.count_in_bars = u32::try_from(bars).unwrap_or(0);
        });
    }
}
