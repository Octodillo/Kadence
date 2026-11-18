use std::path::{Path, PathBuf};
use std::process::Command;

use cxx_qt_build::{CxxQtBuilder, QmlFile, QmlModule};

/// KDE Frameworks used from `src/cpp`, as (include dirs, library).
const KF6: &[(&[&str], &str)] = &[
    (&["KCoreAddons"], "KF6CoreAddons"),
    (&["KI18n"], "KF6I18n"),
    // KLocalizedQmlContext is in KI18n in older Frameworks and in KI18nQml
    // in newer ones (seen with 6.32).
    (&["KI18n", "KI18nQml"], "KF6I18nQml"),
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
    "qml/MetronomeStrip.qml",
    "qml/StripFrame.qml",
    "qml/TrackStrip.qml",
    "qml/MeterClock.qml",
    "qml/PointMenu.qml",
    "qml/SplitHandle.qml",
    "qml/LayoutSize.qml",
    "qml/DevicePanelDrawer.qml",
    "qml/DeviceNames.qml",
    "qml/LevelMeter.qml",
    "qml/GainReductionMeter.qml",
    "qml/EditPage.qml",
    "qml/SettingsPage.qml",
    "qml/AudioSettings.qml",
    "qml/MidiSettings.qml",
    "qml/GeneralSettings.qml",
    "qml/MidiInputButton.qml",
    "qml/EngineErrorText.qml",
    "qml/ProjectMessageText.qml",
    "qml/EditNames.qml",
    "qml/WorkspacePage.qml",
    "qml/SelfTest.qml",
    "qml/Benchmark.qml",
    "qml/TrackHeader.qml",
    "qml/LaneHeader.qml",
    "qml/AddLaneMenu.qml",
    "qml/LaneName.qml",
    "qml/TransportBar.qml",
    "qml/RecoveryDialog.qml",
    "qml/RelinkDialog.qml",
    "qml/PluginBrowser.qml",
    "qml/PluginSettings.qml",
    "qml/ExportDialog.qml",
    "qml/InstrumentButton.qml",
    "qml/LazyMenu.qml",
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

/// The directory holding the KF6 libraries. `KF6_LIBDIR` names it outright.
/// Without `KF6_PREFIX`, distros put KF6 next to Qt's libraries. With it,
/// it is the first of the prefix's `lib64` (on 64-bit targets), `lib` and
/// the multiarch directories under `lib` (such as `lib/x86_64-linux-gnu`,
/// the target's own first) that has them. If none does, the libraries
/// come from elsewhere on the linker's path, and `lib` is added as before.
fn kf6_lib_dir(prefix: &Path) -> PathBuf {
    println!("cargo:rerun-if-env-changed=KF6_LIBDIR");
    if let Some(dir) = std::env::var_os("KF6_LIBDIR").filter(|dir| !dir.is_empty()) {
        return PathBuf::from(dir);
    }
    if std::env::var_os("KF6_PREFIX").is_none() {
        return qmake_query("QT_INSTALL_LIBS");
    }
    let target = |key: &str| std::env::var(format!("CARGO_CFG_TARGET_{key}")).unwrap_or_default();
    let lib = prefix.join("lib");
    let mut candidates = Vec::new();
    if target("POINTER_WIDTH") == "64" {
        candidates.push(prefix.join("lib64"));
    }
    candidates.push(lib.clone());
    let mut multiarch: Vec<PathBuf> = std::fs::read_dir(&lib)
        .into_iter()
        .flatten()
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.is_dir())
        .collect();
    // In name order, so the choice doesn't depend on how the directory
    // happens to list them; the target's own triple (`x86_64-…`) first.
    let own = format!("{}-", target("ARCH"));
    multiarch.sort();
    multiarch.sort_by_key(|dir| {
        !dir.file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with(&own))
    });
    candidates.extend(multiarch);
    let name = KF6[0].1;
    let has_kf6 = |dir: &PathBuf| {
        ["so", "a"]
            .iter()
            .any(|ext| dir.join(format!("lib{name}.{ext}")).exists())
    };
    candidates.into_iter().find(has_kf6).unwrap_or_else(|| {
        println!(
            "cargo:warning=lib{name} not found under {}; set KF6_LIBDIR if linking fails",
            prefix.display()
        );
        lib
    })
}

fn main() {
    let prefix = kf6_prefix();
    let include = prefix.join("include/KF6");
    assert!(
        include.is_dir(),
        "KDE Frameworks 6 headers not found in {}; set KF6_PREFIX",
        include.display()
    );
    // Before the long C++ build, so a wrong prefix is reported at once.
    let lib_dir = kf6_lib_dir(&prefix);

    let mut builder = CxxQtBuilder::new_qml_module(
        QmlModule::new("io.github.octodillo.daw")
            .qml_files(QML_FILES.iter().copied())
            // Shared helpers without state: one instance for the whole app.
            .qml_file(QmlFile::from("qml/ValueText.qml").singleton(true)),
    )
    .files([
        "src/bridge.rs",
        "src/mixer_bridge.rs",
        "src/tracks_bridge.rs",
        "src/automation_bridge.rs",
        "src/device_panel_bridge.rs",
        "src/plugins_bridge.rs",
        "src/instruments_bridge.rs",
        "src/plugin_editors_bridge.rs",
        "src/ffi.rs",
        "src/test_helper.rs",
        "src/project.rs",
        "src/export_bridge.rs",
        "src/arrange_view.rs",
        "src/piano_roll.rs",
        "src/paint.rs",
        "src/settings_bridge.rs",
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

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    for (_, lib) in KF6 {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }
    println!("cargo:rerun-if-changed=src/cpp");
}
