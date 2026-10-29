mod common;

use std::time::Duration;

use assert_no_alloc::{AllocDisabler, assert_no_alloc, reset_violation_count, violation_count};
use common::{max_abs_diff, test_signal};
use daw_media::{PeakCache, Recorder, WavFormat, decode_file};

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

#[test]
fn records_rt_blocks_to_wav_with_peaks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("take.wav");
    let src = test_signal(48_000, 48_000, 2);
    let (rec, mut input) = Recorder::start(&path, 2, 48_000, WavFormat::Float32, 1.0).unwrap();
    reset_violation_count();
    for start in (0..48_000).step_by(256) {
        let end = (start + 256).min(48_000);
        let queued = assert_no_alloc(|| {
            input.write(&[&src.channels[0][start..end], &src.channels[1][start..end]])
        });
        assert_eq!(queued, end - start);
        // Faster than real time, but slow enough for the writer to keep up.
        std::thread::sleep(Duration::from_micros(200));
    }
    assert_eq!(violation_count(), 0, "RecordInput::write allocated");
    drop(input);
    let stats = rec.finish().unwrap();
    assert_eq!(stats.frames, 48_000);
    assert_eq!(stats.dropped_frames, 0);
    let back = decode_file(&path).unwrap();
    for c in 0..2 {
        assert_eq!(max_abs_diff(&back.channels[c], &src.channels[c]), 0.0);
    }
    // The peak cache was saved next to the file and is current.
    assert!(PeakCache::path_for(&path).exists());
    assert_eq!(PeakCache::load_or_build(&path).unwrap(), stats.peaks);
}

#[test]
fn overflow_drops_whole_blocks_and_counts_them() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("burst.wav");
    // Tiny ring (the minimum, 4096 frames); push 10 x 1000 frames instantly.
    let (rec, mut input) = Recorder::start(&path, 1, 48_000, WavFormat::Pcm24, 0.0).unwrap();
    let block = vec![0.25f32; 1000];
    let mut queued = 0;
    for _ in 0..10 {
        queued += input.write(&[&block]);
    }
    drop(input);
    let stats = rec.finish().unwrap();
    assert_eq!(stats.frames as usize, queued);
    assert_eq!(stats.dropped_frames as usize, 10_000 - queued);
    assert_eq!(decode_file(&path).unwrap().frames(), queued);
}

#[test]
fn file_is_playable_before_finish() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("live.wav");
    let (rec, mut input) = Recorder::start(&path, 1, 48_000, WavFormat::Pcm16, 1.0).unwrap();
    let block = vec![0.5f32; 4800];
    input.write(&[&block]);
    // The header is rewritten about once a second.
    std::thread::sleep(Duration::from_millis(1300));
    let partial = decode_file(&path).unwrap();
    assert_eq!(partial.frames(), 4800);
    drop(input);
    rec.finish().unwrap();
}

#[test]
fn failed_peak_save_does_not_fail_the_take() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("take.wav");
    std::fs::create_dir(dir.path().join("take.wav.peaks.tmp")).unwrap();
    let (rec, mut input) = Recorder::start(&path, 1, 48_000, WavFormat::Pcm16, 1.0).unwrap();
    input.write(&[&[0.1f32; 1000][..]]);
    drop(input);
    let stats = rec.finish().unwrap();
    assert_eq!(stats.frames, 1000);
    assert!(stats.peaks_error.is_some());
    assert_eq!(decode_file(&path).unwrap().frames(), 1000);
}
