//! Waveform peaks for drawing, loaded on a background thread so opening a
//! project with long files never blocks the UI.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;

use daw_media::PeakCache;

/// What the pool knows about one file.
#[derive(Clone, Debug)]
pub enum Peaks {
    Loading,
    Ready(Arc<PeakCache>),
    Failed,
}

/// Loads peak caches (building and saving them next to the audio if they
/// are missing) on its own thread.
pub struct PeakPool {
    known: HashMap<PathBuf, Peaks>,
    requests: Option<Sender<PathBuf>>,
    results: Receiver<(PathBuf, Option<PeakCache>)>,
    worker: Option<JoinHandle<()>>,
}

impl Default for PeakPool {
    fn default() -> Self {
        Self::new()
    }
}

impl PeakPool {
    pub fn new() -> Self {
        let (req_tx, req_rx) = mpsc::channel::<PathBuf>();
        let (res_tx, res_rx) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("kadence-peaks".into())
            .spawn(move || {
                for path in req_rx {
                    let cache = PeakCache::load_or_build(&path).ok();
                    if res_tx.send((path, cache)).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn peak loader");
        Self {
            known: HashMap::new(),
            requests: Some(req_tx),
            results: res_rx,
            worker: Some(worker),
        }
    }

    /// The peaks for `path`, starting a load the first time it is asked for.
    pub fn get(&mut self, path: &Path) -> Peaks {
        if let Some(p) = self.known.get(path) {
            return p.clone();
        }
        self.known.insert(path.to_path_buf(), Peaks::Loading);
        if let Some(tx) = &self.requests {
            let _ = tx.send(path.to_path_buf());
        }
        Peaks::Loading
    }

    /// Takes finished loads. Returns true if any arrived, meaning the view
    /// should be redrawn.
    pub fn poll(&mut self) -> bool {
        let mut any = false;
        while let Ok((path, cache)) = self.results.try_recv() {
            let p = match cache {
                Some(c) => Peaks::Ready(Arc::new(c)),
                None => Peaks::Failed,
            };
            self.known.insert(path, p);
            any = true;
        }
        any
    }

    /// Whether any request is still being worked on.
    pub fn busy(&self) -> bool {
        self.known.values().any(|p| matches!(p, Peaks::Loading))
    }

    /// Forgets a file so it is loaded again, e.g. after recording into it.
    pub fn invalidate(&mut self, path: &Path) {
        self.known.remove(path);
    }

    /// Waits for every pending load (for tests and screenshots).
    pub fn wait(&mut self) {
        while self.busy() {
            match self.results.recv() {
                Ok((path, cache)) => {
                    let p = match cache {
                        Some(c) => Peaks::Ready(Arc::new(c)),
                        None => Peaks::Failed,
                    };
                    self.known.insert(path, p);
                }
                Err(_) => break,
            }
        }
    }
}

impl Drop for PeakPool {
    fn drop(&mut self) {
        self.requests = None;
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}
