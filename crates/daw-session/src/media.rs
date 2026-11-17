//! Audio files for playback: which ones are loaded into memory and which
//! are streamed, and the cache of the ones in memory.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use daw_media::{
    AudioData, AudioFileInfo, Decoder, DiskStreamer, MediaError, StreamerConfig, load_file_within,
};

use crate::player::Streams;

/// Files at most this long (at the engine rate) are decoded into memory.
pub const MEMORY_LIMIT_SECONDS: f64 = 60.0;

/// The most samples a second of a file in memory is counted as: 8 channels
/// at 192 kHz.
const NOMINAL_SAMPLES_PER_SECOND: u64 = 192_000 * 8;

/// What tells a file from an earlier version of itself: its modification
/// time and length.
type Stamp = (std::time::SystemTime, u64);

fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

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
    /// Files read so far, each with its [`stamp`] at the time: a file that
    /// changed since is read again. Files that could not be read are not
    /// kept, so each look tries them again.
    loaded: HashMap<PathBuf, (Loaded, Stamp)>,
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
        // The cache is by resolved path, so it holds for any root.
        self.root = root;
    }

    /// Forgets every file read so far.
    pub(crate) fn clear(&mut self) {
        self.loaded.clear();
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
        let Some(now) = stamp(path) else {
            self.loaded.remove(path);
            return Loaded::Missing;
        };
        if let Some((loaded, then)) = self.loaded.get(path)
            && *then == now
        {
            return loaded.clone();
        }
        match self.load(path) {
            Ok(loaded) => {
                self.loaded
                    .insert(path.to_path_buf(), (loaded.clone(), now));
                loaded
            }
            Err(_) => {
                self.loaded.remove(path);
                Loaded::Missing
            }
        }
    }

    pub(crate) fn open_streams(&self, path: &Path) -> Result<Streams, MediaError> {
        Streams::open(&self.streamer, path)
    }

    fn load(&self, path: &Path) -> Result<Loaded, MediaError> {
        let info = Decoder::open(path)?.info().clone();
        let seconds = info
            .frames
            .map(|f| f as f64 / f64::from(info.sample_rate.max(1)));
        let in_memory = match seconds {
            // The header's length decides whether to try, but the decoder
            // holds to the limit itself: a file can be longer than it says.
            Some(s) if s <= self.memory_limit => {
                load_file_within(path, self.sample_rate, self.max_samples(&info))?
            }
            _ => None,
        };
        Ok(match in_memory {
            Some(data) => Loaded::Memory(Arc::new(data), info.sample_rate),
            None => Loaded::Stream(info.sample_rate),
        })
    }

    /// The memory limit in samples of the file `info` is of. The file picks
    /// its own rate and channel count, so a second of it counts for no more
    /// than [`NOMINAL_SAMPLES_PER_SECOND`]: past that it holds fewer seconds.
    fn max_samples(&self, info: &AudioFileInfo) -> u64 {
        let per_second =
            (u64::from(info.sample_rate) * info.channels as u64).min(NOMINAL_SAMPLES_PER_SECOND);
        // Saturates: no limit is `u64::MAX`, and NaN is none in memory.
        (self.memory_limit * per_second as f64).ceil() as u64
    }
}

#[cfg(test)]
mod tests {
    use daw_media::{FlacBits, write_flac};

    use super::*;

    /// A second of mono FLAC at 48 kHz. With `honest` unset, its header says
    /// it holds 10 frames.
    fn flac(dir: &Path, honest: bool) -> PathBuf {
        let path = dir.join(format!("second-{honest}.flac"));
        write_flac(&path, &AudioData::silence(48_000, 1, 48_000), FlacBits::B16).unwrap();
        if !honest {
            let mut bytes = std::fs::read(&path).unwrap();
            // The frame count is the low 36 bits of STREAMINFO's bytes
            // 13..18, after "fLaC" and a 4-byte block header.
            bytes[21] &= 0xf0;
            bytes[22..26].copy_from_slice(&10u32.to_be_bytes());
            std::fs::write(&path, bytes).unwrap();
        }
        path
    }

    #[test]
    fn a_file_longer_than_its_header_says_is_streamed_not_held() {
        let dir = tempfile::tempdir().unwrap();
        let mut pool = MediaPool::new(48_000);
        pool.set_memory_limit(0.5);
        assert!(matches!(
            pool.get(&flac(dir.path(), false)),
            Loaded::Stream(48_000)
        ));
        assert!(matches!(
            pool.get(&flac(dir.path(), true)),
            Loaded::Stream(48_000)
        ));
        pool.set_memory_limit(1.0);
        for honest in [false, true] {
            let Loaded::Memory(data, 48_000) = pool.get(&flac(dir.path(), honest)) else {
                panic!("a file within the limit is held in memory");
            };
            assert_eq!(data.frames(), 48_000);
        }
    }

    #[test]
    fn the_memory_limit_counts_no_more_than_the_nominal_samples_a_second() {
        let pool = MediaPool::new(48_000);
        let info = |sample_rate, channels| AudioFileInfo {
            path: PathBuf::new(),
            sample_rate,
            channels,
            frames: None,
            format: "wave",
        };
        assert_eq!(pool.max_samples(&info(48_000, 2)), 60 * 96_000);
        assert_eq!(
            pool.max_samples(&info(768_000, 64)),
            60 * NOMINAL_SAMPLES_PER_SECOND
        );
    }
}
