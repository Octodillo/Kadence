//! Disk streaming through the RT reader.
mod common;

use std::f32::consts::TAU;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use assert_no_alloc::{AllocDisabler, assert_no_alloc, reset_violation_count, violation_count};
use daw_core::time::SampleTime;
use daw_media::{
    AudioData, ClipReader, DiskStreamer, ReadStatus, StreamerConfig, WavFormat, write_wav,
};

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

const RATE: u32 = 48_000;
const BLOCK: usize = 256;

fn freq(i: usize) -> f32 {
    100.0 + 13.0 * i as f32
}

/// Sample `n` of channel `c` of file `i`. Channel 1 is phase-inverted so swaps show.
fn expected(i: usize, c: usize, n: i64, rate: u32) -> f32 {
    let s = 0.5 * (TAU * freq(i) * n as f32 / rate as f32).sin();
    if c == 0 { s } else { -s }
}

fn write_file(dir: &Path, i: usize, rate: u32, seconds: f32, channels: usize) -> PathBuf {
    let frames = (seconds * rate as f32) as usize;
    let data = AudioData {
        sample_rate: rate,
        channels: (0..channels)
            .map(|c| {
                (0..frames)
                    .map(|n| expected(i, c, n as i64, rate))
                    .collect()
            })
            .collect(),
    };
    let path = dir.join(format!("clip{i}.wav"));
    write_wav(&path, &data, WavFormat::Float32).unwrap();
    path
}

fn wait_ready(readers: &mut [ClipReader]) {
    let t = Instant::now();
    while !readers.iter_mut().all(|r| r.is_ready()) {
        assert!(
            t.elapsed() < Duration::from_secs(10),
            "streams never became ready"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Done-when: 64 files read concurrently through the RT reader with no underruns, paced
/// like a dummy backend at 256 frames per cycle, without allocating on the RT side.
#[test]
fn sixty_four_streams_in_real_time_without_underruns() {
    let dir = tempfile::tempdir().unwrap();
    let n_files = 64;
    let seconds = 4.0;
    let paths: Vec<_> = (0..n_files)
        .map(|i| write_file(dir.path(), i, RATE, seconds, 2))
        .collect();
    let streamer = DiskStreamer::new(StreamerConfig {
        sample_rate: RATE,
        buffer_seconds: 1.0,
        ..Default::default()
    });
    let mut readers: Vec<ClipReader> = paths.iter().map(|p| streamer.open(p).unwrap()).collect();
    wait_ready(&mut readers);

    let mut left = vec![0.0f32; BLOCK];
    let mut right = vec![0.0f32; BLOCK];
    let cycles = (seconds as usize * RATE as usize) / BLOCK;
    let period = Duration::from_secs_f64(BLOCK as f64 / f64::from(RATE));
    let start = Instant::now();
    let mut worst_err = 0.0f32;
    let mut bad_status = 0;
    reset_violation_count();
    for cycle in 0..cycles {
        let pos = (cycle * BLOCK) as i64;
        assert_no_alloc(|| {
            for (i, r) in readers.iter_mut().enumerate() {
                let status = r.read(SampleTime(pos), &mut [&mut left[..], &mut right[..]]);
                if status != ReadStatus::Ok {
                    bad_status += 1;
                }
                for f in (0..BLOCK).step_by(17) {
                    let n = pos + f as i64;
                    worst_err = worst_err.max((left[f] - expected(i, 0, n, RATE)).abs());
                    worst_err = worst_err.max((right[f] - expected(i, 1, n, RATE)).abs());
                }
            }
        });
        // Pace like a sound card: wait for this cycle's deadline.
        let deadline = start + period * (cycle as u32 + 1);
        if let Some(d) = deadline.checked_duration_since(Instant::now()) {
            std::thread::sleep(d);
        }
    }
    assert_eq!(violation_count(), 0, "RT reader allocated");
    let underruns: u64 = readers.iter().map(|r| r.underruns()).sum();
    assert_eq!(underruns, 0);
    assert_eq!(bad_status, 0);
    assert!(worst_err < 1e-6, "data mismatch {worst_err}");
}

#[test]
fn seek_refills_from_new_position() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_file(dir.path(), 3, RATE, 5.0, 2);
    let streamer = DiskStreamer::new(StreamerConfig {
        sample_rate: RATE,
        buffer_seconds: 0.5,
        ..Default::default()
    });
    let mut r = streamer.open(&path).unwrap();
    let mut l = vec![0.0; BLOCK];
    let mut rr = vec![0.0; BLOCK];
    wait_ready(std::slice::from_mut(&mut r));
    assert_eq!(
        r.read(SampleTime(0), &mut [&mut l[..], &mut rr[..]]),
        ReadStatus::Ok
    );

    // Jump far beyond the buffered window: first read is silent and pending.
    let target = 3 * RATE as i64 + 123;
    assert_eq!(
        r.read(SampleTime(target), &mut [&mut l[..], &mut rr[..]]),
        ReadStatus::Pending
    );
    assert!(l.iter().all(|s| *s == 0.0));

    // Cue ahead, wait, then the data is exact.
    let target = 2 * RATE as i64 + 7;
    r.cue(SampleTime(target));
    wait_ready(std::slice::from_mut(&mut r));
    assert_eq!(
        r.read(SampleTime(target), &mut [&mut l[..], &mut rr[..]]),
        ReadStatus::Ok
    );
    for f in 0..BLOCK {
        let n = target + f as i64;
        assert!(
            (l[f] - expected(3, 0, n, RATE)).abs() < 1e-6,
            "frame {f}: {} vs {} (next {})",
            l[f],
            expected(3, 0, n, RATE),
            expected(3, 0, n + 1, RATE)
        );
        assert!((rr[f] - expected(3, 1, n, RATE)).abs() < 1e-6);
    }
    // Small backwards jump within the file also works.
    r.cue(SampleTime(1000));
    wait_ready(std::slice::from_mut(&mut r));
    r.read(SampleTime(1000), &mut [&mut l[..], &mut rr[..]]);
    assert!((l[5] - expected(3, 0, 1005, RATE)).abs() < 1e-6);
    assert_eq!(r.underruns(), 0);
}

#[test]
fn edges_are_silent_and_mono_fills_all_outputs() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_file(dir.path(), 5, RATE, 0.1, 1);
    let streamer = DiskStreamer::new(StreamerConfig::default());
    let mut r = streamer.open(&path).unwrap();
    assert_eq!(r.channels(), 1);
    assert_eq!(r.frames(), Some(4800));
    r.cue(SampleTime(-100));
    wait_ready(std::slice::from_mut(&mut r));
    // The whole clip fits the buffer; give the filler a moment to finish it.
    std::thread::sleep(Duration::from_millis(100));
    let mut a = vec![1.0; BLOCK];
    let mut b = vec![1.0; BLOCK];
    let mut c = vec![1.0; BLOCK];
    // Starts 100 frames before the clip.
    assert_eq!(
        r.read(SampleTime(-100), &mut [&mut a[..], &mut b[..], &mut c[..]]),
        ReadStatus::Ok
    );
    assert!(a[..100].iter().all(|s| *s == 0.0));
    for f in 100..BLOCK {
        let want = expected(5, 0, f as i64 - 100, RATE);
        assert!((a[f] - want).abs() < 1e-6 && a[f] == b[f] && b[f] == c[f]);
    }
    // Run through the end of the file: the tail is silent, without underruns.
    let mut pos = -100 + BLOCK as i64;
    while pos < 6000 {
        let st = r.read(SampleTime(pos), &mut [&mut a[..]]);
        assert_ne!(st, ReadStatus::Underrun, "at {pos}");
        if pos > 4800 {
            assert!(a.iter().all(|s| *s == 0.0));
        }
        pos += BLOCK as i64;
    }
    assert_eq!(r.underruns(), 0);
}

#[test]
fn resampled_stream_has_right_pitch_and_alignment() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_file(dir.path(), 7, 44_100, 2.0, 2);
    let streamer = DiskStreamer::new(StreamerConfig::default());
    let mut r = streamer.open(&path).unwrap();
    assert_eq!(r.info().sample_rate, 44_100);
    wait_ready(std::slice::from_mut(&mut r));
    let mut l = vec![0.0; 48_000];
    let mut rr = vec![0.0; 48_000];
    // Large reads, waiting between them so the filler keeps up.
    for chunk in 0..2 {
        let t = Instant::now();
        while r.buffered_frames() < 24_000 && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(1));
        }
        let at = chunk * 24_000;
        r.read(
            SampleTime(at as i64),
            &mut [&mut l[at..at + 24_000], &mut rr[at..at + 24_000]],
        );
    }
    let f = common::estimate_freq(&l[1000..47_000], RATE);
    assert!((f - freq(7)).abs() < 2.0, "{f}");
    for n in [2_000i64, 20_000, 40_000] {
        let want = expected(7, 0, n, RATE);
        assert!((l[n as usize] - want).abs() < 0.02, "frame {n}");
        assert!((rr[n as usize] + l[n as usize]).abs() < 0.02);
    }
}

#[test]
fn dropping_reader_lets_streamer_release_the_stream() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_file(dir.path(), 1, RATE, 0.5, 2);
    let streamer = DiskStreamer::new(StreamerConfig::default());
    for _ in 0..20 {
        let r = streamer.open(&path).unwrap();
        drop(r);
    }
    drop(streamer); // joins the fill thread
}

#[test]
fn resampled_seek_matches_continuous_playback() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_file(dir.path(), 9, 44_100, 2.0, 2);
    let streamer = DiskStreamer::new(StreamerConfig::default());
    let read_from = |start: i64| {
        let mut r = streamer.open(&path).unwrap();
        r.cue(SampleTime(start));
        wait_ready(std::slice::from_mut(&mut r));
        let t = Instant::now();
        while r.buffered_frames() < 16_384 + 4096 {
            assert!(
                t.elapsed() < Duration::from_secs(10),
                "fill never caught up"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        let mut l = vec![0.0; 16_384];
        let mut rr = vec![0.0; 16_384];
        assert_eq!(
            r.read(SampleTime(start), &mut [&mut l[..], &mut rr[..]]),
            ReadStatus::Ok
        );
        l
    };
    let continuous = read_from(0);
    for start in [7i64, 161, 1234, 9001] {
        let seeked = read_from(start);
        // From the very first frame, the seeked stream must line up sample for sample
        // with continuous playback: no restart transient, no fractional-frame shift.
        for i in 0..(16_384 - start as usize) {
            let d = (seeked[i] - continuous[i + start as usize]).abs();
            assert!(d < 1e-3, "start {start} frame {i}: diff {d}");
        }
    }
}

#[test]
fn repeated_cue_while_pending_still_becomes_ready() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_file(dir.path(), 4, RATE, 3.0, 2);
    let streamer = DiskStreamer::new(StreamerConfig::default());
    let mut r = streamer.open(&path).unwrap();
    let target = SampleTime(RATE as i64 + 5);
    let t = Instant::now();
    // A pre-roll loop that cues every cycle until ready must not keep restarting the seek.
    loop {
        r.cue(target);
        if r.is_ready() {
            break;
        }
        assert!(t.elapsed() < Duration::from_secs(5), "never became ready");
    }
    let mut l = vec![0.0; BLOCK];
    let mut rr = vec![0.0; BLOCK];
    assert_eq!(
        r.read(target, &mut [&mut l[..], &mut rr[..]]),
        ReadStatus::Ok
    );
    assert!((l[0] - expected(4, 0, target.0, RATE)).abs() < 1e-6);
}

#[test]
fn odd_rate_seek_is_ready_only_with_data_at_the_target() {
    let dir = tempfile::tempdir().unwrap();
    // gcd(22_254, 48_000) = 6, so an aligned seek period would be 8000 engine frames,
    // longer than a fill block.
    let path = write_file(dir.path(), 2, 22_254, 2.0, 2);
    let streamer = DiskStreamer::new(StreamerConfig::default());
    let mut r = streamer.open(&path).unwrap();
    for target in [10_000i64, 31_999, 40_000] {
        r.cue(SampleTime(target));
        wait_ready(std::slice::from_mut(&mut r));
        let mut l = vec![0.0; BLOCK];
        let mut rr = vec![0.0; BLOCK];
        assert_eq!(
            r.read(SampleTime(target), &mut [&mut l[..], &mut rr[..]]),
            ReadStatus::Ok,
            "target {target}"
        );
        // Values match the file at the target. The odd ratio leaves a sub-frame offset,
        // so allow about one frame of slope.
        for (f, s) in l.iter().enumerate() {
            let want = expected(2, 0, target + f as i64, RATE);
            assert!(
                (s - want).abs() < 0.02,
                "target {target} frame {f}: {s} vs {want}"
            );
        }
    }
    assert_eq!(r.underruns(), 0);
}

#[test]
fn rolling_through_a_locate_reports_pending_not_underrun() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_file(dir.path(), 6, 44_100, 4.0, 2);
    let streamer = DiskStreamer::new(StreamerConfig::default());
    let mut r = streamer.open(&path).unwrap();
    wait_ready(std::slice::from_mut(&mut r));
    let mut l = vec![0.0; BLOCK];
    let mut rr = vec![0.0; BLOCK];
    // Locate, and keep the transport rolling at real-time pace while the seek is served.
    let start = Instant::now();
    let period = Duration::from_secs_f64(BLOCK as f64 / f64::from(RATE));
    let mut served = false;
    for cycle in 0..400 {
        let pos = 2 * RATE as i64 + (cycle * BLOCK) as i64;
        let st = r.read(SampleTime(pos), &mut [&mut l[..], &mut rr[..]]);
        if st == ReadStatus::Ok {
            served = true;
            break;
        }
        // Until the seek is served, the gap is reported as Pending, never Underrun.
        assert_eq!(st, ReadStatus::Pending, "cycle {cycle}");
        let deadline = start + period * (cycle as u32 + 1);
        if let Some(d) = deadline.checked_duration_since(Instant::now()) {
            std::thread::sleep(d);
        }
    }
    assert!(served, "the seek was never served while rolling");
    assert_eq!(r.underruns(), 0);
}
