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

        /// Make a temporary folder holding a bundle `Files` whose clips play
        /// `outside/kick.wav` (outside the bundle) and `moved/snare.wav`
        /// (missing: the file is in `found/`). Returns the folder, or "".
        #[qinvokable]
        fn make_files_test(self: &TestHelper) -> QString;

        /// Leave a never-saved project with one track behind in a scratch
        /// folder, as a crash would, and offer it for recovery.
        #[qinvokable]
        fn offer_test_recovery(self: &TestHelper) -> bool;
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

    fn make_files_test(&self) -> QString {
        match files_test() {
            Ok(dir) => QString::from(dir.display().to_string().as_str()),
            Err(e) => {
                eprintln!("files test: {e}");
                QString::default()
            }
        }
    }

    fn offer_test_recovery(&self) -> bool {
        let base = test_dir("recovery");
        let result = (|| -> Result<(), String> {
            let scratch = crate::recovery::Scratch::create(&base).map_err(|e| e.to_string())?;
            let mut p = daw_model::Project::default();
            p.tracks
                .push(daw_model::Track::new(daw_model::TrackKind::Instrument, "Recovered").into());
            scratch.write(&p).map_err(|e| e.to_string())?;
            // Dropped without removing the folder, as a crash would.
            Ok(())
        })();
        if let Err(e) = result {
            eprintln!("recovery test: {e}");
            return false;
        }
        crate::state::with(|s| {
            s.recovery = crate::recovery::find(Some(&base), &[]);
            s.touch();
            !s.recovery.is_empty()
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

/// A fresh folder for one part of the self test.
fn test_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir()
        .join(format!("kadence-selftest-{}", std::process::id()))
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn files_test() -> Result<std::path::PathBuf, String> {
    use daw_core::time::{SampleTime, TickTime};
    use daw_model::{AudioClip, Clip, ClipContent, Document, Project, Track, TrackKind};

    let dir = test_dir("files");
    let err = |e: &dyn std::fmt::Display| e.to_string();
    let tone = daw_media::AudioData {
        sample_rate: 48_000,
        channels: vec![vec![0.25; 4_800]],
    };
    for f in ["outside/kick.wav", "found/snare.wav"] {
        let path = dir.join(f);
        std::fs::create_dir_all(path.parent().expect("has a folder")).map_err(|e| err(&e))?;
        daw_media::write_wav(&path, &tone, daw_media::WavFormat::Float32).map_err(|e| err(&e))?;
    }
    let clip = |name: &str, source: std::path::PathBuf, start: i64| {
        std::sync::Arc::new(Clip::new(
            name,
            TickTime(start),
            TickTime(daw_core::time::PPQ),
            ClipContent::Audio(AudioClip::new(source, SampleTime(0))),
        ))
    };
    let mut t = Track::new(TrackKind::Audio, "Drums");
    t.clips = vec![
        clip("Kick", dir.join("outside/kick.wav"), 0),
        clip("Snare", dir.join("moved/snare.wav"), daw_core::time::PPQ),
    ];
    let mut p = Project::new("Files");
    p.tracks.push(t.into());
    let mut doc = Document::new(p);
    doc.save_as(dir.join("Files")).map_err(|e| err(&e))?;
    Ok(dir)
}
