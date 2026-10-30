//! `Exporter`: the QML-facing side of exporting the song to a file (see
//! [`crate::export`]). The dialog is `ExportDialog.qml`.

use std::pin::Pin;

use cxx_qt_lib::{QMap, QMapPair_QString_QVariant, QString, QUrl};

use crate::export::{FORMATS, Range, Request, extension};
use crate::message::{Context, Message};
use crate::project::message_map;
use crate::state;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qurl.h");
        type QUrl = cxx_qt_lib::QUrl;
        include!("cxx-qt-lib/qmap.h");
        type QMap_QString_QVariant = cxx_qt_lib::QMap<cxx_qt_lib::QMapPair_QString_QVariant>;
    }

    /// The `range` argument of `start`.
    #[qenum(Exporter)]
    enum ExportRange {
        WholeSong,
        LoopRange,
        Bars,
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        /// An export is running.
        #[qproperty(bool, running)]
        /// From 0 to 1 while running.
        #[qproperty(f64, progress)]
        /// Bars the song spans, as of the last `refresh`; 0 when it has no
        /// clips.
        #[qproperty(i32, song_bars)]
        /// A loop is set, as of the last `refresh`.
        #[qproperty(bool, has_loop)]
        /// The rate the engine runs at, as of the last `refresh`.
        #[qproperty(i32, engine_rate)]
        type Exporter = super::ExporterRust;

        /// Reads the song length, loop and engine rate for the dialog.
        #[qinvokable]
        fn refresh(self: Pin<&mut Exporter>);

        /// Starts exporting to `url`. `range` is an `ExportRange`;
        /// `first_bar` and `last_bar` (from 1, both included) are for
        /// `Bars`. `format` indexes the dialog's list: WAV 16-bit, 24-bit,
        /// 32-bit float, FLAC 16-bit, 24-bit. Returns an empty map, or a
        /// message map saying why it did not start.
        #[qinvokable]
        fn start(
            self: Pin<&mut Exporter>,
            url: &QUrl,
            range: i32,
            first_bar: i32,
            last_bar: i32,
            tail_seconds: f64,
            format: i32,
            sample_rate: i32,
            dither: bool,
        ) -> QMap_QString_QVariant;

        /// Stops the running export; `finished` then reports it cancelled.
        #[qinvokable]
        fn cancel(self: Pin<&mut Exporter>);

        /// Called by a timer while running: updates `progress`, and emits
        /// `finished` once the export has ended.
        #[qinvokable]
        fn poll(self: Pin<&mut Exporter>);

        /// The file extension for `format`, without the dot.
        #[qinvokable]
        fn extension(self: &Exporter, format: i32) -> QString;

        /// The export ended: `message` says where the file is, or why there
        /// is none.
        #[qsignal]
        fn finished(self: Pin<&mut Exporter>, message: QMap_QString_QVariant);
    }

    impl cxx_qt::Initialize for Exporter {}
}

#[derive(Default)]
pub struct ExporterRust {
    running: bool,
    progress: f64,
    song_bars: i32,
    has_loop: bool,
    engine_rate: i32,
}

fn format_at(index: i32) -> daw_media::FileFormat {
    usize::try_from(index)
        .ok()
        .and_then(|i| FORMATS.get(i).copied())
        .unwrap_or(FORMATS[0])
}

impl cxx_qt::Initialize for qobject::Exporter {
    fn initialize(self: Pin<&mut Self>) {
        self.refresh();
    }
}

impl qobject::Exporter {
    fn refresh(mut self: Pin<&mut Self>) {
        let (bars, has_loop, rate) = state::with(|s| {
            (
                i32::try_from(s.song_bars()).unwrap_or(i32::MAX),
                s.loop_range().is_some(),
                s.sample_rate().round() as i32,
            )
        });
        self.as_mut().set_song_bars(bars);
        self.as_mut().set_has_loop(has_loop);
        self.as_mut().set_engine_rate(rate);
    }

    fn start(
        mut self: Pin<&mut Self>,
        url: &QUrl,
        range: i32,
        first_bar: i32,
        last_bar: i32,
        tail_seconds: f64,
        format: i32,
        sample_rate: i32,
        dither: bool,
    ) -> QMap<QMapPair_QString_QVariant> {
        let Some(path) = url.to_local_file() else {
            return message_map(&Message::NotLocalFile(Context::Export).fields());
        };
        use qobject::ExportRange as R;
        let range = match range {
            r if r == R::LoopRange.repr => Range::Loop,
            r if r == R::Bars.repr => Range::Bars {
                first: i64::from(first_bar),
                last: i64::from(last_bar),
            },
            _ => Range::Song,
        };
        let request = Request {
            range,
            tail_seconds: tail_seconds.max(0.0),
            format: format_at(format),
            sample_rate: u32::try_from(sample_rate).unwrap_or(0),
            dither,
        };
        let path = std::path::PathBuf::from(path.to_string());
        let result = state::with(|s| s.start_export(path, &request));
        match result {
            Ok(()) => {
                self.as_mut().set_progress(0.0);
                self.as_mut().set_running(true);
                QMap::default()
            }
            Err(m) => message_map(&m.fields()),
        }
    }

    fn cancel(self: Pin<&mut Self>) {
        state::with(|s| s.cancel_export());
    }

    fn poll(mut self: Pin<&mut Self>) {
        let (progress, done) = state::with(|s| {
            let progress = s.export_progress();
            (progress, s.poll_export())
        });
        if let Some(p) = progress {
            self.as_mut().set_progress(p);
        }
        if let Some(message) = done {
            self.as_mut().set_running(false);
            self.as_mut().finished(message_map(&message.fields()));
        }
    }

    fn extension(&self, format: i32) -> QString {
        QString::from(extension(format_at(format)))
    }
}
