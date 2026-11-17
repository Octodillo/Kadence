mod common;

use common::{estimate_freq, max_abs_diff, test_signal};
use daw_media::{
    AudioData, Decoder, FlacBits, MediaError, PeakCache, WavFormat, decode_file,
    decode_file_within, load_file_within, resample, write_flac, write_wav,
};

fn data_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data")
}

fn roundtrip_wav(format: WavFormat, tolerance: f32) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.wav");
    let src = test_signal(48_000, 10_000, 2);
    write_wav(&path, &src, format).unwrap();
    let back = decode_file(&path).unwrap();
    assert_eq!(back.sample_rate, 48_000);
    assert_eq!(back.num_channels(), 2);
    assert_eq!(back.frames(), src.frames());
    for c in 0..2 {
        let d = max_abs_diff(&src.channels[c], &back.channels[c]);
        assert!(d <= tolerance, "{format:?} ch{c} diff {d}");
    }
}

#[test]
fn wav_float32_is_lossless() {
    roundtrip_wav(WavFormat::Float32, 0.0);
}

#[test]
fn wav_pcm24_roundtrip() {
    roundtrip_wav(WavFormat::Pcm24, 1.0 / 8_388_608.0 + 1e-7);
}

#[test]
fn wav_pcm16_roundtrip() {
    roundtrip_wav(WavFormat::Pcm16, 1.0 / 32_768.0 + 1e-6);
}

fn roundtrip_flac(bits: FlacBits, tolerance: f32) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.flac");
    let src = test_signal(44_100, 30_000, 2);
    write_flac(&path, &src, bits).unwrap();
    let back = decode_file(&path).unwrap();
    assert_eq!(back.sample_rate, 44_100);
    assert_eq!(back.frames(), src.frames());
    for c in 0..2 {
        let d = max_abs_diff(&src.channels[c], &back.channels[c]);
        assert!(d <= tolerance, "{bits:?} ch{c} diff {d}");
    }
}

#[test]
fn flac_24_roundtrip() {
    roundtrip_flac(FlacBits::B24, 1.0 / 8_388_608.0 + 1e-7);
}

#[test]
fn flac_shorter_than_one_block() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("short.flac");
    let src = test_signal(44_100, 1000, 1);
    write_flac(&path, &src, FlacBits::B24).unwrap();
    assert_eq!(decode_file(&path).unwrap().frames(), 1000);
}

#[test]
fn flac_16_roundtrip() {
    roundtrip_flac(FlacBits::B16, 1.0 / 32_768.0 + 1e-6);
}

#[test]
fn mono_and_many_channels_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    for ch in [1, 6] {
        let path = dir.path().join(format!("{ch}.wav"));
        let src = test_signal(48_000, 1000, ch);
        write_wav(&path, &src, WavFormat::Float32).unwrap();
        assert_eq!(decode_file(&path).unwrap(), src);
    }
}

#[test]
fn clipping_is_clamped_for_integer_formats() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clip.wav");
    let src = AudioData {
        sample_rate: 48_000,
        channels: vec![vec![2.0, -2.0, 1.0, -1.0]],
    };
    write_wav(&path, &src, WavFormat::Pcm16).unwrap();
    let back = decode_file(&path).unwrap();
    assert!((back.channels[0][0] - 32767.0 / 32768.0).abs() < 1e-6);
    assert_eq!(back.channels[0][1], -1.0);
}

#[test]
fn decodes_ogg_vorbis_fixture() {
    let d = decode_file(data_dir().join("sine440.ogg")).unwrap();
    assert_eq!(d.sample_rate, 44_100);
    assert_eq!(d.num_channels(), 2);
    assert!(
        (d.frames() as i64 - 22_050).abs() < 1200,
        "{} frames",
        d.frames()
    );
    let f = estimate_freq(&d.channels[0], d.sample_rate);
    assert!((f - 440.0).abs() < 10.0, "freq {f}");
}

#[test]
fn decodes_mp3_fixture() {
    let d = decode_file(data_dir().join("sine440.mp3")).unwrap();
    assert_eq!(d.sample_rate, 44_100);
    assert_eq!(d.num_channels(), 2);
    assert!(
        (d.frames() as i64 - 22_050).abs() < 2400,
        "{} frames",
        d.frames()
    );
    let f = estimate_freq(&d.channels[1], d.sample_rate);
    assert!((f - 440.0).abs() < 10.0, "freq {f}");
}

#[test]
fn seek_is_sample_accurate_for_wav_and_flac() {
    let dir = tempfile::tempdir().unwrap();
    let src = test_signal(48_000, 50_000, 2);
    let wav = dir.path().join("s.wav");
    let flac = dir.path().join("s.flac");
    write_wav(&wav, &src, WavFormat::Float32).unwrap();
    write_flac(&flac, &src, FlacBits::B24).unwrap();
    for path in [&wav, &flac] {
        let mut dec = Decoder::open(path).unwrap();
        for target in [0u64, 1, 4095, 4096, 12_345, 33_333, 49_000] {
            dec.seek(target).unwrap();
            let mut buf = vec![0.0f32; 2 * 500];
            let n = dec.read(&mut buf).unwrap();
            assert_eq!(n, 500.min(50_000 - target as usize));
            for i in 0..n {
                let want = src.channels[0][target as usize + i];
                assert!(
                    (buf[i * 2] - want).abs() < 1e-5,
                    "{} seek {target} frame {i}: {} vs {want}",
                    path.display(),
                    buf[i * 2]
                );
            }
        }
        // Past the end reads nothing.
        dec.seek(60_000).unwrap();
        let mut buf = vec![0.0f32; 64];
        assert_eq!(dec.read(&mut buf).unwrap(), 0);
    }
}

#[test]
fn resample_preserves_pitch_and_length() {
    let src = test_signal(44_100, 44_100, 2);
    let out = resample(&src, 48_000).unwrap();
    assert_eq!(out.sample_rate, 48_000);
    assert!(
        (out.frames() as i64 - 48_000).abs() <= 1,
        "{}",
        out.frames()
    );
    for (c, channel) in out.channels.iter().enumerate().take(2) {
        let want = 220.0 * (c + 1) as f32 + 0.37;
        let f = estimate_freq(&channel[1000..47_000], 48_000);
        assert!((f - want).abs() < 2.0, "ch{c}: {f} vs {want}");
    }
    // The startup delay is trimmed: output lines up with the input to within one frame.
    for t in [5_000usize, 10_000, 20_000] {
        let expected = 0.8 * (std::f32::consts::TAU * 220.37 * t as f32 / 48_000.0).sin();
        assert!((out.channels[0][t] - expected).abs() < 0.03, "frame {t}");
    }
}

#[test]
fn load_file_resamples_to_engine_rate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("r.wav");
    write_wav(&path, &test_signal(96_000, 9600, 1), WavFormat::Float32).unwrap();
    let d = load_file_within(&path, 48_000, u64::MAX).unwrap().unwrap();
    assert_eq!(d.sample_rate, 48_000);
    assert!((d.frames() as i64 - 4800).abs() <= 1);
}

#[test]
fn missing_and_garbage_files_are_errors() {
    let dir = tempfile::tempdir().unwrap();
    assert!(decode_file(dir.path().join("nope.wav")).is_err());
    let junk = dir.path().join("junk.wav");
    std::fs::write(&junk, b"definitely not audio").unwrap();
    assert!(decode_file(&junk).is_err());
}

/// Issue #28: rewriting a file with the same length inside the filesystem's mtime
/// granularity (2 s on exFAT) left the old waveform cache looking current.
#[test]
fn rewriting_a_file_drops_its_peak_cache() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bounce.wav");
    let loud = AudioData {
        sample_rate: 48_000,
        channels: vec![vec![0.9; 48_000]],
    };
    let quiet = AudioData {
        sample_rate: 48_000,
        channels: vec![vec![0.1; 48_000]],
    };
    write_wav(&path, &loud, WavFormat::Pcm16).unwrap();
    let old = PeakCache::load_or_build(&path).unwrap();
    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    write_wav(&path, &quiet, WavFormat::Pcm16).unwrap();
    // Simulate a coarse clock: same length, same mtime.
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    let new = PeakCache::load_or_build(&path).unwrap();
    assert_ne!(new, old, "stale peaks served for the rewritten file");
    assert_eq!(new, PeakCache::build(&path).unwrap());
}

/// A WAV whose header states `rate`, whatever the audio in it is.
fn wav_claiming_rate(dir: &std::path::Path, rate: u32) -> std::path::PathBuf {
    let path = dir.join(format!("rate-{rate}.wav"));
    write_wav(&path, &test_signal(48_000, 100, 1), WavFormat::Pcm16).unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    // The sample rate of the canonical header hound writes.
    assert_eq!(bytes[24..28], 48_000u32.to_le_bytes());
    bytes[24..28].copy_from_slice(&rate.to_le_bytes());
    std::fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn sample_rates_no_real_file_has_are_refused_on_open() {
    let dir = tempfile::tempdir().unwrap();
    // A prime near the top of the range sizes a resampler by the whole rate.
    for rate in [1, 999, 768_001, 4_294_967_291] {
        let path = wav_claiming_rate(dir.path(), rate);
        let err = Decoder::open(&path).err().expect("refused");
        assert!(matches!(err, MediaError::Unsupported(_)), "{rate}: {err}");
        assert!(load_file_within(&path, 48_000, u64::MAX).is_err());
    }
    for rate in [1_000, 768_000] {
        Decoder::open(wav_claiming_rate(dir.path(), rate)).unwrap();
    }
}

#[test]
fn resampling_refuses_rates_outside_the_supported_range() {
    let data = test_signal(48_000, 100, 1);
    assert!(resample(&data, 4_294_967_291).is_err());
    let odd = AudioData {
        sample_rate: 999,
        ..data
    };
    assert!(resample(&odd, 48_000).is_err());
    assert!(daw_media::ChunkResampler::new(4_294_967_291, 48_000, 1).is_err());
}

#[test]
fn limited_decode_keeps_nothing_of_a_file_that_is_too_long() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("second.wav");
    write_wav(&path, &test_signal(48_000, 48_000, 2), WavFormat::Pcm16).unwrap();
    // The limit counts every channel's samples.
    assert!(decode_file_within(&path, 48_000).unwrap().is_none());
    assert!(load_file_within(&path, 44_100, 95_999).unwrap().is_none());
    let whole = decode_file_within(&path, 96_000).unwrap().expect("fits");
    assert_eq!(whole.frames(), 48_000);
    let resampled = load_file_within(&path, 96_000, 96_000)
        .unwrap()
        .expect("fits");
    assert!((resampled.frames() as i64 - 96_000).abs() <= 1);
    // The limit is on the file's own length, not the resampled one.
    assert_eq!(resampled.sample_rate, 96_000);
}

/// A FLAC of `frames` mono frames whose header says it holds only 10.
fn flac_understating_its_length(dir: &std::path::Path, frames: usize) -> std::path::PathBuf {
    let path = dir.join("short-they-said.flac");
    write_flac(&path, &test_signal(48_000, frames, 1), FlacBits::B16).unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    // STREAMINFO follows "fLaC" and a 4-byte block header; the frame count
    // is the low 36 bits of its bytes 13..18.
    assert_eq!(&bytes[..4], b"fLaC");
    bytes[21] &= 0xf0;
    bytes[22..26].copy_from_slice(&10u32.to_be_bytes());
    std::fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn the_decode_limit_holds_against_a_header_that_understates_the_length() {
    let dir = tempfile::tempdir().unwrap();
    let path = flac_understating_its_length(dir.path(), 48_000);
    assert_eq!(Decoder::open(&path).unwrap().info().frames, Some(10));
    assert!(decode_file_within(&path, 47_999).unwrap().is_none());
    let whole = decode_file_within(&path, 48_000).unwrap().expect("fits");
    assert_eq!(whole.frames(), 48_000);
}

/// A minimal AIFF header stating `channels`, with no audio.
fn aiff_claiming_channels(dir: &std::path::Path, channels: u16) -> std::path::PathBuf {
    let path = dir.join(format!("channels-{channels}.aiff"));
    let mut bytes = Vec::new();
    bytes.extend(b"FORM");
    bytes.extend(46u32.to_be_bytes());
    bytes.extend(b"AIFFCOMM");
    bytes.extend(18u32.to_be_bytes());
    bytes.extend(channels.to_be_bytes());
    bytes.extend(0u32.to_be_bytes()); // frames
    // 8 bits a sample: with more, the reader's own frame size overflows.
    bytes.extend(8u16.to_be_bytes());
    // 48 kHz as an 80-bit float.
    bytes.extend([0x40, 0x0e, 0xbb, 0x80, 0, 0, 0, 0, 0, 0]);
    bytes.extend(b"SSND");
    bytes.extend(8u32.to_be_bytes());
    bytes.extend([0; 8]); // offset, block size
    std::fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn channel_counts_no_real_file_has_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let ok = aiff_claiming_channels(dir.path(), 2);
    assert_eq!(Decoder::open(&ok).unwrap().info().channels, 2);
    for channels in [65, 65_535] {
        let path = aiff_claiming_channels(dir.path(), channels);
        let err = Decoder::open(&path).err().expect("refused");
        assert!(
            matches!(err, MediaError::Unsupported(_)),
            "{channels}: {err}"
        );
        assert!(PeakCache::load_or_build(&path).is_err());
    }
    assert!(daw_media::ChunkResampler::new(44_100, 48_000, 65).is_err());
}

#[test]
fn flushing_a_stream_gives_all_of_it_at_rates_that_share_no_factor() {
    // The resampler then works in blocks of a whole second or more.
    for (from, to) in [(767_999, 48_000), (48_000, 767_999), (44_100, 48_000)] {
        let mut rs = daw_media::ChunkResampler::new(from, to, 2).unwrap();
        let input = vec![0.5f32; 2 * from as usize / 10];
        let mut out = Vec::new();
        rs.process(&input, &mut out).unwrap();
        rs.flush(&mut out).unwrap();
        let frames = (out.len() / 2) as i64;
        assert!(
            (frames - i64::from(to) / 10).abs() <= 1,
            "{from} to {to}: {frames}"
        );
    }
}
