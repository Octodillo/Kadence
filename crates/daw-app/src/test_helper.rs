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
}
