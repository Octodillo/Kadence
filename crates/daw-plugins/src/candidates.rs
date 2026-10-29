//! Finding plugin bundles on disk.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use daw_core::plugin::PluginFormatKind;

/// The file or directory extension of one plugin bundle of `kind`.
pub fn bundle_extension(kind: &PluginFormatKind) -> &'static str {
    match kind {
        PluginFormatKind::Clap => "clap",
        PluginFormatKind::Lv2 => "lv2",
        PluginFormatKind::Vst3 => "vst3",
    }
}

/// Every bundle of `kind` under `roots`, without duplicates. Roots keep their order, so earlier
/// search paths take precedence; bundles within one root are sorted. A root may itself be a
/// bundle. The walk does not descend into bundles (VST3 and LV2 bundles are directories) and
/// skips anything it cannot read.
pub fn find_candidates(kind: &PluginFormatKind, roots: &[PathBuf]) -> Vec<PathBuf> {
    let ext = bundle_extension(kind);
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    for root in roots {
        let mut here = Vec::new();
        walk(root, ext, &mut here, 0);
        here.sort();
        found.extend(here.into_iter().filter(|b| seen.insert(b.clone())));
    }
    found
}

/// Deep enough for vendor/product/version layouts, shallow enough to stop symlink loops.
const MAX_DEPTH: usize = 8;

fn walk(path: &Path, ext: &str, found: &mut Vec<PathBuf>, depth: usize) {
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
        && path.exists()
    {
        found.push(path.to_owned());
        return;
    }
    if depth >= MAX_DEPTH || !path.is_dir() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    let mut children: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    children.sort();
    for child in children {
        walk(&child, ext, found, depth + 1);
    }
}
