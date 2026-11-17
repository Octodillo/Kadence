//! Waveform peaks for drawing, loaded on a background thread so opening a
//! project with long files never blocks the UI.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

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
    /// How long after a load failed [`get`](Self::get) tries it again, so a
    /// file that was missing gets its waveform once it is back.
    pub retry_after: Duration,
    known: HashMap<PathBuf, Peaks>,
    /// When each [`Peaks::Failed`] load failed, or `None` while it is being
    /// tried again.
    failed: HashMap<PathBuf, Option<Instant>>,
    requests: Option<Sender<PathBuf>>,
    results: Receiver<(PathBuf, Option<PeakCache>)>,
    worker: Option<JoinHandle<()>>,
    /// Tells the worker to leave the requests still queued.
    stop: Arc<AtomicBool>,
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
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = std::thread::Builder::new()
            .name("kadence-peaks".into())
            .spawn(move || {
                for path in req_rx {
                    if stopped.load(Ordering::Relaxed) {
                        break;
                    }
                    let cache = PeakCache::load_or_build(&path).ok();
                    if res_tx.send((path, cache)).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn peak loader");
        Self {
            retry_after: Duration::from_secs(2),
            known: HashMap::new(),
            failed: HashMap::new(),
            requests: Some(req_tx),
            results: res_rx,
            worker: Some(worker),
            stop,
        }
    }

    /// The peaks for `path`, starting a load the first time it is asked for
    /// and again every [`retry_after`](Self::retry_after) while it fails.
    pub fn get(&mut self, path: &Path) -> Peaks {
        match self.known.get(path) {
            Some(Peaks::Failed) => {
                if let Some(at) = self.failed.get_mut(path)
                    && at.is_some_and(|t| t.elapsed() >= self.retry_after)
                {
                    *at = None;
                    Self::request(&self.requests, path);
                }
                Peaks::Failed
            }
            Some(p) => p.clone(),
            None => {
                self.known.insert(path.to_path_buf(), Peaks::Loading);
                Self::request(&self.requests, path);
                Peaks::Loading
            }
        }
    }

    fn request(requests: &Option<Sender<PathBuf>>, path: &Path) {
        if let Some(tx) = requests {
            let _ = tx.send(path.to_path_buf());
        }
    }

    /// Notes a finished load. Returns whether what [`get`](Self::get) says
    /// about the file changed (a load that failed again changes nothing).
    fn store(&mut self, path: PathBuf, cache: Option<PeakCache>) -> bool {
        match cache {
            Some(c) => {
                self.failed.remove(&path);
                self.known.insert(path, Peaks::Ready(Arc::new(c)));
                true
            }
            None => {
                let again = self
                    .failed
                    .insert(path.clone(), Some(Instant::now()))
                    .is_some();
                self.known.insert(path, Peaks::Failed);
                !again
            }
        }
    }

    /// Takes finished loads. Returns true if any changed what there is to
    /// draw, meaning the view should be redrawn.
    pub fn poll(&mut self) -> bool {
        let mut any = false;
        while let Ok((path, cache)) = self.results.try_recv() {
            any |= self.store(path, cache);
        }
        any
    }

    /// Whether any first load is still being worked on.
    pub fn busy(&self) -> bool {
        self.known.values().any(|p| matches!(p, Peaks::Loading))
    }

    /// Waits for every pending first load (for tests and screenshots).
    pub fn wait(&mut self) {
        while self.busy() {
            match self.results.recv() {
                Ok((path, cache)) => {
                    self.store(path, cache);
                }
                Err(_) => break,
            }
        }
    }
}

impl Drop for PeakPool {
    fn drop(&mut self) {
        // The load in progress finishes; the queued ones are left, so
        // quitting doesn't wait for every file to be decoded.
        self.stop.store(true, Ordering::Relaxed);
        self.requests = None;
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use daw_media::{AudioData, WavFormat, write_wav};

    use super::*;

    fn tone(path: &Path, frames: usize) {
        let data = AudioData {
            sample_rate: 48_000,
            channels: vec![(0..frames).map(|i| (i as f32 * 0.05).sin()).collect()],
        };
        write_wav(path, &data, WavFormat::Float32).unwrap();
    }

    /// Issue #204 item 9.
    #[test]
    fn dropping_the_pool_leaves_queued_loads() {
        let dir = tempfile::tempdir().unwrap();
        let files: Vec<PathBuf> = (0..24)
            .map(|i| dir.path().join(format!("{i}.wav")))
            .collect();
        for f in &files {
            tone(f, 480_000);
        }
        let mut pool = PeakPool::new();
        for f in &files {
            pool.get(f);
        }
        drop(pool);
        let built = files
            .iter()
            .filter(|f| PeakCache::path_for(f).exists())
            .count();
        assert!(
            built < files.len() / 2,
            "{built} of 24 loads ran before exit"
        );
    }

    /// Issue #204 item 11: a file that was missing is loaded once it is back.
    #[test]
    fn a_failed_load_is_tried_again() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("late.wav");
        let mut pool = PeakPool::new();
        pool.retry_after = Duration::ZERO;
        assert!(matches!(pool.get(&file), Peaks::Loading));
        pool.wait();
        assert!(matches!(pool.get(&file), Peaks::Failed));
        // Failing again is nothing new to draw.
        while pool.failed[&file].is_none() {
            assert!(!pool.poll());
            std::thread::yield_now();
        }

        tone(&file, 4_800);
        assert!(matches!(pool.get(&file), Peaks::Failed));
        while !pool.poll() {
            std::thread::yield_now();
        }
        assert!(matches!(pool.get(&file), Peaks::Ready(_)));
    }
}
