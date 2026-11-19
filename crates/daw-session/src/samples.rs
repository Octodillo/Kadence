//! The decoded files samplers play, shared while something plays them and
//! for a while after.
//!
//! A sampler's node holds its samples decoded in memory. A new node for the
//! same sampler (its saved state changed) or another sampler playing the
//! same file takes the buffer already decoded instead of reading the file
//! again. The pool also keeps the buffers used last alive, up to
//! [`KEEP_BYTES`], so undoing or redoing a change of sample finds the
//! buffer it played before instead of decoding the file again (on the UI
//! thread, which a long sample would block).

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::SystemTime;

use daw_devices::SampleBuffer;
use daw_media::MediaError;

/// The most memory, in bytes, the buffers used last take while nothing
/// plays them: a few minutes of stereo audio. A sampler plays short
/// sounds, so this covers going back and forth between several.
const KEEP_BYTES: usize = 256 << 20;

/// What tells one version of a file from the next: its length and when it
/// was last written.
type Stamp = (u64, Option<SystemTime>);

fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()))
}

fn bytes(sample: &SampleBuffer) -> usize {
    sample
        .channels
        .iter()
        .map(|c| c.len() * std::mem::size_of::<f32>())
        .sum()
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
    /// The buffers used last, least recently used first, kept alive while
    /// they take no more than [`KEEP_BYTES`] together.
    recent: VecDeque<Arc<SampleBuffer>>,
    recent_bytes: usize,
}

impl SamplePool {
    /// The file at `path` decoded: the buffer something plays, or played
    /// lately, when the file has not changed since, else read now. Not
    /// real-time safe.
    pub fn load(&mut self, path: &Path) -> Result<Arc<SampleBuffer>, MediaError> {
        self.held.retain(|_, h| h.sample.strong_count() > 0);
        let now = stamp(path);
        if let Some(held) = self.held.get(path)
            && Some(held.stamp) == now
            && let Some(sample) = held.sample.upgrade()
        {
            self.used(&sample);
            return Ok(sample);
        }
        let sample = Arc::new(SampleBuffer::load(path)?);
        if let Some(stamp) = now {
            self.hold_as(path.to_path_buf(), &sample, stamp);
        }
        Ok(sample)
    }

    /// Takes `sample` as what the file at `path` decodes to, for as long as
    /// the file stays as it is and the buffer is played or used lately.
    pub fn hold(&mut self, path: PathBuf, sample: &Arc<SampleBuffer>) {
        if let Some(stamp) = stamp(&path) {
            self.hold_as(path, sample, stamp);
        }
    }

    /// Lets go of the buffers nothing plays, for a new document whose
    /// samples are not these.
    pub fn forget_unused(&mut self) {
        self.recent.clear();
        self.recent_bytes = 0;
        self.held.retain(|_, h| h.sample.strong_count() > 0);
    }

    fn hold_as(&mut self, path: PathBuf, sample: &Arc<SampleBuffer>, stamp: Stamp) {
        self.used(sample);
        let sample = Arc::downgrade(sample);
        self.held.insert(path, Held { sample, stamp });
    }

    /// Makes `sample` the one used last, letting go of the least recently
    /// used ones beyond [`KEEP_BYTES`].
    fn used(&mut self, sample: &Arc<SampleBuffer>) {
        if let Some(i) = self.recent.iter().position(|s| Arc::ptr_eq(s, sample)) {
            let s = self.recent.remove(i).expect("found just now");
            self.recent.push_back(s);
            return;
        }
        self.recent_bytes += bytes(sample);
        self.recent.push_back(sample.clone());
        while self.recent_bytes > KEEP_BYTES
            && let Some(old) = self.recent.pop_front()
        {
            self.recent_bytes -= bytes(&old);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tone(path: &Path, frames: usize) {
        let data = daw_media::AudioData {
            sample_rate: 48_000,
            channels: vec![vec![0.25; frames]],
        };
        daw_media::write_wav(path, &data, daw_media::WavFormat::Float32).unwrap();
    }

    /// Issue #268: undo and redo of Load Sample find the buffer again
    /// after the node that played it is gone.
    #[test]
    fn a_buffer_nothing_plays_any_more_is_found_again() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("hit.wav");
        write_tone(&file, 480);
        let mut pool = SamplePool::default();
        let first = pool.load(&file).unwrap();
        let weak = Arc::downgrade(&first);
        drop(first);
        let again = pool.load(&file).unwrap();
        assert!(
            Weak::ptr_eq(&weak, &Arc::downgrade(&again)),
            "decoded again"
        );
    }

    #[test]
    fn a_changed_file_is_read_again() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("hit.wav");
        write_tone(&file, 480);
        let mut pool = SamplePool::default();
        let first = pool.load(&file).unwrap();
        write_tone(&file, 960);
        let again = pool.load(&file).unwrap();
        assert!(!Arc::ptr_eq(&first, &again));
        assert_eq!(again.len(), 960);
    }

    #[test]
    fn only_so_much_is_kept() {
        let mut pool = SamplePool::default();
        // Each a third of what is kept, plus a little.
        let frames = KEEP_BYTES / 3 / std::mem::size_of::<f32>() + 1;
        let buffers: Vec<Weak<SampleBuffer>> = (0..4)
            .map(|_| {
                let sample = Arc::new(SampleBuffer::mono(vec![0.0; frames], 48_000.0));
                pool.used(&sample);
                Arc::downgrade(&sample)
            })
            .collect();
        let alive: Vec<bool> = buffers.iter().map(|b| b.strong_count() > 0).collect();
        assert_eq!(alive, [false, false, true, true]);
        assert!(pool.recent_bytes <= KEEP_BYTES);

        pool.forget_unused();
        assert!(buffers.iter().all(|b| b.strong_count() == 0));
        assert_eq!(pool.recent_bytes, 0);
    }

    #[test]
    fn a_buffer_used_again_is_kept_longest() {
        let mut pool = SamplePool::default();
        let frames = KEEP_BYTES / 3 / std::mem::size_of::<f32>();
        let make = || Arc::new(SampleBuffer::mono(vec![0.0; frames], 48_000.0));
        let (a, b, c) = (make(), make(), make());
        pool.used(&a);
        pool.used(&b);
        pool.used(&c);
        pool.used(&a);
        let (b_weak, a_weak) = (Arc::downgrade(&b), Arc::downgrade(&a));
        drop((a, b, c));
        pool.used(&make());
        assert_eq!(b_weak.strong_count(), 0, "least recently used goes first");
        assert_eq!(a_weak.strong_count(), 1);
    }
}
