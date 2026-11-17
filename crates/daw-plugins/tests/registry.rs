//! Registry, scanner and cache tests. The end-to-end ones run the `daw-plugin-scan` binary on
//! the CLAP test plugin.

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
        // The same build as every other test that uses this target directory: a
        // different package set unifies features differently and rebuilds the
        // plugin while another test process is copying or loading it.
        let status = Command::new(cargo)
            .args([
                "build",
                "--quiet",
                "--package",
                "daw-clap-test-plugin",
                "--package",
                "daw-plugin-host",
                "--bin",
                "daw-plugin-host",
                "--lib",
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

/// The default registry with only CLAP searched, so plugins installed on the
/// machine (LV2 and VST3 search their system paths) cannot change the results.
fn clap_only_registry() -> PluginRegistry {
    let mut registry = default_registry();
    registry.set_search_paths(PluginFormatKind::Lv2, Vec::new());
    registry.set_search_paths(PluginFormatKind::Vst3, Vec::new());
    registry
}

fn scanner() -> Scanner {
    Scanner::new(env!("CARGO_BIN_EXE_daw-plugin-scan"), Vec::<String>::new())
}

fn sh(script: &str) -> Scanner {
    Scanner::new("sh", ["-c", script, "sh"])
}

/// What a scanner prints for a bundle that loads and holds no plugins.
const NO_PLUGINS: &str = r#"{"plugins":[]}"#;

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
fn bundles_reached_through_links_are_found_once() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("plugins");
    std::fs::create_dir_all(root.join("vendor")).unwrap();
    std::fs::write(root.join("vendor/one.clap"), "").unwrap();
    // A link back to the root, one to a sibling directory, and a second name for the bundle.
    symlink(&root, root.join("vendor/all")).unwrap();
    symlink(root.join("vendor"), root.join("zz-vendor")).unwrap();
    symlink(root.join("vendor/one.clap"), root.join("zz-one.clap")).unwrap();
    symlink(root.join("missing.clap"), root.join("broken.clap")).unwrap();
    // A second search path that is the first one again.
    let alias = dir.path().join("alias");
    symlink(&root, &alias).unwrap();

    assert_eq!(
        find_candidates(&PluginFormatKind::Clap, &[root.clone(), alias.clone()]),
        vec![root.join("vendor/one.clap")]
    );
    // The first path that leads to a bundle names it.
    assert_eq!(
        find_candidates(&PluginFormatKind::Clap, &[alias.clone(), root]),
        vec![alias.join("vendor/one.clap")]
    );
}

/// A link to a bundle is the bundle, whatever the link is called: it is not walked as a plain
/// directory, which would hide the bundle when its own name comes up.
#[test]
fn a_link_without_the_extension_does_not_hide_its_bundle() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("plugins");
    std::fs::create_dir_all(root.join("Synth.vst3/Contents")).unwrap();
    // Sorts before the bundle's own name.
    symlink(root.join("Synth.vst3"), root.join("Current")).unwrap();
    assert_eq!(
        find_candidates(&PluginFormatKind::Vst3, std::slice::from_ref(&root)),
        vec![root.join("Current")]
    );
    // A search path that is such a link, ahead of the one that holds the bundle.
    assert_eq!(
        find_candidates(
            &PluginFormatKind::Vst3,
            &[root.join("Current"), root.clone()]
        ),
        vec![root.join("Current")]
    );
}

/// A directory first reached too deep (through a link) for its bundles to be in range is
/// walked again when it comes up nearer the root.
#[test]
fn a_directory_cut_short_through_a_link_is_walked_again_nearer_the_root() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("plugins");
    std::fs::create_dir_all(root.join("a/b/c/d/e/f")).unwrap();
    std::fs::create_dir_all(root.join("z/x/y")).unwrap();
    std::fs::write(root.join("z/x/y/p.clap"), "").unwrap();
    symlink(root.join("z"), root.join("a/b/c/d/e/f/l")).unwrap();
    assert_eq!(
        find_candidates(&PluginFormatKind::Clap, std::slice::from_ref(&root)),
        vec![root.join("z/x/y/p.clap")]
    );
}

#[test]
fn a_bundles_time_follows_its_links_once() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let later = |seconds| std::time::SystemTime::now() + Duration::from_secs(seconds);
    let touch = |path: &Path, at| {
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(at).unwrap();
    };
    // A bundle whose contents live elsewhere, as versioned installs do.
    let contents = dir.path().join("synth-1.2/Contents");
    std::fs::create_dir_all(contents.join("x86_64-linux")).unwrap();
    std::fs::write(contents.join("x86_64-linux/Synth.so"), "").unwrap();
    let bundle = dir.path().join("Synth.vst3");
    std::fs::create_dir(&bundle).unwrap();
    symlink(&contents, bundle.join("Contents")).unwrap();
    // Links back up the tree end the walk instead of looping.
    symlink(&bundle, contents.join("loop")).unwrap();
    symlink(dir.path(), contents.join("up")).unwrap();
    let before = daw_plugins::modified_time(&bundle).unwrap();

    // A change behind the linked directory counts, also when the bundle is reached by a link.
    touch(&contents.join("x86_64-linux/Synth.so"), later(60));
    let changed = daw_plugins::modified_time(&bundle).unwrap();
    assert!(changed > before);
    let alias = dir.path().join("Alias.vst3");
    symlink(&bundle, &alias).unwrap();
    assert_eq!(daw_plugins::modified_time(&alias), Some(changed));

    // So does a linked file's target.
    let shared = dir.path().join("shared.so");
    std::fs::write(&shared, "").unwrap();
    touch(&shared, later(120));
    symlink(&shared, contents.join("x86_64-linux/shared.so")).unwrap();
    assert!(daw_plugins::modified_time(&bundle).unwrap() > changed);
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
    assert!(found.skipped.is_empty(), "{:?}", found.skipped);
    let found = found.plugins;
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

/// A plugin that hangs as the scanner exits has already had its result written.
#[test]
fn a_scanner_that_hangs_after_a_complete_result_keeps_it() {
    // Long enough for a loaded machine to start `sh` and print: these scans end at the timeout.
    let patient = Duration::from_secs(3);
    let found = sh(&format!("echo '{NO_PLUGINS}'; sleep 60"))
        .with_timeout(patient)
        .scan(&PluginFormatKind::Clap, Path::new("/x.clap"))
        .unwrap();
    assert!(found.plugins.is_empty());
    match sh(r#"printf '{"load_failed":"boom"}'; sleep 60"#)
        .with_timeout(patient)
        .scan(&PluginFormatKind::Clap, Path::new("/x.clap"))
    {
        Err(ScanError::LoadFailed(why)) => assert_eq!(why, "boom"),
        other => panic!("expected a load failure, got {other:?}"),
    }
    let err = sh(r#"printf '{"plugins":['; sleep 60"#)
        .with_timeout(Duration::from_millis(300))
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
    let mut registry = clap_only_registry();
    registry.set_search_paths(PluginFormatKind::Clap, vec![dir.path().to_owned()]);
    let cache_file = dir.path().join("cache/plugins.json");

    let mut cache = ScanCache::load(&cache_file);
    assert!(cache.is_empty());
    let seen = std::sync::Mutex::new(Vec::new());
    let report = registry.scan_with_progress(&scanner(), &mut cache, &|done, total| {
        seen.lock().unwrap().push((done, total));
    });
    assert_eq!(seen.into_inner().unwrap(), [(0, 1), (1, 1)]);
    assert_eq!((report.scanned, report.cached), (1, 0));
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(registry.plugins().len(), 1);
    cache.save(&cache_file).unwrap();

    // A second scan, from the saved cache, loads nothing: the scanner would fail if it ran.
    let mut cache = ScanCache::load(&cache_file);
    assert_eq!(cache.len(), 1);
    let seen = std::sync::Mutex::new(Vec::new());
    let report = registry.scan_with_progress(&sh("exit 1"), &mut cache, &|done, total| {
        seen.lock().unwrap().push((done, total));
    });
    assert_eq!(
        seen.into_inner().unwrap(),
        [(1, 1)],
        "cached bundles count as done"
    );
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
    let mut registry = clap_only_registry();
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
    let found = sh(&format!("sleep 10 & echo '{NO_PLUGINS}'"))
        .with_timeout(Duration::from_secs(5))
        .scan(&PluginFormatKind::Clap, Path::new("/x.clap"))
        .unwrap();
    assert!(found.plugins.is_empty());
    assert!(start.elapsed() < Duration::from_secs(3));
}

#[test]
fn a_scanner_without_the_format_is_not_a_crash() {
    let err = sh("exit 64")
        .scan(&PluginFormatKind::Clap, Path::new("/x.clap"))
        .unwrap_err();
    assert!(matches!(err, ScanError::Unsupported), "{err}");
    assert!(!err.is_plugin_fault());
}

#[test]
fn forgotten_failures_are_scanned_again() {
    let dir = plugin_dir();
    let mut registry = clap_only_registry();
    registry.set_search_paths(PluginFormatKind::Clap, vec![dir.path().to_owned()]);
    let mut cache = ScanCache::new();
    registry.scan(&sh("kill -SEGV $$"), &mut cache);
    assert_eq!(cache.len(), 1);
    cache.forget_failures();
    assert!(cache.is_empty());
    let report = registry.scan(&scanner(), &mut cache);
    assert_eq!((report.scanned, report.failed.len()), (1, 0));
}

/// A bundle that does not load is a cached failure that says why, not a bundle without plugins.
#[test]
fn bundles_that_do_not_load_are_reported_and_cached() {
    let dir = tempfile::tempdir().unwrap();
    let broken = dir.path().join("broken.clap");
    std::fs::write(&broken, b"not a shared library").unwrap();
    match scanner().scan(&PluginFormatKind::Clap, &broken) {
        Err(e @ ScanError::LoadFailed(_)) => {
            assert!(e.is_plugin_fault());
            assert!(e.to_string().contains("broken.clap"), "{e}");
        }
        other => panic!("expected a load failure, got {other:?}"),
    }

    let mut registry = clap_only_registry();
    registry.set_search_paths(PluginFormatKind::Clap, vec![dir.path().to_owned()]);
    let mut cache = ScanCache::new();
    let report = registry.scan(&scanner(), &mut cache);
    assert_eq!(report.failed.len(), 1, "{report:?}");
    assert_eq!(report.failed[0].0, broken);
    assert!(report.failed[0].1.contains("did not load"), "{report:?}");
    assert_eq!(cache.len(), 1);

    // The reason counts even when the scanner crashes on its way out.
    match sh(r#"printf '{"load_failed":"boom"}'; kill -SEGV $$"#)
        .scan(&PluginFormatKind::Clap, &broken)
    {
        Err(ScanError::LoadFailed(why)) => assert_eq!(why, "boom"),
        other => panic!("expected a load failure, got {other:?}"),
    }

    // The cached failure is reported again without a scan, until a rescan forgets it.
    let report = registry.scan(&scanner(), &mut cache);
    assert_eq!(
        (report.cached, report.scanned, report.failed.len()),
        (1, 0, 1)
    );
    cache.forget_failures();
    assert!(cache.is_empty());
}

#[test]
fn scanner_problems_are_not_cached_as_plugin_failures() {
    let dir = plugin_dir();
    let mut registry = clap_only_registry();
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
    let mut registry = clap_only_registry();
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

/// LV2 goes through the same out-of-process scan, when mda-lv2 is installed.
#[test]
fn registry_scans_and_instantiates_lv2() {
    let dirs = daw_plugins_lv2::format().default_search_paths();
    let Some(dir) = daw_test_support::lv2::find_bundles(dirs, &["mda.lv2"]) else {
        return;
    };
    let mda = dir.join("mda.lv2");
    let dir = tempfile::tempdir().unwrap();
    let mut registry = default_registry();
    registry.set_search_paths(PluginFormatKind::Clap, Vec::new());
    registry.set_search_paths(PluginFormatKind::Vst3, Vec::new());
    registry.set_search_paths(PluginFormatKind::Lv2, vec![mda]);
    let mut cache = ScanCache::load(&dir.path().join("plugins.json"));
    let report = registry.scan(&scanner(), &mut cache);
    assert_eq!(report.scanned, 1);
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    let d = registry
        .find(
            &PluginFormatKind::Lv2,
            "http://drobilla.net/plugins/mda/EPiano",
        )
        .expect("mda ePiano not found")
        .clone();
    assert!(d.is_instrument);
    let plugin = registry.instantiate(&d, 48_000.0, 256).unwrap();
    assert_eq!(plugin.ports().audio_out, 2);
}

#[test]
fn a_scanner_that_crashes_after_a_complete_result_keeps_it() {
    // Plugins often crash in exit-time destructors, after the result is already written.
    let found = sh(&format!("echo '{NO_PLUGINS}'; kill -SEGV $$"))
        .scan(&PluginFormatKind::Clap, Path::new("/x.clap"))
        .unwrap();
    assert!(found.plugins.is_empty());
    let err = sh(r#"printf '{"plugins":['; kill -SEGV $$"#)
        .scan(&PluginFormatKind::Clap, Path::new("/x.clap"))
        .unwrap_err();
    assert!(matches!(err, ScanError::Crashed(_)), "{err}");
}

#[test]
fn a_hanging_scanners_helpers_are_killed_with_it() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let script = format!("sleep 30 & echo $! > '{}'; wait", pid_file.display());
    let err = sh(&script)
        .with_timeout(Duration::from_secs(1))
        .scan(&PluginFormatKind::Clap, Path::new("/x.clap"))
        .unwrap_err();
    assert!(matches!(err, ScanError::TimedOut(_)), "{err}");
    let pid = std::fs::read_to_string(&pid_file).unwrap();
    let stat = Path::new("/proc").join(pid.trim()).join("stat");
    // The kill is asynchronous; give it generous time before calling the helper a survivor.
    let gone = (0..500).any(|_| {
        let alive = std::fs::read_to_string(&stat).is_ok_and(|s| {
            s.rsplit(')')
                .next()
                .is_some_and(|r| !r.trim_start().starts_with('Z'))
        });
        if alive {
            std::thread::sleep(Duration::from_millis(10));
        }
        !alive
    });
    assert!(gone, "the helper outlived its scanner");
}

#[test]
fn bundles_without_plugins_are_retried_with_failures() {
    // A CLAP that fails to load scans as an empty list, so "rescan failed plugins" must retry
    // empty results too.
    let dir = plugin_dir();
    let mut registry = clap_only_registry();
    registry.set_search_paths(PluginFormatKind::Clap, vec![dir.path().to_owned()]);
    let mut cache = ScanCache::new();
    registry.scan(&sh(&format!("echo '{NO_PLUGINS}'")), &mut cache);
    assert_eq!(cache.len(), 1);
    cache.forget_failures();
    assert!(cache.is_empty());
    let report = registry.scan(&scanner(), &mut cache);
    assert_eq!((report.scanned, report.failed.len()), (1, 0));
    assert_eq!(registry.plugins().len(), 1);
}

/// A bundle that loads but cannot describe one of its plugins lists the others, reports the
/// skipped one as a failure, and is scanned again by "rescan failed plugins".
#[test]
fn skipped_plugins_are_reported_and_retried() {
    let dir = plugin_dir();
    let bundle = dir.path().join("Vendor/test.clap");
    let mut registry = clap_only_registry();
    registry.set_search_paths(PluginFormatKind::Clap, vec![dir.path().to_owned()]);
    let mut cache = ScanCache::new();

    let partial = concat!(
        r#"{"plugins":[{"format":"Clap","id":"a","name":"A","vendor":"","#,
        r#""path":"/x.clap","is_instrument":false,"ports":[2,2]}],"#,
        r#""skipped":["B: no licence"]}"#
    );
    let report = registry.scan(&sh(&format!("echo '{partial}'")), &mut cache);
    assert_eq!(registry.plugins().len(), 1);
    assert_eq!(
        report.failed,
        [(bundle.clone(), "B: no licence".to_owned())]
    );

    // From the cache, the skipped plugin is still reported.
    let report = registry.scan(&sh("exit 1"), &mut cache);
    assert_eq!((report.cached, report.failed.len()), (1, 1));

    cache.forget_failures();
    assert!(cache.is_empty(), "a partial result is retried");
    let report = registry.scan(&scanner(), &mut cache);
    assert_eq!((report.scanned, report.failed.len()), (1, 0));
    assert!(
        registry
            .find(&PluginFormatKind::Clap, "org.daw.test.gain")
            .is_some()
    );
}

/// A bundle replaced while its scan ran is scanned again: the first result may be the old
/// file's, and must not be cached under the new file's time.
#[test]
fn a_bundle_replaced_during_its_scan_is_scanned_again() {
    let dir = plugin_dir();
    let mut registry = clap_only_registry();
    registry.set_search_paths(PluginFormatKind::Clap, vec![dir.path().to_owned()]);
    let mut cache = ScanCache::new();

    // The scanner's arguments are `--scan clap <bundle>`. The first scan finds the bundle
    // replaced under it; the second, of the new file, says so.
    let marker = tempfile::tempdir().unwrap();
    let replaced_once = sh(&format!(
        r#"if [ -e "{marker}" ]; then printf '{{"load_failed":"the new file"}}'; exit 65; fi
           : > "{marker}"; touch -d 2030-01-01 "$3"; echo '{NO_PLUGINS}'"#,
        marker = marker.path().join("scanned").display()
    ));
    let report = registry.scan(&replaced_once, &mut cache);
    assert_eq!(report.scanned, 1);
    assert_eq!(report.failed.len(), 1, "{:?}", report.failed);
    assert!(report.failed[0].1.contains("the new file"));

    // The second result is cached as the new file's.
    let report = registry.scan(&sh("exit 1"), &mut cache);
    assert_eq!((report.scanned, report.cached), (0, 1));
    assert!(report.failed[0].1.contains("the new file"));
}

/// A plugin that writes into its own bundle whenever it is loaded is still cached, or it would
/// be loaded at every start.
#[test]
fn a_bundle_that_changes_itself_when_loaded_is_cached() {
    let dir = plugin_dir();
    let mut registry = clap_only_registry();
    registry.set_search_paths(PluginFormatKind::Clap, vec![dir.path().to_owned()]);
    let mut cache = ScanCache::new();

    let counter = tempfile::NamedTempFile::new().unwrap();
    // Each load leaves the bundle with a later time than the last.
    let self_modifying = sh(&format!(
        r#"echo x >> "{counter}"; touch -d "2030-01-0$(wc -l < "{counter}" | tr -d ' ')" "$3"
           echo '{NO_PLUGINS}'"#,
        counter = counter.path().display()
    ));
    let report = registry.scan(&self_modifying, &mut cache);
    assert_eq!(report.scanned, 1);
    assert_eq!(cache.len(), 1);
    let report = registry.scan(&sh("exit 1"), &mut cache);
    assert_eq!((report.scanned, report.cached), (0, 1));
}

/// With no program to run, every bundle is reported with the reason and nothing is cached, so
/// a later scan that can run finds the plugins.
#[test]
fn an_unavailable_scanner_reports_why_and_caches_nothing() {
    let dir = plugin_dir();
    let mut registry = clap_only_registry();
    registry.set_search_paths(PluginFormatKind::Clap, vec![dir.path().to_owned()]);
    let mut cache = ScanCache::new();

    let unavailable = Scanner::unavailable("no /proc");
    let err = unavailable
        .scan(&PluginFormatKind::Clap, Path::new("/x.clap"))
        .unwrap_err();
    assert!(matches!(err, ScanError::Unavailable(_)), "{err}");
    assert!(!err.is_plugin_fault());

    let report = registry.scan(&unavailable, &mut cache);
    assert_eq!(report.failed.len(), 1);
    assert!(report.failed[0].1.contains("no /proc"), "{report:?}");
    assert!(cache.is_empty());

    let report = registry.scan(&scanner(), &mut cache);
    assert_eq!((report.scanned, report.failed.len()), (1, 0));
}
