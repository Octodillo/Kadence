//! Where each plugin format's bundles are installed.
//!
//! Every format follows the same layout: a hidden directory in the user's home, a directory
//! named after the format in each system library directory, and an environment variable that
//! lists more. [`default_search_paths`] builds that list for one format, so every format looks
//! in the same places (lib64 and multiarch distributions, Flatpak) and they cannot drift apart.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::PluginFormatKind;

/// The system library directories plugins are installed under: `lib` and `lib64` (Fedora,
/// openSUSE), in `/usr/local` first so that a plugin the administrator installed wins over the
/// distribution's copy, as it does for programs and libraries.
const LIBRARY_DIRS: [&str; 4] = [
    "/usr/local/lib",
    "/usr/local/lib64",
    "/usr/lib",
    "/usr/lib64",
];

/// The multiarch library directory (Debian, Ubuntu) of the architecture this build runs on.
/// Other architectures' directories hold plugins this process cannot load.
fn multiarch_dir() -> PathBuf {
    PathBuf::from(format!("/usr/lib/{}-linux-gnu", std::env::consts::ARCH))
}

/// Inside Flatpak: the plugin extension point (`org.freedesktop.LinuxAudio.Plugins`) and the
/// plugins bundled with the application.
const FLATPAK_DIRS: [&str; 2] = ["/app/extensions/Plugins", "/app/lib"];

/// How one format names its directories.
struct Layout {
    /// The directory inside each library directory, and after the dot in the home directory.
    dir: &'static str,
    /// The environment variable that lists more directories, in `PATH` syntax.
    env: &'static str,
    /// Whether the variable replaces the home and system directories (LV2, as lilv does) or is
    /// searched before them (CLAP, as its specification says, and VST3 by the same convention;
    /// the VST 3 specification names no variable).
    env_replaces: bool,
}

fn layout(kind: &PluginFormatKind) -> Layout {
    match kind {
        PluginFormatKind::Clap => Layout {
            dir: "clap",
            env: "CLAP_PATH",
            env_replaces: false,
        },
        PluginFormatKind::Lv2 => Layout {
            dir: "lv2",
            env: "LV2_PATH",
            env_replaces: true,
        },
        PluginFormatKind::Vst3 => Layout {
            dir: "vst3",
            env: "VST3_PATH",
            env_replaces: false,
        },
    }
}

/// What the search paths depend on outside the format: read from the process by
/// [`Environment::current`], or made up by a test.
struct Environment {
    /// The format's environment variable.
    env_paths: Option<OsString>,
    home: Option<OsString>,
    flatpak: bool,
}

impl Environment {
    fn current(layout: &Layout) -> Self {
        Self {
            env_paths: std::env::var_os(layout.env),
            home: std::env::var_os("HOME"),
            flatpak: Path::new("/.flatpak-info").is_file(),
        }
    }
}

/// The paths to search for `kind`'s bundles, in order of precedence: the format's
/// environment variable (`CLAP_PATH`, `LV2_PATH`, `VST3_PATH`), the user's home, the system
/// library directories and, inside Flatpak, the plugin extension and the application's own
/// plugins. `LV2_PATH` replaces the home and system directories; the other two add to them.
///
/// Only paths that exist are returned, each once: the same directory is often listed twice or
/// reached through a symlink (`/usr/lib64` is a link to `/usr/lib` on Arch). A path from the
/// variable may also be a single bundle.
pub fn default_search_paths(kind: &PluginFormatKind) -> Vec<PathBuf> {
    let layout = layout(kind);
    existing(listed(&layout, &Environment::current(&layout)))
}

/// Every path the layout names in `environment`, whether or not it exists.
fn listed(layout: &Layout, environment: &Environment) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = environment
        .env_paths
        .as_deref()
        .map(|listed| {
            std::env::split_paths(listed)
                .filter(|p| !p.as_os_str().is_empty())
                .collect()
        })
        .unwrap_or_default();
    if paths.is_empty() || !layout.env_replaces {
        if let Some(home) = &environment.home {
            paths.push(Path::new(home).join(format!(".{}", layout.dir)));
        }
        let libraries = LIBRARY_DIRS.iter().map(PathBuf::from);
        paths.extend(
            libraries
                .chain([multiarch_dir()])
                .map(|lib| lib.join(layout.dir)),
        );
    }
    if environment.flatpak {
        paths.extend(
            FLATPAK_DIRS
                .iter()
                .map(|lib| Path::new(lib).join(layout.dir)),
        );
    }
    paths
}

/// The paths that exist, without the ones that name an earlier path again.
fn existing(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    paths
        .into_iter()
        .filter(|p| p.canonicalize().is_ok_and(|real| seen.insert(real)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment(env_paths: Option<&str>, flatpak: bool) -> Environment {
        Environment {
            env_paths: env_paths.map(OsString::from),
            home: Some("/home/me".into()),
            flatpak,
        }
    }

    fn paths(kind: PluginFormatKind, env_paths: Option<&str>, flatpak: bool) -> Vec<String> {
        listed(&layout(&kind), &environment(env_paths, flatpak))
            .into_iter()
            .map(|p| p.to_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn every_format_covers_lib64_and_this_builds_multiarch() {
        let arch = std::env::consts::ARCH;
        for (kind, dir) in [
            (PluginFormatKind::Clap, "clap"),
            (PluginFormatKind::Lv2, "lv2"),
            (PluginFormatKind::Vst3, "vst3"),
        ] {
            assert_eq!(
                paths(kind, None, false),
                [
                    format!("/home/me/.{dir}"),
                    format!("/usr/local/lib/{dir}"),
                    format!("/usr/local/lib64/{dir}"),
                    format!("/usr/lib/{dir}"),
                    format!("/usr/lib64/{dir}"),
                    format!("/usr/lib/{arch}-linux-gnu/{dir}"),
                ]
            );
        }
    }

    #[test]
    fn the_variable_comes_first_and_replaces_only_lv2s_defaults() {
        let clap = paths(PluginFormatKind::Clap, Some("/a:/b"), false);
        assert_eq!(clap[..3], ["/a", "/b", "/home/me/.clap"]);
        let vst3 = paths(PluginFormatKind::Vst3, Some("/a"), false);
        assert_eq!(vst3[..2], ["/a", "/home/me/.vst3"]);
        assert_eq!(
            paths(PluginFormatKind::Lv2, Some("/a:/b"), false),
            ["/a", "/b"]
        );
        // An empty variable is no variable.
        assert_eq!(
            paths(PluginFormatKind::Lv2, Some(""), false),
            paths(PluginFormatKind::Lv2, None, false)
        );
    }

    #[test]
    fn flatpak_adds_its_plugin_directories_last() {
        let native = paths(PluginFormatKind::Clap, None, false);
        let flatpak = paths(PluginFormatKind::Clap, None, true);
        assert_eq!(flatpak[..native.len()], native[..]);
        assert_eq!(
            flatpak[native.len()..],
            ["/app/extensions/Plugins/clap", "/app/lib/clap"]
        );
        // LV2_PATH replaces the standard locations but not Flatpak's.
        assert_eq!(
            paths(PluginFormatKind::Lv2, Some("/a"), true),
            ["/a", "/app/extensions/Plugins/lv2", "/app/lib/lv2"]
        );
    }

    #[test]
    fn missing_and_repeated_paths_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("lib");
        let link = dir.path().join("lib64");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        // A path may be one bundle rather than a directory of them.
        let bundle = dir.path().join("one.clap");
        std::fs::write(&bundle, "").unwrap();
        let found = existing(vec![
            real.clone(),
            dir.path().join("missing"),
            link,
            bundle.clone(),
            real.clone(),
        ]);
        assert_eq!(found, [real, bundle]);
    }
}
