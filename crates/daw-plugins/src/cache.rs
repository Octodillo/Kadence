//! The JSON scan cache.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use daw_core::plugin::{PluginDescriptor, PluginFormatKind};
use serde::{Deserialize, Serialize};

/// The scan result for one bundle.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CacheEntry {
    pub format: PluginFormatKind,
    /// The bundle's modification time, in nanoseconds since the Unix epoch.
    pub mtime_ns: u128,
    /// The plugins found, or why the scan failed.
    pub result: Result<Vec<PluginDescriptor>, String>,
}

/// Scan results keyed by bundle path.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScanCache {
    version: u32,
    entries: BTreeMap<PathBuf, CacheEntry>,
}

const VERSION: u32 = 1;

impl ScanCache {
    pub fn new() -> Self {
        Self {
            version: VERSION,
            entries: BTreeMap::new(),
        }
    }

    /// Reads a cache file. A missing, unreadable or outdated file gives an empty cache, since the
    /// cache can always be rebuilt by scanning.
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|data| serde_json::from_slice::<Self>(&data).ok())
            .filter(|c| c.version == VERSION)
            .unwrap_or_default()
    }

    /// Writes the cache atomically: to a temporary file, then renamed over `path`.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, path)
    }

    /// The cached entry for `bundle`, if it is still current.
    pub fn get(&self, bundle: &Path) -> Option<&CacheEntry> {
        let entry = self.entries.get(bundle)?;
        (Some(entry.mtime_ns) == modified_time(bundle)).then_some(entry)
    }

    pub fn insert(&mut self, bundle: PathBuf, entry: CacheEntry) {
        self.entries.insert(bundle, entry);
    }

    /// Drops cached failures so the next scan tries those bundles again, for a "rescan failed
    /// plugins" action. Bundles that gave no plugins count as failures: formats report a bundle
    /// that would not load (a missing system library, say) as an empty list.
    pub fn forget_failures(&mut self) {
        self.entries
            .retain(|_, e| e.result.as_ref().is_ok_and(|found| !found.is_empty()));
    }

    /// Drops entries for bundles that no longer exist.
    pub fn prune(&mut self) {
        self.entries.retain(|path, _| path.exists());
    }

    pub fn entries(&self) -> impl Iterator<Item = (&Path, &CacheEntry)> {
        self.entries.iter().map(|(p, e)| (p.as_path(), e))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for ScanCache {
    fn default() -> Self {
        Self::new()
    }
}

/// The latest modification time of `path` or, for a bundle directory, of anything inside it.
pub fn modified_time(path: &Path) -> Option<u128> {
    fn mtime(path: &Path) -> Option<u128> {
        let modified = std::fs::metadata(path).ok()?.modified().ok()?;
        Some(
            modified
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        )
    }
    fn newest(path: &Path, depth: usize) -> Option<u128> {
        let mut best = mtime(path)?;
        if depth < 8 && path.is_dir() {
            for entry in std::fs::read_dir(path).ok()?.flatten() {
                if let Some(t) = newest(&entry.path(), depth + 1) {
                    best = best.max(t);
                }
            }
        }
        Some(best)
    }
    newest(path, 0)
}
