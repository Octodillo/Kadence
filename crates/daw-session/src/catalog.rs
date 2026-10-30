//! The installed plugins, for a plugin browser.
//!
//! A [`PluginCatalog`] scans every format's folders, plus folders the user added, on a
//! background thread, so the UI stays responsive while plugins load in scanner child
//! processes. Results are cached by bundle (see [`daw_plugins::ScanCache`]), so only new or
//! changed bundles are loaded again. The UI polls the catalog on its timer: [`poll`] picks up
//! a finished scan, [`progress`] says how far a running one got.
//!
//! Hand the result to the session with
//! [`Session::set_known_plugins`](crate::Session::set_known_plugins), so its loader finds
//! plugins in the user's folders too and does not scan again.
//!
//! [`poll`]: PluginCatalog::poll
//! [`progress`]: PluginCatalog::progress

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;

use daw_core::plugin::{PluginDescriptor, PluginFormatKind};
use daw_plugins::{ScanCache, Scanner};

/// Every plugin format, in the order a browser lists them.
pub const FORMATS: [PluginFormatKind; 3] = [
    PluginFormatKind::Clap,
    PluginFormatKind::Vst3,
    PluginFormatKind::Lv2,
];

/// What a plugin is for, as the browser filters it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PluginKind {
    /// Plays notes.
    Instrument,
    /// Processes audio: everything that is not an instrument.
    Effect,
}

impl PluginKind {
    pub fn of(d: &PluginDescriptor) -> Self {
        if d.is_instrument {
            Self::Instrument
        } else {
            Self::Effect
        }
    }
}

/// Which plugins a browser shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    /// Words that must each appear in the name or vendor, ignoring case.
    pub text: String,
    pub format: Option<PluginFormatKind>,
    pub kind: Option<PluginKind>,
}

impl Filter {
    pub fn matches(&self, d: &PluginDescriptor) -> bool {
        if self.format.as_ref().is_some_and(|f| *f != d.format)
            || self.kind.is_some_and(|k| k != PluginKind::of(d))
        {
            return false;
        }
        let name = d.name.to_lowercase();
        let vendor = d.vendor.to_lowercase();
        self.text
            .to_lowercase()
            .split_whitespace()
            .all(|word| name.contains(word) || vendor.contains(word))
    }
}

/// How far a running scan got, in bundles.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanProgress {
    pub done: usize,
    /// 0 until the scan has found the bundles.
    pub total: usize,
}

/// A bundle that did not scan, and why (in English, often the plugin's own words).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanFailure {
    pub bundle: PathBuf,
    pub reason: String,
}

struct Outcome {
    plugins: Vec<PluginDescriptor>,
    failed: Vec<ScanFailure>,
}

struct Running {
    done: Arc<AtomicUsize>,
    total: Arc<AtomicUsize>,
    result: mpsc::Receiver<Outcome>,
}

/// The installed plugins and the scan that finds them. See the module docs.
pub struct PluginCatalog {
    scanner: Scanner,
    cache_file: Option<PathBuf>,
    extra_folders: Vec<PathBuf>,
    /// Replaces a format's own folders (tests, or a host with fixed folders).
    folders: HashMap<PluginFormatKind, Vec<PathBuf>>,
    plugins: Vec<PluginDescriptor>,
    failed: Vec<ScanFailure>,
    scanned: bool,
    running: Option<Running>,
    /// A scan asked for while one ran, and whether it retries failures.
    queued: Option<bool>,
    revision: u64,
}

impl PluginCatalog {
    /// A catalog that scans with `scanner` (a program answering `--scan`; see
    /// [`daw_plugins::run_scan_child`]). Nothing is scanned until [`scan`](Self::scan).
    pub fn new(scanner: Scanner) -> Self {
        Self {
            scanner,
            cache_file: None,
            extra_folders: Vec::new(),
            folders: HashMap::new(),
            plugins: Vec::new(),
            failed: Vec::new(),
            scanned: false,
            running: None,
            queued: None,
            revision: 0,
        }
    }

    /// Keeps scan results in `path` between runs.
    pub fn with_cache_file(mut self, path: PathBuf) -> Self {
        self.cache_file = Some(path);
        self
    }

    /// Scans only `folders` for `format` instead of its usual ones. The extra folders are
    /// still scanned too.
    pub fn with_folders(mut self, format: PluginFormatKind, folders: Vec<PathBuf>) -> Self {
        self.folders.insert(format, folders);
        self
    }

    /// Folders the user added, scanned for every format after the usual ones. Takes effect on
    /// the next scan.
    pub fn set_extra_folders(&mut self, folders: Vec<PathBuf>) {
        self.extra_folders = folders;
    }

    pub fn extra_folders(&self) -> &[PathBuf] {
        &self.extra_folders
    }

    /// Starts a scan on a background thread. Bundles scanned before and unchanged since are
    /// answered from the cache; with `retry_failed`, bundles that failed are loaded again.
    /// While a scan runs, another one is queued to start when it ends.
    pub fn scan(&mut self, retry_failed: bool) {
        if self.running.is_some() {
            let retry = self.queued.unwrap_or(false) || retry_failed;
            self.queued = Some(retry);
            return;
        }
        let done = Arc::new(AtomicUsize::new(0));
        let total = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = mpsc::channel();
        let job = Job {
            scanner: self.scanner.clone(),
            cache_file: self.cache_file.clone(),
            extra_folders: self.extra_folders.clone(),
            folders: self.folders.clone(),
            retry_failed,
        };
        let progress = (Arc::clone(&done), Arc::clone(&total));
        let spawned = std::thread::Builder::new()
            .name("plugin-scan".into())
            .spawn(move || {
                let _ = tx.send(job.run(&progress.0, &progress.1));
            });
        if spawned.is_err() {
            // Out of threads: nothing changes, and a later scan may do better.
            return;
        }
        self.running = Some(Running {
            done,
            total,
            result: rx,
        });
        self.revision += 1;
    }

    /// Picks up a finished scan. Returns true when the plugin list changed, or a scan started
    /// or ended: anything a browser shows.
    pub fn poll(&mut self) -> bool {
        let Some(running) = &self.running else {
            return false;
        };
        let outcome = match running.result.try_recv() {
            Ok(outcome) => outcome,
            Err(mpsc::TryRecvError::Empty) => return false,
            // The scan thread panicked: keep what was there.
            Err(mpsc::TryRecvError::Disconnected) => Outcome {
                plugins: self.plugins.clone(),
                failed: self.failed.clone(),
            },
        };
        self.running = None;
        self.plugins = outcome.plugins;
        self.failed = outcome.failed;
        self.scanned = true;
        self.revision += 1;
        if let Some(retry) = self.queued.take() {
            self.scan(retry);
        }
        true
    }

    /// The running scan's progress, or `None` when none runs.
    pub fn progress(&self) -> Option<ScanProgress> {
        let r = self.running.as_ref()?;
        Some(ScanProgress {
            done: r.done.load(Ordering::Relaxed),
            total: r.total.load(Ordering::Relaxed),
        })
    }

    pub fn is_scanning(&self) -> bool {
        self.running.is_some()
    }

    /// Whether a scan has finished, so an empty list means no plugins rather than not known
    /// yet.
    pub fn has_scanned(&self) -> bool {
        self.scanned
    }

    /// Every plugin found by the last scan, sorted by name.
    pub fn plugins(&self) -> &[PluginDescriptor] {
        &self.plugins
    }

    /// The plugins `filter` lets through, in [`plugins`](Self::plugins) order.
    pub fn filtered<'a>(
        &'a self,
        filter: &'a Filter,
    ) -> impl Iterator<Item = &'a PluginDescriptor> + 'a {
        self.plugins.iter().filter(|d| filter.matches(d))
    }

    pub fn find(&self, format: &PluginFormatKind, id: &str) -> Option<&PluginDescriptor> {
        self.plugins
            .iter()
            .find(|d| d.format == *format && d.id == id)
    }

    /// Bundles the last scan could not load.
    pub fn failed(&self) -> &[ScanFailure] {
        &self.failed
    }

    /// Changes whenever anything this catalog reports may have changed.
    pub fn revision(&self) -> u64 {
        self.revision
    }
}

/// One scan, as the background thread runs it.
struct Job {
    scanner: Scanner,
    cache_file: Option<PathBuf>,
    extra_folders: Vec<PathBuf>,
    folders: HashMap<PluginFormatKind, Vec<PathBuf>>,
    retry_failed: bool,
}

impl Job {
    fn run(self, done: &AtomicUsize, total: &AtomicUsize) -> Outcome {
        let mut registry = daw_plugins::default_registry();
        for format in FORMATS {
            let mut paths = match self.folders.get(&format) {
                Some(paths) => paths.clone(),
                None => registry.search_paths(&format),
            };
            paths.extend(self.extra_folders.iter().cloned());
            registry.set_search_paths(format, paths);
        }
        let mut cache = self
            .cache_file
            .as_deref()
            .map(ScanCache::load)
            .unwrap_or_default();
        if self.retry_failed {
            cache.forget_failures();
        }
        let report = registry.scan_with_progress(&self.scanner, &mut cache, &|d, t| {
            total.store(t, Ordering::Relaxed);
            done.store(d, Ordering::Relaxed);
        });
        if let Some(path) = &self.cache_file {
            // The cache only saves time; failing to write it is no reason to stop.
            let _ = cache.save(path);
        }
        Outcome {
            plugins: registry.plugins().to_vec(),
            failed: report
                .failed
                .into_iter()
                .map(|(bundle, reason)| ScanFailure { bundle, reason })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(
        name: &str,
        vendor: &str,
        format: PluginFormatKind,
        instrument: bool,
    ) -> PluginDescriptor {
        PluginDescriptor {
            format,
            id: name.to_lowercase(),
            name: name.into(),
            vendor: vendor.into(),
            path: PathBuf::new(),
            is_instrument: instrument,
            ports: (if instrument { 0 } else { 2 }, 2),
        }
    }

    #[test]
    fn filter_matches_words_format_and_kind() {
        let surge = plugin("Surge XT", "Surge Synth Team", PluginFormatKind::Clap, true);
        let eq = plugin("Para EQ", "x42", PluginFormatKind::Lv2, false);

        let all = Filter::default();
        assert!(all.matches(&surge) && all.matches(&eq));

        let words = Filter {
            text: "  synth XT ".into(),
            ..Default::default()
        };
        assert!(
            words.matches(&surge),
            "words match name and vendor, any case"
        );
        assert!(!words.matches(&eq));

        let lv2 = Filter {
            format: Some(PluginFormatKind::Lv2),
            ..Default::default()
        };
        assert!(lv2.matches(&eq) && !lv2.matches(&surge));

        let instruments = Filter {
            kind: Some(PluginKind::Instrument),
            ..Default::default()
        };
        assert!(instruments.matches(&surge) && !instruments.matches(&eq));
        let effects = Filter {
            kind: Some(PluginKind::Effect),
            ..Default::default()
        };
        assert!(effects.matches(&eq) && !effects.matches(&surge));
    }
}
