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

/// Every bundle of `kind` under `roots`, each once. Roots keep their order, so earlier search
/// paths take precedence; each directory's entries are visited in sorted order. A root may
/// itself be a bundle.
/// The walk does not descend into bundles (VST3 and LV2 bundles are directories) and skips
/// anything it cannot read.
///
/// Symbolic links are followed, but a bundle or directory reached a second time (through a
/// link, or because one root is inside another) is left out: each bundle is reported under the
/// first path that led to it.
pub fn find_candidates(kind: &PluginFormatKind, roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut walk = Walk {
        extension: bundle_extension(kind),
        visited: HashSet::new(),
        found: Vec::new(),
    };
    for root in roots {
        walk.visit(root, 0);
    }
    walk.found
}

/// Deep enough for vendor/product/version layouts.
const MAX_DEPTH: usize = 8;

struct Walk {
    extension: &'static str,
    /// The bundles and directories seen so far, with links resolved.
    visited: HashSet<PathBuf>,
    found: Vec<PathBuf>,
}

impl Walk {
    fn visit(&mut self, path: &Path, depth: usize) {
        // A path that does not resolve is missing or a broken link.
        let Ok(real) = path.canonicalize() else {
            return;
        };
        let is_bundle = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case(self.extension));
        if !is_bundle && (depth >= MAX_DEPTH || !real.is_dir()) {
            return;
        }
        if !self.visited.insert(real) {
            return;
        }
        if is_bundle {
            self.found.push(path.to_owned());
            return;
        }
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        let mut children: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        children.sort();
        for child in children {
            self.visit(&child, depth + 1);
        }
    }
}
