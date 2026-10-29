//! Registry, scanner and cache tests. The end-to-end ones run the real `daw-plugin-scan` binary
//! on the CLAP test plugin.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use daw_core::plugin::PluginFormatKind;
use daw_plugins::{
    PluginRegistry, ScanCache, ScanError, Scanner, default_registry, find_candidates,
};

/// Builds `daw-clap-test-plugin` once per process in its own target directory.
fn test_plugin_lib() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let target = root.join("target").join("clap-test-plugin");
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let status = Command::new(cargo)
            .args([
                "build",
                "--quiet",
                "--package",
                "daw-clap-test-plugin",
                "--target-dir",
            ])
            .arg(&target)
            .current_dir(&root)
            .status()
            .expect("run cargo");
        assert!(status.success(), "building the test plugin failed");
        target.join("debug").join("libdaw_clap_test_plugin.so")
    })
}

fn scanner() -> Scanner {
    Scanner::new(env!("CARGO_BIN_EXE_daw-plugin-scan"), Vec::<String>::new())
}

fn sh(script: &str) -> Scanner {
    Scanner::new("sh", ["-c", script, "sh"])
}

/// A plugin folder with the test plugin in a vendor subdirectory and a file that is not a plugin.
fn plugin_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("Vendor")).unwrap();
    std::fs::copy(test_plugin_lib(), dir.path().join("Vendor/test.clap")).unwrap();
    std::fs::write(dir.path().join("readme.txt"), "hi").unwrap();
    dir
}

#[test]
fn candidates_are_found_without_descending_into_bundles() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    std::fs::create_dir_all(p.join("a/b")).unwrap();
    std::fs::write(p.join("a/b/one.clap"), "").unwrap();
    std::fs::write(p.join("two.CLAP"), "").unwrap();
    std::fs::write(p.join("three.so"), "").unwrap();
    std::fs::create_dir_all(p.join("Synth.vst3/Contents/x86_64-linux")).unwrap();
    std::fs::write(p.join("Synth.vst3/Contents/x86_64-linux/Synth.so"), "").unwrap();
    std::fs::create_dir_all(p.join("Nested.vst3/Inner.vst3")).unwrap();

    let roots = vec![p.to_owned(), p.join("missing")];
    assert_eq!(
        find_candidates(&PluginFormatKind::Clap, &roots),
        vec![p.join("a/b/one.clap"), p.join("two.CLAP")]
    );
    assert_eq!(
        find_candidates(&PluginFormatKind::Vst3, &roots),
        vec![p.join("Nested.vst3"), p.join("Synth.vst3")]
    );
    // A root can itself be a bundle.
    assert_eq!(
        find_candidates(&PluginFormatKind::Clap, &[p.join("two.CLAP")]),
        vec![p.join("two.CLAP")]
    );
}

#[test]
fn scanner_child_describes_the_plugin() {
    let dir = plugin_dir();
    let found = scanner()
        .scan(
            &PluginFormatKind::Clap,
            &dir.path().join("Vendor/test.clap"),
        )
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, "org.daw.test.gain");
    assert_eq!(found[0].ports, (2, 2));
}

#[test]
fn a_crashing_scanner_is_reported_not_fatal() {
    let err = sh("kill -SEGV $$")
        .scan(&PluginFormatKind::Clap, Path::new("/x.clap"))
        .unwrap_err();
    assert!(matches!(err, ScanError::Crashed(_)), "{err}");
}

#[test]
fn a_hanging_scanner_is_killed() {
    let err = sh("sleep 10")
        .with_timeout(Duration::from_millis(200))
        .scan(&PluginFormatKind::Clap, Path::new("/x.clap"))
        .unwrap_err();
    assert!(matches!(err, ScanError::TimedOut(_)), "{err}");
}

#[test]
fn garbage_output_and_missing_programs_are_errors() {
    let err = sh("echo nope")
        .scan(&PluginFormatKind::Clap, Path::new("/x.clap"))
        .unwrap_err();
    assert!(matches!(err, ScanError::BadOutput(_)), "{err}");
    let err = Scanner::new("/nonexistent/scanner", Vec::<String>::new())
        .scan(&PluginFormatKind::Clap, Path::new("/x.clap"))
        .unwrap_err();
    assert!(matches!(err, ScanError::Spawn(_)), "{err}");
}

#[test]
fn registry_scans_caches_and_instantiates() {
    let dir = plugin_dir();
    let mut registry = default_registry();
    registry.set_search_paths(PluginFormatKind::Clap, vec![dir.path().to_owned()]);
    let cache_file = dir.path().join("cache/plugins.json");

    let mut cache = ScanCache::load(&cache_file);
    assert!(cache.is_empty());
    let report = registry.scan(&scanner(), &mut cache);
    assert_eq!((report.scanned, report.cached), (1, 0));
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(registry.plugins().len(), 1);
    cache.save(&cache_file).unwrap();

    // A second scan, from the saved cache, loads nothing: the scanner would fail if it ran.
    let mut cache = ScanCache::load(&cache_file);
    assert_eq!(cache.len(), 1);
    let report = registry.scan(&sh("exit 1"), &mut cache);
    assert_eq!((report.scanned, report.cached), (0, 1));
    assert_eq!(registry.plugins().len(), 1);

    let d = registry
        .find(&PluginFormatKind::Clap, "org.daw.test.gain")
        .unwrap()
        .clone();
    let plugin = registry.instantiate(&d, 48_000.0, 128).unwrap();
    assert_eq!(plugin.latency(), 32);
}

#[test]
fn failures_are_cached_until_the_bundle_changes() {
    let dir = plugin_dir();
    let mut registry = default_registry();
    registry.set_search_paths(PluginFormatKind::Clap, vec![dir.path().to_owned()]);
    let mut cache = ScanCache::new();

    let report = registry.scan(&sh("kill -SEGV $$"), &mut cache);
    assert_eq!(report.failed.len(), 1);
    assert!(registry.plugins().is_empty());

    // Unchanged: the failure comes from the cache and the plugin is not loaded again.
    let report = registry.scan(&scanner(), &mut cache);
    assert_eq!(
        (report.scanned, report.cached, report.failed.len()),
        (0, 1, 1)
    );

    // Updated: rescanned, and this time it works.
    let bundle = dir.path().join("Vendor/test.clap");
    let later = std::time::SystemTime::now() + Duration::from_secs(5);
    std::fs::File::options()
        .write(true)
        .open(&bundle)
        .unwrap()
        .set_modified(later)
        .unwrap();
    let report = registry.scan(&scanner(), &mut cache);
    assert_eq!((report.scanned, report.failed.len()), (1, 0));
    assert_eq!(registry.plugins().len(), 1);

    // Removed: dropped from the plugin list and the cache.
    std::fs::remove_file(&bundle).unwrap();
    registry.scan(&scanner(), &mut cache);
    assert!(registry.plugins().is_empty());
    assert!(cache.is_empty());
}

#[test]
fn corrupt_cache_files_start_empty() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("plugins.json");
    std::fs::write(&file, "{ not json").unwrap();
    assert!(ScanCache::load(&file).is_empty());
    assert!(ScanCache::load(&dir.path().join("missing.json")).is_empty());
}

#[test]
fn registry_replaces_formats_of_the_same_kind_and_rejects_unknown_ones() {
    let mut registry = PluginRegistry::new();
    assert!(registry.format(&PluginFormatKind::Clap).is_none());
    registry.register(daw_plugins_clap::format());
    registry.register(daw_plugins_clap::format());
    assert_eq!(registry.formats().count(), 1);

    let d = daw_core::plugin::PluginDescriptor {
        format: PluginFormatKind::Vst3,
        id: "x".into(),
        name: "x".into(),
        vendor: String::new(),
        path: PathBuf::from("/x.vst3"),
        is_instrument: false,
        ports: (0, 0),
    };
    assert!(registry.instantiate(&d, 48_000.0, 64).is_err());
}

#[test]
fn scan_child_ignores_other_arguments() {
    let registry = default_registry();
    assert!(daw_plugins::run_scan_child(&registry, ["--help".into()]).is_none());
}

#[test]
fn a_helper_process_holding_the_pipe_cannot_hang_the_scan() {
    let start = std::time::Instant::now();
    let err = sh("sleep 10 & echo '[]'")
        .with_timeout(Duration::from_millis(300))
        .scan(&PluginFormatKind::Clap, Path::new("/x.clap"))
        .unwrap_err();
    assert!(matches!(err, ScanError::TimedOut(_)), "{err}");
    assert!(start.elapsed() < Duration::from_secs(3));
}

#[test]
fn scanner_problems_are_not_cached_as_plugin_failures() {
    let dir = plugin_dir();
    let mut registry = default_registry();
    registry.set_search_paths(PluginFormatKind::Clap, vec![dir.path().to_owned()]);
    let mut cache = ScanCache::new();

    let missing = Scanner::new("/nonexistent/scanner", Vec::<String>::new());
    let report = registry.scan(&missing, &mut cache);
    assert_eq!(report.failed.len(), 1);
    assert!(cache.is_empty());

    let report = registry.scan(&scanner(), &mut cache);
    assert_eq!((report.scanned, report.failed.len()), (1, 0));
    assert_eq!(registry.plugins().len(), 1);
}

#[test]
fn earlier_search_paths_win_for_duplicate_plugins() {
    let first = plugin_dir();
    let second = plugin_dir();
    let mut registry = default_registry();
    // Listed so that the second directory would sort first by path.
    let (a, b) = if first.path() < second.path() {
        (second.path(), first.path())
    } else {
        (first.path(), second.path())
    };
    registry.set_search_paths(PluginFormatKind::Clap, vec![a.to_owned(), b.to_owned()]);
    let report = registry.scan(&scanner(), &mut ScanCache::new());
    assert_eq!(report.scanned, 2);
    assert_eq!(registry.plugins().len(), 1);
    assert!(registry.plugins()[0].path.starts_with(a));
}
