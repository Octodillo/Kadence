//! Finding plugin bundles on disk.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use daw_core::plugin::PluginFormatKind;

/// Every bundle of `kind` under `roots`, each once. Roots keep their order, so earlier search
/// paths take precedence; each directory's entries are visited in sorted order. A root may
/// itself be a bundle.
/// The walk does not descend into bundles (VST3 and LV2 bundles are directories), goes
/// [`MAX_DEPTH`] directories deep and skips anything it cannot read.
///
/// Symbolic links are followed, but a bundle reached a second time (through a link, or because
/// one root is inside another) is left out: each bundle is reported under the first path that
/// led to it. A link to a bundle is that bundle whatever the link is called.
pub fn find_candidates(kind: &PluginFormatKind, roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut walk = Walk {
        // Every format's bundles are named after it.
        extension: kind.as_str(),
        walked: HashMap::new(),
        reported: HashSet::new(),
        found: Vec::new(),
    };
    for root in roots {
        walk.visit(root, 0);
    }
    walk.found
}

/// How many directories deep bundles are looked for, and how deep a bundle's own files are
/// looked at: enough for vendor/product/version layouts.
pub(crate) const MAX_DEPTH: usize = 8;

struct Walk {
    extension: &'static str,
    /// The directories walked so far, with links resolved, and the depth each was walked at.
    walked: HashMap<PathBuf, usize>,
    /// The bundles found so far, with links resolved.
    reported: HashSet<PathBuf>,
    found: Vec<PathBuf>,
}

impl Walk {
    fn names_bundle(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|e| e.eq_ignore_ascii_case(self.extension))
    }

    fn visit(&mut self, path: &Path, depth: usize) {
        // Most entries are neither: decide that before resolving anything.
        let named = self.names_bundle(path);
        if !named && !path.is_dir() {
            return;
        }
        // A path that does not resolve is missing or a broken link.
        let Ok(real) = path.canonicalize() else {
            return;
        };
        if named || self.names_bundle(&real) {
            if self.reported.insert(real) {
                self.found.push(path.to_owned());
            }
            return;
        }
        // A directory is walked once, which also ends link cycles; again only when it is
        // reached nearer a root, since the first walk may have stopped short of bundles
        // that this one is allowed to reach.
        if depth >= MAX_DEPTH || self.walked.get(&real).is_some_and(|&at| at <= depth) {
            return;
        }
        self.walked.insert(real, depth);
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
