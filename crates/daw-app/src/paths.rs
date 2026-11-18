//! Where Kadence keeps files of its own: under the XDG base directories.
//! Flatpak points these at the app's own folders.

use std::ffi::OsString;
use std::path::PathBuf;

/// `kadence` in the user's data directory (`$XDG_DATA_HOME`, else
/// `~/.local/share`): unsaved projects' recovery copies and recordings.
pub fn data_dir() -> Option<PathBuf> {
    xdg_dir("XDG_DATA_HOME", ".local/share")
}

/// `kadence` in the user's cache directory (`$XDG_CACHE_HOME`, else
/// `~/.cache`): what can be made again, like plugin scan results.
pub fn cache_dir() -> Option<PathBuf> {
    xdg_dir("XDG_CACHE_HOME", ".cache")
}

fn xdg_dir(var: &str, home_default: &str) -> Option<PathBuf> {
    base_dir(
        std::env::var_os(var),
        std::env::var_os("HOME"),
        home_default,
    )
    .map(|base| base.join("kadence"))
}

/// An XDG base directory: `value` when it is an absolute path (the
/// specification says to ignore a relative one), else `home_default` under
/// `home`. `None` when there is neither.
fn base_dir(
    value: Option<OsString>,
    home: Option<OsString>,
    home_default: &str,
) -> Option<PathBuf> {
    value
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home.map(|h| PathBuf::from(h).join(home_default)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> Option<OsString> {
        Some(s.into())
    }

    #[test]
    fn an_absolute_variable_wins_over_home() {
        assert_eq!(
            base_dir(os("/data"), os("/home/u"), ".local/share"),
            Some("/data".into())
        );
    }

    #[test]
    fn an_unset_or_relative_variable_falls_back_to_home() {
        for value in [None, os("relative/data"), os("")] {
            assert_eq!(
                base_dir(value, os("/home/u"), ".cache"),
                Some("/home/u/.cache".into())
            );
        }
    }

    #[test]
    fn nothing_set_gives_nothing() {
        assert_eq!(base_dir(None, None, ".cache"), None);
        assert_eq!(base_dir(os("relative"), None, ".cache"), None);
    }
}
