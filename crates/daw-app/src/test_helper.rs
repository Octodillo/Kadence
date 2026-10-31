//! Test helpers for `--smoke-test` and `--screenshot-dir` (see SelfTest.qml).

use cxx_qt_lib::{QString, QUrl};

#[cxx_qt::bridge]
pub mod self_test {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qurl.h");
        type QUrl = cxx_qt_lib::QUrl;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        type TestHelper = super::TestHelperRust;

        /// Save the visible window titled `title` as a PNG at `path`.
        #[qinvokable]
        fn save_screenshot(self: &TestHelper, title: &QString, path: &QString) -> bool;

        /// Replace the document with a small demo song (two audio tracks with
        /// generated audio, a MIDI track) for screenshots. Returns false if
        /// the audio could not be written.
        #[qinvokable]
        fn load_demo_project(self: &TestHelper) -> bool;

        /// Replace the document with `tracks` audio tracks of `clips` clips
        /// each, for `--benchmark`.
        #[qinvokable]
        fn load_stress_project(self: &TestHelper, tracks: i32, clips: i32) -> bool;

        /// A monotonic clock in milliseconds, for frame timing.
        #[qinvokable]
        fn now_ms(self: &TestHelper) -> f64;

        /// Deletes the temporary demo and benchmark projects.
        #[qinvokable]
        fn remove_temp_files(self: &TestHelper);

        /// Open the first MIDI clip in the piano roll, as a double-click on
        /// it in the arrange view would. Returns false if there is none.
        #[qinvokable]
        fn open_first_midi_clip(self: &TestHelper) -> bool;

        /// How many notes the piano roll's clip has.
        #[qinvokable]
        fn piano_note_count(self: &TestHelper) -> i32;

        /// The sum of the keys of the piano roll's notes.
        #[qinvokable]
        fn piano_key_sum(self: &TestHelper) -> i32;

        /// A file URL for `name` in a fresh directory of this process's
        /// own under the temporary directory.
        #[qinvokable]
        fn temp_file_url(self: &TestHelper, name: &QString) -> QUrl;

        /// Whether the local file at `url` exists.
        #[qinvokable]
        fn file_exists(self: &TestHelper, url: &QUrl) -> bool;
    }
}

#[derive(Default)]
pub struct TestHelperRust;

impl self_test::TestHelper {
    fn save_screenshot(&self, title: &QString, path: &QString) -> bool {
        crate::ffi::save_window_screenshot(&title.to_string(), &path.to_string())
    }

    fn load_demo_project(&self) -> bool {
        crate::state::with(|s| match crate::demo::build() {
            Ok(doc) => {
                s.replace_document(doc);
                true
            }
            Err(e) => {
                // Test-only; the self test reports the failure.
                eprintln!("demo project: {e}");
                false
            }
        })
    }

    fn load_stress_project(&self, tracks: i32, clips: i32) -> bool {
        let (tracks, clips) = (tracks.max(0) as usize, clips.max(0) as usize);
        crate::state::with(|s| match crate::demo::stress(tracks, clips) {
            Ok(doc) => {
                s.replace_document(doc);
                true
            }
            Err(e) => {
                eprintln!("stress project: {e}");
                false
            }
        })
    }

    fn now_ms(&self) -> f64 {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        START
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_secs_f64()
            * 1000.0
    }

    fn remove_temp_files(&self) {
        crate::demo::remove_temp_files();
    }

    fn open_first_midi_clip(&self) -> bool {
        crate::state::with(|s| {
            let clip = s
                .doc
                .project()
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| matches!(c.content, daw_model::ClipContent::Midi(_)))
                .map(|c| c.id);
            match clip {
                Some(id) => {
                    s.apply(daw_arrange::Outcome {
                        open_clip: Some(id),
                        ..Default::default()
                    });
                    true
                }
                None => false,
            }
        })
    }

    fn piano_note_count(&self) -> i32 {
        crate::state::with(|s| s.piano.notes(s.doc.project()).len() as i32)
    }

    fn piano_key_sum(&self) -> i32 {
        crate::state::with(|s| {
            s.piano
                .notes(s.doc.project())
                .iter()
                .map(|n| i32::from(n.key))
                .sum()
        })
    }

    fn temp_file_url(&self, name: &QString) -> QUrl {
        let dir = std::env::temp_dir().join(format!("kadence-test-{}", std::process::id()));
        // A missing directory shows up as a failed export in the test.
        let _ = std::fs::create_dir_all(&dir);
        QUrl::from_local_file(&QString::from(
            dir.join(name.to_string()).display().to_string().as_str(),
        ))
    }

    fn file_exists(&self, url: &QUrl) -> bool {
        url.to_local_file()
            .is_some_and(|p| std::path::Path::new(&p.to_string()).exists())
    }
}
