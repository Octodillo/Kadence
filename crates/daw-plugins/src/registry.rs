//! The registry of formats and known plugins.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use daw_core::plugin::{
    BundleScan, PluginDescriptor, PluginError, PluginFormat, PluginFormatKind, PluginInstance,
};

use crate::cache::{CacheEntry, ScanCache, modified_time};
use crate::candidates::find_candidates;
use crate::scanner::Scanner;

/// What one bundle's scan found, or why it failed.
type BundleResult = Result<BundleScan, String>;

/// What a [`PluginRegistry::scan`] did.
#[derive(Debug, Default)]
pub struct ScanReport {
    /// Bundles loaded in a scanner this time.
    pub scanned: usize,
    /// Bundles answered from the cache.
    pub cached: usize,
    /// Bundles whose scan failed, now or in a cached earlier attempt, with the reason. A bundle
    /// that loaded but skipped some of its plugins has one row per skipped plugin.
    pub failed: Vec<(PathBuf, String)>,
}

/// Plugin formats and the plugins they have found.
#[derive(Default)]
pub struct PluginRegistry {
    formats: Vec<Box<dyn PluginFormat>>,
    search_paths: HashMap<PluginFormatKind, Vec<PathBuf>>,
    plugins: Vec<PluginDescriptor>,
}

/// A registry with every format this build was compiled with.
pub fn default_registry() -> PluginRegistry {
    #[allow(unused_mut)]
    let mut registry = PluginRegistry::new();
    #[cfg(feature = "clap")]
    registry.register(daw_plugins_clap::format());
    #[cfg(feature = "lv2")]
    registry.register(daw_plugins_lv2::format());
    #[cfg(feature = "vst3")]
    registry.register(daw_plugins_vst3::format());
    registry
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a format, replacing any earlier one of the same kind.
    pub fn register(&mut self, format: Box<dyn PluginFormat>) {
        let kind = format.kind();
        self.formats.retain(|f| f.kind() != kind);
        self.formats.push(format);
    }

    pub fn format(&self, kind: &PluginFormatKind) -> Option<&dyn PluginFormat> {
        self.formats
            .iter()
            .find(|f| f.kind() == *kind)
            .map(|f| f.as_ref())
    }

    pub fn formats(&self) -> impl Iterator<Item = &dyn PluginFormat> {
        self.formats.iter().map(|f| f.as_ref())
    }

    /// Replaces a format's default search paths, for user settings and tests.
    pub fn set_search_paths(&mut self, kind: PluginFormatKind, paths: Vec<PathBuf>) {
        self.search_paths.insert(kind, paths);
    }

    pub fn search_paths(&self, kind: &PluginFormatKind) -> Vec<PathBuf> {
        match self.search_paths.get(kind) {
            Some(paths) => paths.clone(),
            None => self
                .format(kind)
                .map(|f| f.default_search_paths())
                .unwrap_or_default(),
        }
    }

    /// Every plugin found by the last scan, sorted by name.
    pub fn plugins(&self) -> &[PluginDescriptor] {
        &self.plugins
    }

    /// Replaces the plugin list with one found elsewhere, such as a scan on another thread.
    pub fn set_plugins(&mut self, plugins: Vec<PluginDescriptor>) {
        self.plugins = plugins;
    }

    pub fn find(&self, kind: &PluginFormatKind, id: &str) -> Option<&PluginDescriptor> {
        self.plugins
            .iter()
            .find(|p| p.format == *kind && p.id == id)
    }

    /// Lets every format clean up before the process exits. Call it once, after dropping
    /// every instance.
    pub fn shutdown(&self) {
        for format in &self.formats {
            format.shutdown();
        }
    }

    pub fn instantiate(
        &self,
        d: &PluginDescriptor,
        sample_rate: f64,
        max_frames: u32,
    ) -> Result<Box<dyn PluginInstance>, PluginError> {
        self.format(&d.format)
            .ok_or_else(|| PluginError::Load(format!("no {:?} support in this build", d.format)))?
            .instantiate(d, sample_rate, max_frames)
    }

    /// Finds every bundle of every format and rebuilds the plugin list. Bundles whose cache entry
    /// is current are not loaded; the rest are scanned in child processes by `scanner`, several
    /// at a time, and their results stored in `cache`.
    pub fn scan(&mut self, scanner: &Scanner, cache: &mut ScanCache) -> ScanReport {
        self.scan_with_progress(scanner, cache, &|_, _| {})
    }

    /// [`scan`](Self::scan), calling `progress(done, total)` with the number of bundles finished
    /// out of all found: once the bundles are known, then after each scanned one. Called from
    /// the scanning threads.
    pub fn scan_with_progress(
        &mut self,
        scanner: &Scanner,
        cache: &mut ScanCache,
        progress: &(dyn Fn(usize, usize) + Sync),
    ) -> ScanReport {
        let mut report = ScanReport::default();
        // One slot per bundle, in search-path order, filled from the cache or a scan.
        let mut slots: Vec<(PathBuf, Option<BundleResult>)> = Vec::new();
        let mut jobs: Vec<Job> = Vec::new();

        for format in &self.formats {
            let kind = format.kind();
            for bundle in find_candidates(&kind, &self.search_paths(&kind)) {
                let mtime_ns = modified_time(&bundle);
                let cached = mtime_ns
                    .and_then(|at| cache.get(&bundle, at))
                    .filter(|entry| entry.format == kind);
                match cached {
                    Some(entry) => {
                        report.cached += 1;
                        slots.push((bundle, Some(entry.result.clone())));
                    }
                    None => {
                        jobs.push(Job {
                            kind: kind.clone(),
                            bundle: bundle.clone(),
                            mtime_ns,
                            slot: slots.len(),
                        });
                        slots.push((bundle, None));
                    }
                }
            }
        }

        let total = slots.len();
        progress(report.cached, total);
        let done = AtomicUsize::new(report.cached);
        let results = scan_parallel(scanner, &jobs, &|| {
            progress(done.fetch_add(1, Ordering::Relaxed) + 1, total);
        });
        for (job, result) in jobs.into_iter().zip(results) {
            report.scanned += 1;
            let cacheable = result
                .as_ref()
                .map_or_else(|e| e.is_plugin_fault(), |_| true);
            let result = result.map_err(|e| e.to_string());
            // A bundle replaced while it was scanned gave the result of neither version for
            // certain, so it is left out of the cache and scanned again next time.
            if cacheable
                && let Some(mtime_ns) = job.mtime_ns
                && modified_time(&job.bundle) == Some(mtime_ns)
            {
                cache.insert(
                    job.bundle,
                    CacheEntry {
                        format: job.kind,
                        mtime_ns,
                        result: result.clone(),
                    },
                );
            }
            slots[job.slot].1 = Some(result);
        }
        cache.prune();

        let mut plugins = Vec::new();
        for (bundle, result) in slots {
            match result.expect("every slot is filled") {
                Ok(scan) => {
                    plugins.extend(scan.plugins);
                    let skipped = scan.skipped.into_iter();
                    report
                        .failed
                        .extend(skipped.map(|why| (bundle.clone(), why)));
                }
                Err(e) => report.failed.push((bundle, e)),
            }
        }
        // The same plugin can sit in several search paths; the first path wins.
        let mut seen = HashSet::new();
        plugins.retain(|p| seen.insert((p.format.clone(), p.id.clone())));
        plugins.sort_by_cached_key(|p| (p.name.to_lowercase(), p.id.clone()));
        self.plugins = plugins;
        report
    }
}

/// One bundle to scan.
struct Job {
    kind: PluginFormatKind,
    bundle: PathBuf,
    /// The bundle's modification time when the scan was planned.
    mtime_ns: Option<u128>,
    /// The slot the result fills.
    slot: usize,
}

fn scan_parallel(
    scanner: &Scanner,
    todo: &[Job],
    finished: &(dyn Fn() + Sync),
) -> Vec<Result<BundleScan, crate::ScanError>> {
    let next = Mutex::new(0usize);
    let results: Vec<Mutex<Option<_>>> = todo.iter().map(|_| Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..scanner.jobs().min(todo.len()) {
            s.spawn(|| {
                loop {
                    let i = {
                        let mut next = next.lock().unwrap();
                        let i = *next;
                        *next += 1;
                        i
                    };
                    let Some(job) = todo.get(i) else {
                        break;
                    };
                    let result = scanner.scan(&job.kind, &job.bundle);
                    *results[i].lock().unwrap() = Some(result);
                    finished();
                }
            });
        }
    });
    results
        .into_iter()
        .map(|r| r.into_inner().unwrap().expect("every bundle was scanned"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU32;

    struct Counting(PluginFormatKind, Arc<AtomicU32>);

    impl PluginFormat for Counting {
        fn kind(&self) -> PluginFormatKind {
            self.0.clone()
        }
        fn scan(&self, _path: &Path) -> Result<BundleScan, PluginError> {
            Ok(BundleScan::default())
        }
        fn shutdown(&self) {
            self.1.fetch_add(1, Ordering::Relaxed);
        }
        fn instantiate(
            &self,
            _d: &PluginDescriptor,
            _sample_rate: f64,
            _max_frames: u32,
        ) -> Result<Box<dyn PluginInstance>, PluginError> {
            Err(PluginError::Load("test format".into()))
        }
    }

    #[test]
    fn shutdown_reaches_every_format() {
        let calls = Arc::new(AtomicU32::new(0));
        let mut registry = PluginRegistry::new();
        registry.register(Box::new(Counting(PluginFormatKind::Vst3, calls.clone())));
        registry.register(Box::new(Counting(PluginFormatKind::Clap, calls.clone())));
        registry.shutdown();
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }
}
