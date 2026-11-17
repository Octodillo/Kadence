//! Finding the LV2 plugins that tests load.
//!
//! Tests that need a third-party LV2 bundle (mda-lv2, x42-plugins) skip when
//! it is not installed, so the suite runs on a bare machine. CI sets
//! [`REQUIRE_ENV`], which turns the skip into a failure, so a missing package
//! there cannot silently drop coverage.

use std::path::PathBuf;

/// When set, to any value (even `0` or nothing), a missing LV2 test plugin
/// fails the test instead of skipping it.
pub const REQUIRE_ENV: &str = "KADENCE_REQUIRE_LV2_PLUGINS";

/// The first of `search_paths` that holds every bundle in `bundles`
/// (directory names such as `mda.lv2`). Pass the host's own search paths
/// (`daw_plugins_lv2::format().default_search_paths()`), so the test looks
/// exactly where the host would.
///
/// Returns `None`, after printing why, when no directory has them all.
///
/// # Panics
/// If nothing is found and [`REQUIRE_ENV`] is set.
#[track_caller]
pub fn find_bundles(
    search_paths: impl IntoIterator<Item = PathBuf>,
    bundles: &[&str],
) -> Option<PathBuf> {
    let required = std::env::var_os(REQUIRE_ENV).is_some();
    find_bundles_or(search_paths, bundles, required)
}

#[track_caller]
fn find_bundles_or(
    search_paths: impl IntoIterator<Item = PathBuf>,
    bundles: &[&str],
    required: bool,
) -> Option<PathBuf> {
    let found = search_paths
        .into_iter()
        .find(|dir| bundles.iter().all(|b| dir.join(b).is_dir()));
    if found.is_none() {
        let names = bundles.join(", ");
        assert!(
            !required,
            "LV2 test plugins not installed ({names}), and {REQUIRE_ENV} is set"
        );
        eprintln!("skipped: LV2 test plugins not installed ({names})");
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_directory_holding_every_bundle() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::create_dir(a.path().join("one.lv2")).unwrap();
        std::fs::create_dir(b.path().join("one.lv2")).unwrap();
        std::fs::create_dir(b.path().join("two.lv2")).unwrap();
        let paths = || vec![a.path().to_owned(), b.path().to_owned()];
        assert_eq!(
            find_bundles_or(paths(), &["one.lv2"], true).as_deref(),
            Some(a.path())
        );
        assert_eq!(
            find_bundles_or(paths(), &["one.lv2", "two.lv2"], true).as_deref(),
            Some(b.path())
        );
        assert_eq!(find_bundles_or(paths(), &["three.lv2"], false), None);
    }

    #[test]
    #[should_panic(expected = "not installed")]
    fn a_missing_bundle_fails_when_required() {
        find_bundles_or(Vec::new(), &["one.lv2"], true);
    }
}
