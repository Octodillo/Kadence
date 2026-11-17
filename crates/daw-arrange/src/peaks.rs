//! Waveform peaks for drawing, loaded on a background thread so opening a
//! project with long files never blocks the UI.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use daw_media::PeakCache;

/// What the pool knows about one file.
#[derive(Clone, Debug)]
pub enum Peaks {
    Loading,
    Ready(Arc<PeakCache>),
    Failed,
}

/// A file's size and modification time, `None` if it can't be looked at.
/// The file is loaded again when this changes.
type Stamp = Option<(u64, Option<SystemTime>)>;

fn stamp(path: &Path) -> Stamp {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()))
}

struct Entry {
    peaks: Peaks,
    /// The file as it was when `peaks` was loaded.
    stamp: Stamp,
    /// When the worker last looked at the file, `None` while it has been
    /// asked to.
    checked: Option<Instant>,
}

/// Asks the worker to load `path`, unless `seen` is still its stamp.
struct Request {
    path: PathBuf,
    seen: Option<Stamp>,
}

/// The worker's answer: the file's stamp and what loading it gave, or
/// `None` if the file has not changed.
type Reply = (PathBuf, Option<(Stamp, Option<PeakCache>)>);

/// Loads peak caches (building and saving them next to the audio if they
/// are missing) on its own thread.
pub struct PeakPool {
    /// How long a file is left alone before [`get`](Self::get) has it
    /// looked at again. That is how a file that was missing, was still
    /// being copied or has been replaced gets its waveform. Only files
    /// that are asked for (drawn) are looked at.
    pub recheck_after: Duration,
    known: HashMap<PathBuf, Entry>,
    requests: Option<Sender<Request>>,
    results: Receiver<Reply>,
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
        Self::with_loader(|path| PeakCache::load_or_build(path).ok())
    }

    fn with_loader(load: impl Fn(&Path) -> Option<PeakCache> + Send + 'static) -> Self {
        let (req_tx, req_rx) = mpsc::channel::<Request>();
        let (res_tx, res_rx) = mpsc::channel::<Reply>();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = std::thread::Builder::new()
            .name("kadence-peaks".into())
            .spawn(move || {
                for Request { path, seen } in req_rx {
                    if stopped.load(Ordering::Relaxed) {
                        break;
                    }
                    // Stamped before loading: a file that changes during
                    // the load is then loaded again at the next look.
                    let now = stamp(&path);
                    let loaded = (seen != Some(now)).then(|| (now, load(&path)));
                    if res_tx.send((path, loaded)).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn peak loader");
        Self {
            recheck_after: Duration::from_secs(2),
            known: HashMap::new(),
            requests: Some(req_tx),
            results: res_rx,
            worker: Some(worker),
            stop,
        }
    }

    /// The peaks for `path`, starting a load the first time it is asked
    /// for. Later the file is looked at every
    /// [`recheck_after`](Self::recheck_after) and loaded again if it
    /// changed; until that load is done the old answer stands.
    pub fn get(&mut self, path: &Path) -> Peaks {
        let seen = match self.known.get_mut(path) {
            Some(e) => {
                if e.checked.is_none_or(|at| at.elapsed() < self.recheck_after) {
                    return e.peaks.clone();
                }
                e.checked = None;
                Some(e.stamp)
            }
            None => {
                let first = Entry {
                    peaks: Peaks::Loading,
                    stamp: None,
                    checked: None,
                };
                self.known.insert(path.to_path_buf(), first);
                None
            }
        };
        if let Some(tx) = &self.requests {
            let path = path.to_path_buf();
            let _ = tx.send(Request { path, seen });
        }
        self.known[path].peaks.clone()
    }

    /// Notes the worker's answer. Returns whether what [`get`](Self::get)
    /// says about the file changed (a load that failed again changes
    /// nothing).
    fn store(&mut self, (path, loaded): Reply) -> bool {
        let Some(e) = self.known.get_mut(&path) else {
            return false;
        };
        e.checked = Some(Instant::now());
        let Some((stamp, cache)) = loaded else {
            return false;
        };
        e.stamp = stamp;
        let was_failed = matches!(e.peaks, Peaks::Failed);
        e.peaks = match cache {
            Some(c) => Peaks::Ready(Arc::new(c)),
            None => Peaks::Failed,
        };
        !(was_failed && matches!(e.peaks, Peaks::Failed))
    }

    /// Takes finished loads. Returns true if any changed what there is to
    /// draw, meaning the view should be redrawn.
    pub fn poll(&mut self) -> bool {
        let mut any = false;
        while let Ok(reply) = self.results.try_recv() {
            any |= self.store(reply);
        }
        any
    }

    /// Whether any first load is still being worked on.
    pub fn busy(&self) -> bool {
        self.known
            .values()
            .any(|e| matches!(e.peaks, Peaks::Loading))
    }

    /// Waits for every pending first load (for tests and screenshots).
    pub fn wait(&mut self) {
        while self.busy() {
            match self.results.recv() {
                Ok(reply) => {
                    self.store(reply);
                }
                Err(_) => break,
            }
        }
    }

    /// Has the worker leave what is still queued; the load in progress
    /// finishes.
    fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.requests = None;
    }
}

impl Drop for PeakPool {
    fn drop(&mut self) {
        // Quitting doesn't wait for every queued file to be decoded.
        self.stop();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use daw_media::{AudioData, WavFormat, write_wav};

    use super::*;

    fn tone(path: &Path, frames: usize) {
        let data = AudioData {
            sample_rate: 48_000,
            channels: vec![(0..frames).map(|i| (i as f32 * 0.05).sin()).collect()],
        };
        write_wav(path, &data, WavFormat::Float32).unwrap();
    }

    /// Has the file looked at again and waits for the answer. Returns
    /// whether that changed what there is to draw.
    fn recheck(pool: &mut PeakPool, path: &Path) -> bool {
        pool.recheck_after = Duration::ZERO;
        pool.get(path);
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut changed = false;
        while pool.known[path].checked.is_none() {
            assert!(Instant::now() < deadline, "the worker never answered");
            changed |= pool.poll();
            std::thread::yield_now();
        }
        changed
    }

    fn frames(pool: &mut PeakPool, path: &Path) -> Option<u64> {
        pool.recheck_after = Duration::MAX;
        match pool.get(path) {
            Peaks::Ready(cache) => Some(cache.frames),
            _ => None,
        }
    }

    /// Issue #204 item 9.
    #[test]
    fn dropping_the_pool_leaves_queued_loads() {
        let loads = Arc::new(AtomicUsize::new(0));
        let (started_tx, started) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let counted = loads.clone();
        let mut pool = PeakPool::with_loader(move |_| {
            counted.fetch_add(1, Ordering::Relaxed);
            let _ = started_tx.send(());
            let _ = released.recv();
            None
        });
        for i in 0..24 {
            pool.get(Path::new(&format!("{i}.wav")));
        }
        // The first load is under way when the pool is told to stop.
        started.recv().unwrap();
        pool.stop();
        drop(release);
        drop(pool);
        assert_eq!(loads.load(Ordering::Relaxed), 1);
    }

    /// Issue #204 item 11: a file that was missing is loaded once it is back.
    #[test]
    fn a_failed_load_is_tried_again() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("late.wav");
        let mut pool = PeakPool::new();
        assert!(matches!(pool.get(&file), Peaks::Loading));
        pool.wait();
        assert!(matches!(pool.get(&file), Peaks::Failed));
        // Still missing: nothing new to draw.
        assert!(!recheck(&mut pool, &file));

        tone(&file, 4_800);
        assert!(recheck(&mut pool, &file));
        assert_eq!(frames(&mut pool, &file), Some(4_800));
    }

    /// A file that is still being copied when it is loaded gets the rest of
    /// its waveform once the copy is done.
    #[test]
    fn a_file_that_changed_is_loaded_again() {
        let dir = tempfile::tempdir().unwrap();
        let (file, whole) = (dir.path().join("copy.wav"), dir.path().join("whole.wav"));
        tone(&whole, 48_000);
        let bytes = std::fs::read(&whole).unwrap();
        std::fs::write(&file, &bytes[..bytes.len() / 4]).unwrap();
        let mut pool = PeakPool::new();
        pool.get(&file);
        pool.wait();
        let part = frames(&mut pool, &file).expect("a cut-off file still loads");
        assert!(part < 48_000);
        // Unchanged: looked at, not loaded again.
        assert!(!recheck(&mut pool, &file));

        std::fs::write(&file, &bytes).unwrap();
        assert!(recheck(&mut pool, &file));
        assert_eq!(frames(&mut pool, &file), Some(48_000));

        // Gone again: its waveform goes too.
        std::fs::remove_file(&file).unwrap();
        assert!(recheck(&mut pool, &file));
        assert_eq!(frames(&mut pool, &file), None);
    }
}
