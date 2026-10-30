//! Finding LV2 bundles and describing the plugins in them.

use std::path::{Path, PathBuf};

use daw_core::plugin::{PluginDescriptor, PluginFormatKind};

use crate::{bundle_uri, uri_to_path};

const INSTRUMENT_PLUGIN: &str = "http://lv2plug.in/ns/lv2core#InstrumentPlugin";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const DOAP_NAME: &str = "http://usefulinc.com/ns/doap#name";
const DOAP_MAINTAINER: &str = "http://usefulinc.com/ns/doap#maintainer";
const FOAF_NAME: &str = "http://xmlns.com/foaf/0.1/name";

/// `LV2_PATH` if set, otherwise the usual per-user and system locations
/// (Arch, Fedora/openSUSE lib64 and Debian/Ubuntu layouts). Inside Flatpak
/// the plugin extension (`org.freedesktop.LinuxAudio.Plugins`) and plugins
/// bundled with the app are searched after the others, even with `LV2_PATH`
/// set: the manifest cannot set `LV2_PATH` without dropping the standard
/// locations. Only existing directories are returned.
pub(crate) fn default_search_paths() -> Vec<PathBuf> {
    let mut paths = candidate_paths(
        std::env::var_os("LV2_PATH"),
        std::env::var_os("HOME"),
        Path::new("/.flatpak-info").is_file(),
    );
    // The same directory can be listed twice or reached through a symlink
    // (/usr/lib64 -> /usr/lib on Arch); scan it once.
    let mut seen = std::collections::HashSet::new();
    paths.retain(|p| p.is_dir() && seen.insert(p.canonicalize().unwrap_or_else(|_| p.clone())));
    paths
}

/// Where Flatpak's plugin extension point and the app's own plugins live.
const FLATPAK_PATHS: [&str; 2] = ["/app/extensions/Plugins/lv2", "/app/lib/lv2"];

fn candidate_paths(
    lv2_path: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
    flatpak: bool,
) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = match lv2_path {
        Some(p) if !p.is_empty() => std::env::split_paths(&p).collect(),
        _ => {
            let mut v = Vec::new();
            if let Some(home) = home {
                v.push(Path::new(&home).join(".lv2"));
            }
            for p in [
                "/usr/local/lib/lv2",
                "/usr/local/lib64/lv2",
                "/usr/lib/lv2",
                "/usr/lib64/lv2",
                "/usr/lib/x86_64-linux-gnu/lv2",
                "/usr/lib/aarch64-linux-gnu/lv2",
            ] {
                v.push(PathBuf::from(p));
            }
            v
        }
    };
    if flatpak {
        v.extend(FLATPAK_PATHS.iter().map(PathBuf::from));
    }
    v
}

/// Bundles at `path`: the directory itself when it is a bundle, otherwise its
/// immediate subdirectories that are bundles.
pub(crate) fn bundles(path: &Path) -> Vec<PathBuf> {
    if path.join("manifest.ttl").is_file() {
        return vec![path.to_path_buf()];
    }
    let mut out: Vec<PathBuf> = std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("manifest.ttl").is_file())
        .collect();
    out.sort();
    out
}

/// Describes every plugin in the bundles at `path` that this host can load.
/// Plugins needing features or port types we do not provide are left out.
pub(crate) fn scan(path: &Path) -> Vec<PluginDescriptor> {
    bundles(path)
        .iter()
        .flat_map(|bundle| scan_bundle(bundle))
        .collect()
}

fn scan_bundle(bundle: &Path) -> Vec<PluginDescriptor> {
    let world = livi::World::with_load_bundle(&bundle_uri(bundle));
    world
        .iter_plugins()
        .map(|p| describe(&world, &p, bundle))
        .collect()
}

pub(crate) fn describe(world: &livi::World, p: &livi::Plugin, bundle: &Path) -> PluginDescriptor {
    let raw = p.raw();
    let lilv = world.raw();
    let counts = p.port_counts();
    let has_midi_in = p
        .ports_with_type(livi::PortType::AtomSequenceInput)
        .next()
        .is_some();
    let typed_instrument = raw
        .value(&lilv.new_uri(RDF_TYPE))
        .iter()
        .any(|n| n.as_uri() == Some(INSTRUMENT_PLUGIN));
    let is_instrument = typed_instrument
        || p.is_instrument()
        || (has_midi_in && counts.audio_inputs == 0 && counts.audio_outputs > 0);
    let path = raw
        .bundle_uri()
        .as_uri()
        .and_then(uri_to_path)
        .unwrap_or_else(|| bundle.to_path_buf());
    PluginDescriptor {
        format: PluginFormatKind::Lv2,
        id: p.uri(),
        name: p.name(),
        vendor: vendor(lilv, raw),
        path,
        is_instrument,
        ports: (
            counts.audio_inputs.min(u16::MAX as usize) as u16,
            counts.audio_outputs.min(u16::MAX as usize) as u16,
        ),
    }
}

fn vendor(world: &livi::lilv::World, p: &livi::lilv::plugin::Plugin) -> String {
    if let Some(name) = p.author_name().and_then(|n| n.as_str().map(str::to_owned)) {
        return name;
    }
    // Plugins often only name their project's maintainer.
    let project = p.project();
    if let Some(project) = project {
        let maintainer = world.get(Some(&project), Some(&world.new_uri(DOAP_MAINTAINER)), None);
        if let Some(name) = maintainer
            .and_then(|m| world.get(Some(&m), Some(&world.new_uri(FOAF_NAME)), None))
            .and_then(|n| n.as_str().map(str::to_owned))
        {
            return name;
        }
        if let Some(name) = world
            .get(Some(&project), Some(&world.new_uri(DOAP_NAME)), None)
            .and_then(|n| n.as_str().map(str::to_owned))
        {
            return name;
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bundle_directory_is_its_own_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let b = dir.path().join("x.lv2");
        std::fs::create_dir(&b).unwrap();
        std::fs::write(b.join("manifest.ttl"), "").unwrap();
        std::fs::create_dir(dir.path().join("not-a-bundle")).unwrap();
        assert_eq!(bundles(&b), vec![b.clone()]);
        assert_eq!(bundles(dir.path()), vec![b]);
        assert!(bundles(&dir.path().join("missing")).is_empty());
    }

    #[test]
    fn flatpak_adds_its_plugin_directories() {
        let home = Some("/home/me".into());
        let native = candidate_paths(None, home.clone(), false);
        assert_eq!(native[0], Path::new("/home/me/.lv2"));
        assert!(!native.iter().any(|p| p.starts_with("/app")));

        let flatpak = candidate_paths(None, home, true);
        assert_eq!(&flatpak[..native.len()], &native[..]);
        assert_eq!(
            &flatpak[native.len()..],
            &[
                PathBuf::from("/app/extensions/Plugins/lv2"),
                PathBuf::from("/app/lib/lv2")
            ]
        );

        // LV2_PATH replaces the standard locations but not Flatpak's.
        let custom = candidate_paths(Some("/a:/b".into()), None, true);
        assert_eq!(
            custom,
            ["/a", "/b", "/app/extensions/Plugins/lv2", "/app/lib/lv2"].map(PathBuf::from)
        );
        assert_eq!(
            candidate_paths(Some("/a".into()), None, false),
            [PathBuf::from("/a")]
        );
    }
}
