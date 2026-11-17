//! Golden audio files: rendered audio compared with a stored reference.
//!
//! A golden file is little-endian: the channel count and the frame count as
//! `u32`, then every channel's samples in turn as `f32`. After an intended
//! change to the audio, run the tests with [`UPDATE_ENV`] set to rewrite the
//! files, and review the difference by ear or by plot before committing.

use std::path::Path;

/// When set, to any value (even `0` or nothing), [`check`] writes the files
/// instead of comparing.
pub const UPDATE_ENV: &str = "KADENCE_UPDATE_GOLDEN";

/// How closely the audio has to match the file.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Match {
    /// Bit for bit. For audio made only of operations IEEE 754 defines
    /// exactly (add, multiply, divide, square root), the same on every
    /// platform.
    Exact,
    /// Every sample within this distance. For audio that goes through libm
    /// (`sin`, `exp`, `powf`), whose last bits differ between platforms.
    Within(f32),
}

/// The calling crate's `tests/golden` directory, where its golden files
/// live.
#[macro_export]
macro_rules! golden_dir {
    () => {
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden")
    };
}

impl Match {
    fn accepts(self, got: f32, want: f32) -> bool {
        match self {
            Self::Exact => got.to_bits() == want.to_bits(),
            Self::Within(tolerance) => (got - want).abs() <= tolerance,
        }
    }
}

/// Compares `channels` (one slice or `Vec` of samples each) with
/// `<dir>/<name>.f32`, or writes that file when [`UPDATE_ENV`] is set. `dir`
/// is usually [`golden_dir!`](crate::golden_dir). A mismatch reports the
/// sample that is furthest off.
///
/// # Panics
/// If the audio has a non-finite sample, the channels differ in length, the
/// file is missing or malformed, or the audio does not match it.
#[track_caller]
pub fn check(dir: impl AsRef<Path>, name: &str, channels: &[impl AsRef<[f32]>], how: Match) {
    let channels: Vec<&[f32]> = channels.iter().map(AsRef::as_ref).collect();
    let update = std::env::var_os(UPDATE_ENV).is_some();
    check_or_update(dir.as_ref(), name, &channels, how, update);
}

#[track_caller]
fn check_or_update(dir: &Path, name: &str, channels: &[&[f32]], how: Match, update: bool) {
    let frames = channels.first().map_or(0, |c| c.len());
    for c in channels {
        assert_eq!(c.len(), frames, "{name}: channels differ in length");
        assert!(c.iter().all(|s| s.is_finite()), "{name}: non-finite output");
    }
    let path = dir.join(format!("{name}.f32"));
    if update {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(&path, encode(channels))
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        return;
    }
    let bytes = std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e}; run with {UPDATE_ENV}=1 to create it",
            path.display()
        )
    });
    let golden = decode(&bytes).unwrap_or_else(|| panic!("{}: not a golden file", path.display()));
    assert_eq!(golden.len(), channels.len(), "{name}: channel count");
    assert_eq!(golden.first().map_or(0, Vec::len), frames, "{name}: length");
    // (how far off, channel, frame, got, want) of the worst sample.
    let mut worst: Option<(f32, usize, usize, f32, f32)> = None;
    for (c, (got, want)) in channels.iter().zip(&golden).enumerate() {
        for (i, (&g, &w)) in got.iter().zip(want).enumerate() {
            let off = (g - w).abs();
            if !how.accepts(g, w) && worst.is_none_or(|(most, ..)| off > most) {
                worst = Some((off, c, i, g, w));
            }
        }
    }
    if let Some((_, c, i, got, want)) = worst {
        panic!(
            "{name}: channel {c} frame {i} is {got}, the golden file has {want} ({how:?}); \
             if the change is intended, rewrite it with {UPDATE_ENV}=1"
        );
    }
}

fn encode(channels: &[&[f32]]) -> Vec<u8> {
    let frames = channels.first().map_or(0, |c| c.len());
    let header = [channels.len(), frames].map(|n| u32::try_from(n).expect("fits the header"));
    let mut bytes: Vec<u8> = header.iter().flat_map(|n| n.to_le_bytes()).collect();
    for s in channels.iter().copied().flatten() {
        bytes.extend(s.to_le_bytes());
    }
    bytes
}

/// The channels in `bytes`, or `None` if it is not a whole golden file.
fn decode(bytes: &[u8]) -> Option<Vec<Vec<f32>>> {
    let (words, rest) = bytes.as_chunks::<4>();
    let mut words = words.iter().map(|w| u32::from_le_bytes(*w));
    let channels = words.next()? as usize;
    let frames = words.next()? as usize;
    let samples: Vec<f32> = words.map(f32::from_bits).collect();
    if !rest.is_empty() || samples.len() != channels.checked_mul(frames)? {
        return None;
    }
    if frames == 0 {
        return Some(vec![Vec::new(); channels]);
    }
    Some(samples.chunks_exact(frames).map(<[f32]>::to_vec).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const L: [f32; 3] = [0.0, 0.5, -1.0];
    const R: [f32; 3] = [0.25, -0.0, 1.0];

    fn written() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        check_or_update(dir.path(), "t", &[&L, &R], Match::Exact, true);
        dir
    }

    #[test]
    fn a_written_file_matches_what_wrote_it() {
        let dir = written();
        check_or_update(dir.path(), "t", &[&L, &R], Match::Exact, false);
        assert_eq!(
            decode(&encode(&[&L, &R])),
            Some(vec![L.to_vec(), R.to_vec()])
        );
    }

    #[test]
    fn tolerance_absorbs_small_differences_only() {
        let dir = written();
        let near = [0.0, 0.5 + 5e-5, -1.0];
        check_or_update(dir.path(), "t", &[&near, &R], Match::Within(1e-4), false);
        let far = [0.0, 0.5 + 5e-4, -1.0];
        let failed = std::panic::catch_unwind(|| {
            check_or_update(dir.path(), "t", &[&far, &R], Match::Within(1e-4), false);
        });
        assert!(failed.is_err());
    }

    #[test]
    #[should_panic(expected = "channel 1 frame 1")]
    fn exact_tells_zero_from_negative_zero() {
        let dir = written();
        let r = [0.25, 0.0, 1.0];
        check_or_update(dir.path(), "t", &[&L, &r], Match::Exact, false);
    }

    #[test]
    #[should_panic(expected = "channel 1 frame 2")]
    fn a_mismatch_reports_the_sample_furthest_off() {
        let dir = written();
        let l = [0.0, 0.5 + 1e-3, -1.0];
        let r = [0.25, -0.0, 1.0 + 1e-2];
        check_or_update(dir.path(), "t", &[&l, &r], Match::Within(1e-4), false);
    }

    #[test]
    #[should_panic(expected = "channel count")]
    fn a_different_channel_count_fails() {
        let dir = written();
        check_or_update(dir.path(), "t", &[&L], Match::Exact, false);
    }

    #[test]
    #[should_panic(expected = "to create it")]
    fn a_missing_file_says_how_to_create_it() {
        let dir = tempfile::tempdir().unwrap();
        check_or_update(dir.path(), "t", &[&L], Match::Exact, false);
    }

    #[test]
    fn truncated_files_are_rejected() {
        let bytes = encode(&[&L, &R]);
        assert_eq!(decode(&bytes[..bytes.len() - 1]), None);
        assert_eq!(decode(&bytes[..bytes.len() - 4]), None);
        assert_eq!(decode(&bytes[..4]), None);
    }
}
