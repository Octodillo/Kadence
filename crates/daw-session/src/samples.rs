//! The decoded files samplers play, shared while something plays them.
//!
//! A sampler's node holds its samples decoded in memory. A new node for the
//! same sampler (its saved state changed) or another sampler playing the
//! same file takes the buffer already decoded instead of reading the file
//! again. The pool keeps nothing alive: a buffer goes when its last node
//! does.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::SystemTime;

use daw_devices::SampleBuffer;
use daw_media::MediaError;

/// What tells one version of a file from the next: its length and when it
/// was last written.
type Stamp = (u64, Option<SystemTime>);

fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()))
}

struct Held {
    sample: Weak<SampleBuffer>,
    /// The file as it was when `sample` was decoded from it.
    stamp: Stamp,
}

/// Decoded sampler files by path. See the module docs.
#[derive(Default)]
pub(crate) struct SamplePool {
    held: HashMap<PathBuf, Held>,
}

impl SamplePool {
    /// The file at `path` decoded: the buffer something already plays when
    /// the file has not changed since, else read now. Not real-time safe.
    pub fn load(&mut self, path: &Path) -> Result<Arc<SampleBuffer>, MediaError> {
        self.held.retain(|_, h| h.sample.strong_count() > 0);
        let now = stamp(path);
        if let Some(held) = self.held.get(path)
            && Some(held.stamp) == now
            && let Some(sample) = held.sample.upgrade()
        {
            return Ok(sample);
        }
        let sample = Arc::new(SampleBuffer::load(path)?);
        if let Some(stamp) = now {
            self.hold_as(path.to_path_buf(), &sample, stamp);
        }
        Ok(sample)
    }

    /// Takes `sample` as what the file at `path` decodes to, for as long as
    /// the caller (or a node) keeps it and the file stays as it is.
    pub fn hold(&mut self, path: PathBuf, sample: &Arc<SampleBuffer>) {
        if let Some(stamp) = stamp(&path) {
            self.hold_as(path, sample, stamp);
        }
    }

    fn hold_as(&mut self, path: PathBuf, sample: &Arc<SampleBuffer>, stamp: Stamp) {
        let sample = Arc::downgrade(sample);
        self.held.insert(path, Held { sample, stamp });
    }
}
