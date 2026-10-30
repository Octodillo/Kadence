//! Test helpers for `--smoke-test` and `--screenshot-dir` (see SelfTest.qml).

use cxx_qt_lib::QString;

#[cxx_qt::bridge]
pub mod self_test {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
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
                s.status = e;
                false
            }
        })
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
}
