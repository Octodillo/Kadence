//! The decoded files samplers play, shared while something plays them and
//! for a while after.
//!
//! A sampler's node holds its samples decoded in memory. A new node for the
//! same sampler (its saved state changed) or another sampler playing the
//! same file takes the buffer already decoded instead of reading the file
//! again. The pool also keeps the buffers nothing plays any more, the ones
//! used last first, up to [`KEEP_BYTES`], so undoing or redoing a change of
//! sample finds the buffer it played before instead of decoding the file
//! again (on the UI thread, which a long sample would block).
//!
//! Buffers something plays do not count: they take their memory anyway.
//! The pool notices one is no longer played when it is next asked for a
//! file, and lets go of what is beyond the limit then; until that, what
//! stopped playing since is kept too.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use daw_devices::SampleBuffer;
use daw_media::MediaError;

/// The most memory, in bytes, the buffers nothing plays take: about eleven
/// minutes of stereo audio at 48 kHz. A sampler plays short sounds, so
/// this covers going back and forth between many.
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
    sample: Arc<SampleBuffer>,
    /// The file as it was when `sample` was decoded from it.
    stamp: Stamp,
    /// When it was used last, in [`SamplePool::clock`] ticks.
    used: u64,
}

impl Held {
    /// Whether only the pool has it, so keeping it costs memory.
    fn idle(&self) -> bool {
        Arc::strong_count(&self.sample) == 1
    }
}

/// Decoded sampler files by path. See the module docs.
pub(crate) struct SamplePool {
    held: HashMap<PathBuf, Held>,
    /// Counts uses, to order them.
    clock: u64,
    /// The most bytes kept that nothing plays: [`KEEP_BYTES`], less in
    /// tests.
    keep_bytes: usize,
}

impl Default for SamplePool {
    fn default() -> Self {
        Self {
            held: HashMap::new(),
            clock: 0,
            keep_bytes: KEEP_BYTES,
        }
    }
}

impl SamplePool {
    /// The file at `path` decoded: the buffer something plays, or played
    /// lately, when the file has not changed since, else read now. Not
    /// real-time safe.
    pub fn load(&mut self, path: &Path) -> Result<Arc<SampleBuffer>, MediaError> {
        let now = stamp(path);
        let tick = self.tick();
        let found = self
            .held
            .get_mut(path)
            .filter(|held| Some(held.stamp) == now)
            .map(|held| {
                held.used = tick;
                held.sample.clone()
            });
        let sample = match found {
            Some(sample) => sample,
            None => {
                let sample = Arc::new(SampleBuffer::load(path)?);
                match now {
                    Some(stamp) => self.hold_as(path.to_path_buf(), &sample, stamp),
                    // Gone or unreadable as a file: not worth keeping.
                    None => {
                        self.held.remove(path);
                    }
                }
                sample
            }
        };
        self.trim();
        Ok(sample)
    }

    /// Takes `sample` as what the file at `path` decodes to, for as long as
    /// the file stays as it is, in place of what the pool had for it.
    pub fn hold(&mut self, path: PathBuf, sample: &Arc<SampleBuffer>) {
        if let Some(stamp) = stamp(&path) {
            self.hold_as(path, sample, stamp);
            self.trim();
        }
    }

    /// Lets go of the buffers nothing plays, for a new document whose
    /// samples are not these.
    pub fn forget_unused(&mut self) {
        self.held.retain(|_, held| !held.idle());
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn hold_as(&mut self, path: PathBuf, sample: &Arc<SampleBuffer>, stamp: Stamp) {
        let used = self.tick();
        let sample = sample.clone();
        self.held.insert(
            path,
            Held {
                sample,
                stamp,
                used,
            },
        );
    }

    /// Lets go of the buffers nothing plays beyond `keep_bytes`, the least
    /// recently used first.
    fn trim(&mut self) {
        let mut idle: Vec<(u64, &PathBuf, usize)> = self
            .held
            .iter()
            .filter(|(_, held)| held.idle())
            .map(|(path, held)| (held.used, path, bytes(&held.sample)))
            .collect();
        idle.sort_unstable_by_key(|&(used, _, _)| std::cmp::Reverse(used));
        let mut kept = 0;
        let beyond: Vec<PathBuf> = idle
            .into_iter()
            .filter(|&(_, _, size)| {
                kept += size;
                kept > self.keep_bytes
            })
            .map(|(_, path, _)| path.clone())
            .collect();
        for path in beyond {
            self.held.remove(&path);
        }
    }

    #[cfg(test)]
    fn idle_bytes(&self) -> usize {
        self.held
            .values()
            .filter(|held| held.idle())
            .map(|held| bytes(&held.sample))
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Weak;

    use super::*;

    fn write_tone(path: &Path, frames: usize) {
        let data = daw_media::AudioData {
            sample_rate: 48_000,
            channels: vec![vec![0.25; frames]],
        };
        daw_media::write_wav(path, &data, daw_media::WavFormat::Float32).unwrap();
    }

    /// Bytes a mono tone of `frames` decodes to.
    fn size(frames: usize) -> usize {
        frames * std::mem::size_of::<f32>()
    }

    /// A pool that keeps `keep_bytes` and a folder of tones, `frames` long,
    /// named by number.
    fn pool_with_tones(
        keep_bytes: usize,
        count: usize,
        frames: usize,
    ) -> (SamplePool, tempfile::TempDir, Vec<PathBuf>) {
        let dir = tempfile::tempdir().unwrap();
        let files = (0..count)
            .map(|i| {
                let file = dir.path().join(format!("{i}.wav"));
                write_tone(&file, frames);
                file
            })
            .collect();
        let pool = SamplePool {
            keep_bytes,
            ..SamplePool::default()
        };
        (pool, dir, files)
    }

    /// Loads `file` and lets go of it, as a node that is replaced does.
    fn played(pool: &mut SamplePool, file: &Path) -> Weak<SampleBuffer> {
        Arc::downgrade(&pool.load(file).unwrap())
    }

    /// Issue #268: undo and redo of Load Sample find the buffer again
    /// after the node that played it is gone.
    #[test]
    fn a_buffer_nothing_plays_any_more_is_found_again() {
        let (mut pool, _dir, files) = pool_with_tones(KEEP_BYTES, 1, 480);
        let weak = played(&mut pool, &files[0]);
        let again = pool.load(&files[0]).unwrap();
        assert!(
            Weak::ptr_eq(&weak, &Arc::downgrade(&again)),
            "decoded again"
        );
    }

    #[test]
    fn a_changed_file_is_read_again() {
        let (mut pool, _dir, files) = pool_with_tones(KEEP_BYTES, 1, 480);
        let first = pool.load(&files[0]).unwrap();
        write_tone(&files[0], 960);
        let again = pool.load(&files[0]).unwrap();
        assert!(!Arc::ptr_eq(&first, &again));
        assert_eq!(again.len(), 960);
    }

    #[test]
    fn only_so_much_is_kept() {
        // Room for two tones that nothing plays.
        let (mut pool, _dir, files) = pool_with_tones(size(2 * 480), 4, 480);
        let buffers: Vec<_> = files.iter().map(|f| played(&mut pool, f)).collect();
        pool.trim();
        let alive: Vec<bool> = buffers.iter().map(|b| b.strong_count() > 0).collect();
        assert_eq!(alive, [false, false, true, true]);
        assert!(pool.idle_bytes() <= size(2 * 480));

        pool.forget_unused();
        assert!(buffers.iter().all(|b| b.strong_count() == 0));
        assert_eq!(pool.idle_bytes(), 0);
    }

    #[test]
    fn a_buffer_used_again_is_kept_longest() {
        let (mut pool, _dir, files) = pool_with_tones(size(3 * 480), 4, 480);
        let a = played(&mut pool, &files[0]);
        let b = played(&mut pool, &files[1]);
        played(&mut pool, &files[2]);
        played(&mut pool, &files[0]);
        played(&mut pool, &files[3]);
        pool.trim();
        assert_eq!(b.strong_count(), 0, "least recently used goes first");
        assert_eq!(a.strong_count(), 1);
    }

    #[test]
    fn buffers_still_played_do_not_count() {
        let (mut pool, _dir, files) = pool_with_tones(size(480), 3, 480);
        let playing: Vec<_> = files[..2].iter().map(|f| pool.load(f).unwrap()).collect();
        let idle = played(&mut pool, &files[2]);
        assert_eq!(idle.strong_count(), 1, "the playing ones take no room");
        drop(playing);
        // Noticed at the next use, and only one of the three fits.
        pool.trim();
        assert_eq!(pool.idle_bytes(), size(480));
        assert_eq!(idle.strong_count(), 1, "the one used last stays");
    }

    #[test]
    fn holding_a_file_replaces_what_was_kept_for_it() {
        let (mut pool, _dir, files) = pool_with_tones(KEEP_BYTES, 1, 480);
        let old = played(&mut pool, &files[0]);
        let new = Arc::new(SampleBuffer::mono(vec![0.5; 480], 48_000.0));
        pool.hold(files[0].clone(), &new);
        assert_eq!(old.strong_count(), 0, "nothing keeps the old buffer");
        assert!(Arc::ptr_eq(&pool.load(&files[0]).unwrap(), &new));
    }
}
