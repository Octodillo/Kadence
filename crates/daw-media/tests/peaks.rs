mod common;

use std::time::Instant;

use common::test_signal;
use daw_media::{AudioData, BASE_FRAMES_PER_PEAK, PeakBuilder, PeakCache, WavFormat, write_wav};

#[test]
fn levels_hold_exact_min_max() {
    let frames = 100_000;
    let data = test_signal(48_000, frames, 2);
    let mut b = PeakBuilder::new(2, 48_000);
    // Odd chunk sizes so peaks straddle pushes.
    for chunk in data.interleaved().chunks(2 * 777) {
        b.push_interleaved(chunk);
    }
    let cache = b.finish();
    assert_eq!(cache.frames, frames as u64);
    assert_eq!(cache.levels[0].frames_per_peak, BASE_FRAMES_PER_PEAK);
    for level in &cache.levels {
        let fpp = level.frames_per_peak as usize;
        assert_eq!(level.len(2), frames.div_ceil(fpp));
        for p in [0, level.len(2) / 2, level.len(2) - 1] {
            for c in 0..2 {
                let span = &data.channels[c][p * fpp..((p + 1) * fpp).min(frames)];
                let lo = span.iter().copied().fold(f32::MAX, f32::min);
                let hi = span.iter().copied().fold(f32::MIN, f32::max);
                let i = (p * 2 + c) * 2;
                let got = (
                    level.data[i] as f32 / 32767.0,
                    level.data[i + 1] as f32 / 32767.0,
                );
                assert!((got.0 - lo).abs() < 1e-4 && (got.1 - hi).abs() < 1e-4);
            }
        }
    }
}

#[test]
fn query_spans_columns() {
    // Silence with one spike in the middle.
    let mut data = AudioData::silence(48_000, 1, 48_000);
    data.channels[0][30_000] = 0.5;
    data.channels[0][30_001] = -0.25;
    let mut b = PeakBuilder::new(1, 48_000);
    b.push_interleaved(&data.channels[0]);
    let cache = b.finish();
    let mut out = Vec::new();
    for width in [10, 100, 1000] {
        cache.query(0, 0, 48_000, width, &mut out);
        assert_eq!(out.len(), width);
        let col = 30_000 * width / 48_000;
        assert!(
            (out[col].1 - 0.5).abs() < 1e-3,
            "width {width}: {:?}",
            out[col]
        );
        assert!((out[col].0 + 0.25).abs() < 1e-3);
        assert_eq!(out[0], (0.0, 0.0));
    }
    cache.query(5, 0, 10, 10, &mut out);
    assert!(out.is_empty());
}

#[test]
fn disk_cache_is_reused_and_invalidated() {
    let dir = tempfile::tempdir().unwrap();
    let audio = dir.path().join("a.wav");
    write_wav(&audio, &test_signal(48_000, 20_000, 2), WavFormat::Pcm16).unwrap();
    let first = PeakCache::load_or_build(&audio).unwrap();
    let cache_path = PeakCache::path_for(&audio);
    assert!(cache_path.exists());
    assert_eq!(cache_path.file_name().unwrap(), "a.wav.peaks");
    let second = PeakCache::load_or_build(&audio).unwrap();
    assert_eq!(first, second);

    // Changing the audio (different length) makes the cache stale.
    std::thread::sleep(std::time::Duration::from_millis(20));
    write_wav(&audio, &test_signal(48_000, 30_000, 2), WavFormat::Pcm16).unwrap();
    assert_eq!(PeakCache::load_or_build(&audio).unwrap().frames, 30_000);

    // A corrupt cache is rebuilt, not trusted.
    std::fs::write(&cache_path, b"DAWPEAKSgarbage").unwrap();
    assert_eq!(PeakCache::load_or_build(&audio).unwrap().frames, 30_000);
}

/// Done-when: the peak cache for a 10-minute stereo file builds in under 1 s.
#[test]
fn ten_minute_stereo_builds_under_a_second() {
    let dir = tempfile::tempdir().unwrap();
    let audio = dir.path().join("long.wav");
    let frames = 10 * 60 * 48_000;
    write_wav(&audio, &test_signal(48_000, frames, 2), WavFormat::Pcm16).unwrap();
    let t = Instant::now();
    let cache = PeakCache::load_or_build(&audio).unwrap();
    let elapsed = t.elapsed();
    assert_eq!(cache.frames, frames as u64);
    eprintln!("10-minute stereo peak build: {elapsed:?}");
    // The 1 s target is for optimised builds. Debug test runs share the CI runner with
    // every other test binary under nextest, so wall-clock time there only gets a loose
    // sanity bound (it measures ~0.3 s alone, ~2 s under full parallel load).
    let limit = if cfg!(debug_assertions) { 10.0 } else { 1.0 };
    assert!(elapsed.as_secs_f64() < limit, "took {elapsed:?}");
}

#[test]
fn unwritable_cache_still_returns_peaks() {
    let dir = tempfile::tempdir().unwrap();
    let audio = dir.path().join("ro.wav");
    write_wav(&audio, &test_signal(48_000, 5_000, 1), WavFormat::Pcm16).unwrap();
    // A non-empty directory where the cache goes makes the save fail, even as root.
    let blocker = PeakCache::path_for(&audio);
    std::fs::create_dir(&blocker).unwrap();
    std::fs::write(blocker.join("keep"), b"x").unwrap();
    let cache = PeakCache::load_or_build(&audio).unwrap();
    assert_eq!(cache.frames, 5_000);
    // The failed save cleaned up after itself.
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn damaged_cache_headers_are_rebuilt_not_trusted() {
    let dir = tempfile::tempdir().unwrap();
    let audio = dir.path().join("d.wav");
    write_wav(&audio, &test_signal(48_000, 9_000, 2), WavFormat::Pcm16).unwrap();
    let good = PeakCache::load_or_build(&audio).unwrap();
    let cache_path = PeakCache::path_for(&audio);
    let original = std::fs::read(&cache_path).unwrap();
    // Header: magic 8, version 4, len 8, mtime 8, rate 4, channels 4, frames 8 (at 36),
    // level count 4 (at 44), then the first level's frames_per_peak 4 (at 48).
    let patches: [(usize, &[u8]); 4] = [
        (44, &0u32.to_le_bytes()),         // no levels
        (48, &0u32.to_le_bytes()),         // zero frames per peak
        (36, &u64::MAX.to_le_bytes()),     // absurd frame count
        (36, &(1u64 << 40).to_le_bytes()), // big but consistent-looking
    ];
    for (at, bytes) in patches {
        let mut damaged = original.clone();
        damaged[at..at + bytes.len()].copy_from_slice(bytes);
        std::fs::write(&cache_path, &damaged).unwrap();
        let rebuilt = PeakCache::load_or_build(&audio).unwrap();
        assert_eq!(rebuilt, good, "patch at {at}");
        let mut out = Vec::new();
        rebuilt.query(0, 0, 9_000, 50, &mut out);
        assert_eq!(out.len(), 50);
    }
}
