use std::path::PathBuf;
use std::process::Command;

use cxx_qt_build::{CxxQtBuilder, QmlModule};

/// KDE Frameworks used from `src/cpp`, as (include dirs, library).
const KF6: &[(&[&str], &str)] = &[
    (&["KCoreAddons"], "KF6CoreAddons"),
    (&["KI18n"], "KF6I18n"),
    (&["KI18n"], "KF6I18nQml"),
    (&["KCrash"], "KF6Crash"),
    (&["KConfig", "KConfigCore"], "KF6ConfigCore"),
    (&["KColorScheme"], "KF6ColorScheme"),
    (&["KIconThemes"], "KF6IconThemes"),
];

const QML_FILES: &[&str] = &[
    "qml/Main.qml",
    "qml/ArrangePage.qml",
    "qml/MixPage.qml",
    "qml/ChannelStrip.qml",
    "qml/LevelMeter.qml",
    "qml/GainReductionMeter.qml",
    "qml/EditPage.qml",
    "qml/SettingsPage.qml",
    "qml/AudioSettings.qml",
    "qml/EngineErrorText.qml",
    "qml/ProjectMessageText.qml",
    "qml/EditNames.qml",
    "qml/WorkspacePage.qml",
    "qml/SelfTest.qml",
    "qml/TrackHeader.qml",
];

/// Ask qmake (`QMAKE`, else `qmake6`, else `qmake`) for a Qt install path.
fn qmake_query(var: &str) -> PathBuf {
    println!("cargo:rerun-if-env-changed=QMAKE");
    let candidates = match std::env::var("QMAKE") {
        Ok(qmake) => vec![qmake],
        Err(_) => vec!["qmake6".to_owned(), "qmake".to_owned()],
    };
    for qmake in &candidates {
        let Ok(output) = Command::new(qmake).args(["-query", var]).output() else {
            continue;
        };
        let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if output.status.success() && !value.is_empty() {
            return PathBuf::from(value);
        }
    }
    panic!("could not get {var} from {candidates:?}; install Qt 6 or set QMAKE");
}

/// Where KDE Frameworks lives: `KF6_PREFIX`, else the Qt prefix (distros
/// install both under /usr).
fn kf6_prefix() -> PathBuf {
    println!("cargo:rerun-if-env-changed=KF6_PREFIX");
    std::env::var_os("KF6_PREFIX")
        .map(PathBuf::from)
        .unwrap_or_else(|| qmake_query("QT_INSTALL_PREFIX"))
}

fn main() {
    let prefix = kf6_prefix();
    let include = prefix.join("include/KF6");
    assert!(
        include.is_dir(),
        "KDE Frameworks 6 headers not found in {}; set KF6_PREFIX",
        include.display()
    );

    let mut builder = CxxQtBuilder::new_qml_module(
        QmlModule::new("io.github.octodillo.daw").qml_files(QML_FILES.iter().copied()),
    )
    .files([
        "src/bridge.rs",
        "src/mixer_bridge.rs",
        "src/ffi.rs",
        "src/test_helper.rs",
        "src/project.rs",
        "src/arrange_view.rs",
        "src/piano_roll.rs",
        "src/paint.rs",
    ])
    .qt_module("Gui")
    .qt_module("Widgets")
    .qt_module("Quick")
    .qt_module("QuickControls2")
    .cpp_file("src/cpp/kadence.cpp")
    .cpp_file("src/cpp/scenepainter.cpp");
    let mut include_dirs: Vec<PathBuf> = KF6
        .iter()
        .flat_map(|(dirs, _)| dirs.iter().map(|d| include.join(d)))
        .collect();
    // Header-only Qt module that KLocalizedQmlContext includes.
    include_dirs.push(qmake_query("QT_INSTALL_HEADERS").join("QtQmlIntegration"));
    // SAFETY: only adds include paths; no flags that change the ABI.
    builder = unsafe {
        builder.cc_builder(|cc| {
            cc.includes(&include_dirs);
        })
    };
    builder.build();

    // Distros put KF6 next to Qt's libraries (/usr/lib or a multiarch dir).
    let lib_dir = match std::env::var_os("KF6_PREFIX") {
        Some(_) => prefix.join("lib"),
        None => qmake_query("QT_INSTALL_LIBS"),
    };
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    for (_, lib) in KF6 {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }
    println!("cargo:rerun-if-changed=src/cpp");
}
