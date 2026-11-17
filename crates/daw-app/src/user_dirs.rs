//! Where Kadence keeps its own files in the user's home, by the XDG base
//! directory rules. Flatpak points the variables at the app's own folders.
//!
//! Each is `None` when neither the variable nor `HOME` names an absolute
//! path. There is no fallback to the shared temporary folder: another user
//! of the machine could make that path first and read what goes into it.

use std::path::PathBuf;

/// Kadence's folder under `var`, else under `fallback` in the home folder.
fn kadence_dir(var: &str, fallback: &str) -> Option<PathBuf> {
    let absolute = |name: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    absolute(var)
        .or_else(|| absolute("HOME").map(|home| home.join(fallback)))
        .map(|base| base.join("kadence"))
}

/// For what the user would miss: `$XDG_DATA_HOME/kadence`, else
/// `~/.local/share/kadence`.
pub fn data() -> Option<PathBuf> {
    kadence_dir("XDG_DATA_HOME", ".local/share")
}

/// For what can be made again: `$XDG_CACHE_HOME/kadence`, else
/// `~/.cache/kadence`.
pub fn cache() -> Option<PathBuf> {
    kadence_dir("XDG_CACHE_HOME", ".cache")
}
