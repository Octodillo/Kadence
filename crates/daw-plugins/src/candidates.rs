//! Finding plugin bundles on disk.

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

/// Every bundle of `kind` under `roots`, sorted and without duplicates. A root may itself be a
/// bundle. The walk does not descend into bundles (VST3 and LV2 bundles are directories) and
/// skips anything it cannot read.
pub fn find_candidates(kind: &PluginFormatKind, roots: &[PathBuf]) -> Vec<PathBuf> {
    let ext = bundle_extension(kind);
    let mut found = Vec::new();
    for root in roots {
        walk(root, ext, &mut found, 0);
    }
    found.sort();
    found.dedup();
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
