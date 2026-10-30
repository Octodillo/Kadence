//! Audio files for playback: which ones are loaded into memory and which
//! are streamed, and the cache of the ones in memory.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use daw_media::{AudioData, Decoder, DiskStreamer, MediaError, StreamerConfig, load_file};

use crate::player::Streams;

/// Files at most this long (at the engine rate) are decoded into memory.
pub const MEMORY_LIMIT_SECONDS: f64 = 60.0;

/// How a file is played.
#[derive(Clone)]
pub(crate) enum Loaded {
    /// Resampled to the engine rate. The `u32` is the file's own rate.
    Memory(Arc<AudioData>, u32),
    /// Streamed at the engine rate from a file at this rate.
    Stream(u32),
    Missing,
}

/// Decoded files and the disk streamer, owned by the session.
pub struct MediaPool {
    sample_rate: u32,
    root: Option<PathBuf>,
    loaded: HashMap<PathBuf, Loaded>,
    streamer: DiskStreamer,
    memory_limit: f64,
}

impl MediaPool {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            root: None,
            loaded: HashMap::new(),
            streamer: DiskStreamer::new(StreamerConfig {
                sample_rate,
                ..Default::default()
            }),
            memory_limit: MEMORY_LIMIT_SECONDS,
        }
    }

    /// The project bundle directory that relative clip paths are resolved
    /// against.
    pub fn set_root(&mut self, root: Option<PathBuf>) {
        if self.root != root {
            self.root = root;
            self.loaded.clear();
        }
    }

    /// Files longer than this many seconds are streamed instead of decoded
    /// into memory.
    pub fn set_memory_limit(&mut self, seconds: f64) {
        self.memory_limit = seconds;
        self.loaded.clear();
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    pub fn resolve(&self, source: &Path) -> PathBuf {
        match &self.root {
            Some(root) if source.is_relative() => root.join(source),
            _ => source.to_path_buf(),
        }
    }

    /// Drops cached audio no clip in `keep` uses any more.
    pub(crate) fn retain(&mut self, keep: &std::collections::HashSet<PathBuf>) {
        self.loaded.retain(|p, _| keep.contains(p));
    }

    pub(crate) fn get(&mut self, path: &Path) -> Loaded {
        if let Some(l) = self.loaded.get(path) {
            return l.clone();
        }
        let l = self.load(path).unwrap_or(Loaded::Missing);
        self.loaded.insert(path.to_path_buf(), l.clone());
        l
    }

    pub(crate) fn open_streams(&self, path: &Path) -> Result<Streams, MediaError> {
        Streams::open(&self.streamer, path)
    }

    fn load(&self, path: &Path) -> Result<Loaded, MediaError> {
        let info = Decoder::open(path)?.info().clone();
        let seconds = info
            .frames
            .map(|f| f as f64 / f64::from(info.sample_rate.max(1)));
        match seconds {
            Some(s) if s <= self.memory_limit => {
                let data = load_file(path, self.sample_rate)?;
                Ok(Loaded::Memory(Arc::new(data), info.sample_rate))
            }
            _ => Ok(Loaded::Stream(info.sample_rate)),
        }
    }
}
